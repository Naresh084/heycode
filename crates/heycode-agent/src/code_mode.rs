//! Provider-independent script execution through the current agent authority.
use async_trait::async_trait;
use heycode_core::{CoreError, CoreResult, Plugin, ToolSpec, Waterfall};
use heycode_session::{CodeModeChange, SavedScript, Session, SessionEventKind, project_code_mode};
use heycode_tools::{PreToolDecision, Tool, ToolCallInput, ToolCtx, ToolError, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

/// Captured at actual dispatch, so child tools cannot borrow parent permissions.
#[derive(Clone)]
pub(crate) struct ToolExecutionContext {
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) pre_seam: Arc<Waterfall<PreToolDecision>>,
    pub(crate) approval: Arc<dyn crate::ApprovalPolicy>,
    pub(crate) plan: Option<Arc<crate::plan::PlanHandle>>,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) session: Arc<Mutex<Session>>,
    pub(crate) bus: heycode_core::EventBus,
    pub(crate) cancellation: crate::AgentCancellation,
    pub(crate) attachments: Option<Arc<heycode_attachments::AttachmentStore>>,
}
tokio::task_local! { pub(crate) static EXECUTING_TOOLS: ToolExecutionContext; }
tokio::task_local! { pub(crate) static ORIGIN_TOOL_CALL: String; }
impl ToolExecutionContext {
    pub(crate) async fn execute(
        &self,
        name: String,
        args: Value,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Value> {
        let result = self
            .execute_observed(ToolCallInput { name, args }, cancellation, None)
            .await?;
        if let Some(reason) = result.denied_reason {
            anyhow::bail!("tool guard refused: {reason}");
        }
        anyhow::ensure!(
            !result.reported_error,
            "tool reported an execution error: {}",
            result.value
        );
        anyhow::ensure!(
            result.rich_result.is_none(),
            "script/workflow tool result must be plain JSON; call this tool directly for media"
        );
        Ok(result.value)
    }
    /// Carry the exact caller's registry, guards and authority through a spawned job.
    pub(crate) async fn execute_observed(
        &self,
        input: ToolCallInput,
        cancellation: CancellationToken,
        sink: Option<Arc<dyn heycode_exec::ProcessOutputSink>>,
    ) -> anyhow::Result<heycode_tools::ToolOutcome> {
        let mut input = input;
        if let Some(tool) = self.tools.get(&input.name) {
            input.name = tool.spec().name;
        }
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "tool job cancelled before admission"
        );
        // Preserve the same shared Plan authority before prompting. The normal
        // pre-tool guard rechecks if Plan starts while approval is in flight.
        if let Some(reason) = self
            .plan
            .as_ref()
            .and_then(|plan| plan.0.tool_refusal(&input.name, &input.args))
        {
            anyhow::bail!("{reason}");
        }
        // Human input tools use the same admission rule as direct agent calls.
        if !matches!(
            input.name.as_str(),
            "ask_user_question" | "ask_user_question_async" | "exit_plan_mode"
        ) && let heycode_tools::Verdict::Deny { reason } = EXECUTING_TOOLS
            .scope(
                self.clone(),
                self.approval
                    .decide_cancellable(&input, cancellation.clone()),
            )
            .await
        {
            anyhow::bail!("tool approval refused: {reason}");
        }
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "tool job cancelled before dispatch"
        );
        self.execute_preapproved_observed(input, cancellation, sink)
            .await
    }

    pub(crate) async fn execute_preapproved_observed(
        &self,
        input: ToolCallInput,
        cancellation: CancellationToken,
        sink: Option<Arc<dyn heycode_exec::ProcessOutputSink>>,
    ) -> anyhow::Result<heycode_tools::ToolOutcome> {
        let mut input = input;
        if let Some(tool) = self.tools.get(&input.name) {
            input.name = tool.spec().name;
        }
        anyhow::ensure!(
            !cancellation.is_cancelled() && !self.cancellation.is_shutdown(),
            "tool job cancelled before dispatch"
        );
        let cx = ToolCtx {
            cwd: self.cwd.clone(),
            cancellation,
        };
        let owner = self.terminal_owner()?;
        let checkpoint = crate::session_control::prepare_edit(&self.session, &self.cwd, &input)?;
        let result = crate::session_control::QuestionOwner {
            session: self.session.clone(),
            bus: self.bus.clone(),
        }
        .scope(EXECUTING_TOOLS.scope(
            self.clone(),
            heycode_exec::with_terminal_owner(
                owner,
                heycode_tools::execute_tool_observed(&self.tools, &self.pre_seam, input, &cx, sink),
            ),
        ))
        .await;
        if let Some((store, pending)) = checkpoint
            && result
                .as_ref()
                .is_ok_and(|outcome| !outcome.reported_error && outcome.denied_reason.is_none())
            && let Err(error) = store.confirm(pending)
        {
            self.bus.emit(crate::UiEvent::Error {
                message: format!("edit completed, but checkpoint could not be confirmed: {error}"),
            });
        }
        result
    }

    pub(crate) async fn own_job<F: std::future::Future>(
        &self,
        cancellation: &CancellationToken,
        future: F,
    ) -> F::Output {
        let shutdown = self.cancellation.shutdown_token();
        if shutdown.is_cancelled() {
            cancellation.cancel();
        }
        tokio::pin!(future);
        tokio::select! {
            result=&mut future=>result,
            ()=shutdown.cancelled()=>{cancellation.cancel();future.await}
        }
    }

    pub(crate) fn terminal_owner(&self) -> anyhow::Result<heycode_exec::TerminalOwner> {
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        Ok(heycode_exec::TerminalOwner::new(format!(
            "session:{}",
            session.id()
        ))?)
    }

    pub(crate) fn finish_background_result(
        &self,
        outcome: heycode_tools::ToolOutcome,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Value> {
        if let Some(reason) = outcome.denied_reason {
            anyhow::bail!("tool guard refused: {reason}");
        }
        let value = match outcome.rich_result {
            Some(pending) => serde_json::to_value(crate::agent::commit_tool_rich_result(
                self.attachments.clone(),
                pending,
                cancellation,
            )?)?,
            None => outcome.value,
        };
        anyhow::ensure!(
            !outcome.reported_error,
            "tool reported an execution error: {value}"
        );
        Ok(match outcome.untrusted_content {
            Some(boundary) => Value::String(boundary.render_for_model(&value.to_string())),
            None => value,
        })
    }

    pub(crate) fn enqueue_inbox_with_source(
        &self,
        delivery: heycode_session::InboxDelivery,
        text: impl Into<String>,
        source: heycode_session::InboxSource,
    ) -> anyhow::Result<crate::inbox::InboxSubmission> {
        crate::inbox::enqueue_owned_inbox(
            &self.session,
            self.cancellation.is_turn_active(),
            delivery,
            text.into(),
            source,
        )
    }
    pub(crate) fn publish_inbox_submission(&self, submission: crate::inbox::InboxSubmission) {
        crate::inbox::publish_owned_inbox(&self.bus, submission);
    }
    pub(crate) fn submit_inbox_with_source(
        &self,
        delivery: heycode_session::InboxDelivery,
        text: impl Into<String>,
        source: heycode_session::InboxSource,
    ) -> anyhow::Result<()> {
        let submission = self.enqueue_inbox_with_source(delivery, text, source)?;
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .flush()?;
        self.publish_inbox_submission(submission);
        Ok(())
    }

    fn append(&self, change: CodeModeChange) -> anyhow::Result<()> {
        self.session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .append(SessionEventKind::CodeModeChange {
                change: Box::new(change),
            })?;
        Ok(())
    }
}

struct ScriptHost {
    context: ToolExecutionContext,
    run_id: String,
    next: AtomicU32,
    mutation_gate: tokio::sync::RwLock<()>,
    live: Arc<crate::script_controls::LiveScript>,
}
struct CallSettlement {
    context: ToolExecutionContext,
    run_id: String,
    call: u32,
    settled: bool,
}
impl CallSettlement {
    fn finish(&mut self, result: &anyhow::Result<Value>) -> anyhow::Result<()> {
        let (value, error) = match result {
            Ok(value) => (Some(value.clone()), None),
            Err(error) => (None, Some(bounded_error(error))),
        };
        self.context.append(CodeModeChange::CallFinished {
            run_id: self.run_id.clone(),
            call: self.call,
            value,
            error,
        })?;
        self.settled = true;
        Ok(())
    }
}
impl Drop for CallSettlement {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.context.append(CodeModeChange::CallFinished {
                run_id: self.run_id.clone(),
                call: self.call,
                value: None,
                error: Some("interrupted before tool settlement; effects may have occurred".into()),
            });
        }
    }
}
fn bounded_error(error: &anyhow::Error) -> String {
    error.to_string().chars().take(1800).collect()
}
#[async_trait]
impl heycode_code_mode::ToolHost for ScriptHost {
    async fn call(
        &self,
        name: String,
        args: Value,
        cancellation: CancellationToken,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(args.is_object(), "tool arguments must be an object");
        anyhow::ensure!(name != "run_code", "recursive run_code is unavailable");
        let tool = self
            .context
            .tools
            .get(&name)
            .ok_or_else(|| anyhow::anyhow!("tool is no longer registered"))?;
        // Concurrent reads are safe; mutations retain the normal tool scheduler's barrier.
        let read;
        let write;
        if tool.effect() == heycode_tools::ToolEffect::ReadOnly {
            read = Some(self.mutation_gate.read().await);
            write = None;
        } else {
            read = None;
            write = Some(self.mutation_gate.write().await);
        }
        let _leases = (read, write);
        let _call_lease = self.live.enter().await?;
        let call = self.next.fetch_add(1, Ordering::Relaxed);
        self.context.append(CodeModeChange::CallStarted {
            run_id: self.run_id.clone(),
            call,
            name: name.clone(),
            arguments: args.clone(),
        })?;
        let mut settlement = CallSettlement {
            context: self.context.clone(),
            run_id: self.run_id.clone(),
            call,
            settled: false,
        };
        let result = self.context.execute(name, args, cancellation).await;
        settlement.finish(&result)?;
        result
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    #[serde(default = "run_action")]
    action: String,
    source: Option<String>,
    name: Option<String>,
    tools: Option<BTreeSet<String>>,
    run_id: Option<String>,
    #[serde(default)]
    limits: heycode_code_mode::Limits,
}
fn run_action() -> String {
    "run".into()
}
struct RunCode(Arc<crate::script_controls::ScriptControls>);
#[async_trait]
impl Tool for RunCode {
    fn supports_background(&self) -> bool {
        true
    }
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::tool_orchestration())
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {name:"run_code".into(),description:"Run bounded async JavaScript using tools[name](args), text(value), console.log and return. Every call uses current agent permissions. Parallel reads overlap; mutations serialize. No filesystem/network/process globals. Actions: run source, save named script, run_saved by name, list, status by run_id, pause/resume/stop live run_id. Humans can use /scripts while work continues. Pause blocks new tool calls and keeps the VM alive; wall timeout continues. Runs are never automatically replayed; inspect status after interruption before retrying effects.".into(),parameters:json!({"type":"object","properties":{"action":{"type":"string","enum":["run","save","run_saved","list","status","pause","resume","stop"]},"source":{"type":"string"},"name":{"type":"string"},"tools":{"type":"array","items":{"type":"string"},"uniqueItems":true},"run_id":{"type":"string"},"limits":{"type":"object","properties":{"timeout_ms":{"type":"integer","minimum":1,"maximum":600000},"tool_calls":{"type":"integer","minimum":1,"maximum":512},"parallel_calls":{"type":"integer","minimum":1,"maximum":16}},"additionalProperties":true}},"additionalProperties":false})}
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        async {
            let args:RunArgs=serde_json::from_value(args)?;
            let context=EXECUTING_TOOLS.try_with(Clone::clone).map_err(|_|anyhow::anyhow!("run_code requires an active agent tool dispatch"))?;
            let projection={let session=context.session.lock().unwrap_or_else(|e|e.into_inner());project_code_mode(session.events()).map_err(anyhow::Error::msg)?};
            let owner=context.session.lock().unwrap_or_else(|e|e.into_inner()).id().to_string();
            if matches!(args.action.as_str(), "pause"|"resume"|"stop") {
                return self.0.control(&owner,args.run_id.as_deref().ok_or_else(||anyhow::anyhow!("run_id required"))?,&args.action);
            }
            if args.action=="list" {return Ok(json!({"scripts":projection.saved,"runs":projection.runs.keys().collect::<Vec<_>>(),"live":self.0.list(&owner)}));}
            if args.action=="status" {return Ok(serde_json::to_value(projection.runs.get(args.run_id.as_deref().ok_or_else(||anyhow::anyhow!("run_id required"))?).ok_or_else(||anyhow::anyhow!("unknown run_id"))?)?);}
            let script=if args.action=="run_saved" {
                projection.saved.get(args.name.as_deref().ok_or_else(||anyhow::anyhow!("name required"))?).cloned().ok_or_else(||anyhow::anyhow!("saved script not found"))?
            } else {
                anyhow::ensure!(args.action=="run" || args.action=="save","unknown script action");
                SavedScript {name:args.name.unwrap_or_else(||"inline".into()),source:args.source.ok_or_else(||anyhow::anyhow!("source required"))?,tools:args.tools.unwrap_or_default()}
            };
            for name in &script.tools {
                anyhow::ensure!(name!="run_code" && context.tools.get(name).is_some(),"tool is unavailable or recursive: {name}");
            }
            if args.action=="save" {context.append(CodeModeChange::Saved{script:script.clone()})?; return Ok(json!({"saved":script.name}));}
            let run_id=heycode_core::SessionId::generate().to_string();
            let lease=self.0.start(owner,run_id.clone(),cx.cancellation.clone())?;
            context.append(CodeModeChange::Started{run_id:run_id.clone(),script:script.clone()})?;
            let host=Arc::new(ScriptHost{context:context.clone(),run_id:run_id.clone(),next:AtomicU32::new(0),mutation_gate:tokio::sync::RwLock::new(()),live:lease.live.clone()});
            let result=heycode_code_mode::run(&script.source,script.tools,host,args.limits,lease.live.cancellation()).await;
            let (result_value,error)=match &result {Ok(value)=>(Some(serde_json::to_value(value)?),None),Err(error)=>(None,Some(bounded_error(error)))};
            context.append(CodeModeChange::Finished{run_id:run_id.clone(),result:result_value.clone(),error:error.clone()})?;
            // Return the run id even on script failure, so prior effects are inspectable.
            Ok(json!({"run_id":run_id,"ok":result.is_ok(),"result":result_value,"error":error}))
        }.await.map_err(|e:anyhow::Error|ToolError::new(e.to_string()))
    }
}
struct ToolSearch;
#[async_trait]
impl Tool for ToolSearch {
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::tool_orchestration())
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec{name:"tool_search".into(),description:"Search currently available tools and return exact JSON schemas for run_code or direct calls. Searches only this agent's authorized inventory.".into(),parameters:json!({"type":"object","properties":{"query":{"type":"string","maxLength":2048},"limit":{"type":"integer","minimum":1,"maximum":32}},"required":["query"],"additionalProperties":false})}
    }
    fn effect(&self) -> heycode_tools::ToolEffect {
        heycode_tools::ToolEffect::ReadOnly
    }
    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let context = EXECUTING_TOOLS
            .try_with(Clone::clone)
            .map_err(|_| ToolError::new("tool_search requires an active agent tool dispatch"))?;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::new("query required"))?;
        let words = query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(8)
            .clamp(1, 32) as usize;
        let mut rows = context
            .tools
            .specs()
            .into_iter()
            .filter_map(|spec| {
                let haystack = format!("{} {}", spec.name, spec.description).to_lowercase();
                let score = words
                    .iter()
                    .filter(|word| haystack.contains(word.as_str()))
                    .count();
                (words.is_empty() || score > 0).then_some((score, spec))
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
        let total = rows.len();
        let mut bytes = 0usize;
        let mut tools = Vec::new();
        for (_, spec) in rows.into_iter().take(limit) {
            let value = serde_json::to_value(spec).map_err(|e| ToolError::new(e.to_string()))?;
            let size = value.to_string().len();
            if bytes + size > 128 * 1024 {
                break;
            }
            bytes += size;
            tools.push(value);
        }
        Ok(json!({"matches":total,"tools":tools}))
    }
}

/// Install script execution and current-authority tool discovery.
#[must_use]
pub fn code_mode_plugin() -> Box<dyn Plugin> {
    struct CodeModePlugin;
    impl Plugin for CodeModePlugin {
        fn name(&self) -> &'static str {
            "code-mode"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Tool,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }
        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    "run_code",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Tool,
                    "tool_search",
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::Command,
                    "scripts",
                ),
            ]
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_AGENT,
                heycode_tools::SERVICE_TOOLS,
                crate::SERVICE_COMMANDS,
            ]
        }
        fn apply(&self, ctx: &mut heycode_core::Context) -> CoreResult<()> {
            let tools = ctx
                .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
                .ok_or_else(|| CoreError::other("tool registry missing"))?;
            let controls = Arc::new(crate::script_controls::ScriptControls::default());
            let commands = ctx
                .get::<crate::CommandRegistry>(crate::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("command registry missing"))?;
            commands
                .register_effect(
                    ctx,
                    crate::script_controls::command(controls.clone())
                        .map_err(|e| CoreError::other(e.to_string()))?,
                )
                .map_err(|e| CoreError::other(e.to_string()))?;
            let owner = controls.clone();
            ctx.effect(move || owner.shutdown());
            for tool in [
                Arc::new(RunCode(controls)) as Arc<dyn Tool>,
                Arc::new(ToolSearch),
            ] {
                let registration = tools
                    .register_owned(tool)
                    .map_err(|e| CoreError::other(e.to_string()))?;
                ctx.effect(move || drop(registration));
            }
            Ok(())
        }
    }
    Box::new(CodeModePlugin)
}
