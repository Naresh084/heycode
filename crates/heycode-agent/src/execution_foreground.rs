//! Foreground shell ownership and identity-preserving promotion.
use crate::{ExecutionJobService, JobId, JobOutcome, JobSettlement, UiEvent};
use heycode_exec::OutputStream;
use heycode_session::InboxDelivery;
use heycode_tools::{ToolCallInput, ToolOutcome};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Dropping a foreground consumer cancels its owned work unless explicitly promoted.
pub(crate) struct ForegroundLease<'a> {
    pub(crate) service: &'a ExecutionJobService,
    pub(crate) id: JobId,
    pub(crate) promoted: CancellationToken,
}
impl Drop for ForegroundLease<'_> {
    fn drop(&mut self) {
        self.service.end_foreground_wait(&self.id);
        if !self.promoted.is_cancelled() {
            let _requested = self.service.jobs().cancel(&self.id);
        }
    }
}
impl ExecutionJobService {
    pub(crate) async fn execute_foreground_tool(
        &self,
        input: ToolCallInput,
        cancellation: CancellationToken,
    ) -> anyhow::Result<ToolOutcome> {
        self.flush_session()?;
        let output = self.new_output()?;
        let retained = output.clone();
        let agent = self.caller_context();
        let jobs = self.jobs().clone();
        let inline = self.config().inline_bytes;
        let control = crate::execution_jobs::ForegroundControl::default();
        let worker_control = control.clone();
        let label = format!("foreground {}", input.name);
        let shell_output = matches!(input.name.as_str(), "bash" | "powershell");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self
            .jobs()
            .spawn(label, InboxDelivery::Inject, move |id, token| async move {
                let _completion = super::execution_output::OutputCompletion(output.clone());
                // Agent's ordered approval already ran. This still uses the same
                // pre-tool guard seam and the exact process cancellation owner.
                let mut result = agent
                    .own_job(
                        &token,
                        agent.execute_preapproved_observed(
                            input,
                            token.clone(),
                            Some(Arc::new(output.clone())),
                        ),
                    )
                    .await;
                // Serialize promotion against finalization: rich results must
                // reach either the foreground owner or the durable job, once.
                let _settling = worker_control
                    .settling
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if worker_control.promoted.is_cancelled() {
                    result = result.and_then(|outcome| {
                        agent
                            .finish_background_result(outcome, &token)
                            .map(|value| ToolOutcome {
                                value,
                                rich_result: None,
                                reported_error: false,
                                denied_reason: None,
                                untrusted_content: None,
                                operation_cancellation: None,
                            })
                    });
                }
                if output.read(OutputStream::Stdout, 0, 0).total_bytes == 0
                    && output.read(OutputStream::Stderr, 0, 0).total_bytes == 0
                {
                    let text = match &result {
                        Ok(outcome) => outcome
                            .value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| outcome.value.to_string()),
                        Err(error) => error.to_string(),
                    };
                    heycode_exec::ProcessOutputSink::append(
                        &output,
                        OutputStream::Stdout,
                        text.as_bytes(),
                    );
                }
                output.end();
                let outcome = if token.is_cancelled() {
                    JobOutcome::Cancelled
                } else if output.exit_success() == Some(false)
                    || !result
                        .as_ref()
                        .is_ok_and(|r| r.denied_reason.is_none() && !r.reported_error)
                {
                    JobOutcome::Failed
                } else {
                    JobOutcome::Completed
                };
                if shell_output
                    && let Ok(outcome) = &mut result
                    && let Some(text) = outcome.value.as_str().filter(|text| text.len() > inline)
                {
                    let mut start = text.len().saturating_sub(inline.saturating_sub(128));
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    outcome.value = serde_json::Value::String(format!(
                        "[inline output capped; use job_control action=output job_id={id}]\n{}",
                        &text[start..]
                    ));
                }
                let tail = output.tail(OutputStream::Stdout, inline.min(2048));
                let summary = format!(
                    "tool job {id} {}; retained output: job_control action=output\n{}",
                    outcome.name(),
                    tail.text
                );
                if let Err(error) = agent.settle_foreground_job(
                    &jobs,
                    &id,
                    &JobSettlement::bounded_or_failed(outcome, summary),
                ) {
                    result = Err(error);
                }
                let _delivered = sender.send(result);
            })?;
        self.retain_output(id.clone(), retained);
        let promoted = self.register_foreground_wait(&id, control);
        let _lease = ForegroundLease {
            service: self,
            id: id.clone(),
            promoted: promoted.clone(),
        };
        self.principal.bus.emit(UiEvent::Info {
            text: format!(
                "Started foreground tool {id}; task controls can move active work to the background"
            ),
        });
        tokio::select! {
            result=receiver=>result.map_err(|_|anyhow::anyhow!("shell worker ended without a result"))?,
            ()=self.automatic_promotion(&id)=>Ok(ToolOutcome {
                value:serde_json::json!({"job_id":id.as_str(),"background":true,"promoted":true,"automatic":true,"output_tool":"job_control","output_action":"output"}),
                rich_result:None,reported_error:false,denied_reason:None,untrusted_content:None,operation_cancellation:None,
            }),
            ()=promoted.cancelled()=>Ok(ToolOutcome {
                value:serde_json::json!({"job_id":id.as_str(),"background":true,"promoted":true,"output_tool":"job_control","output_action":"output"}),
                rich_result:None,reported_error:false,denied_reason:None,untrusted_content:None,operation_cancellation:None,
            }),
            ()=cancellation.cancelled()=>{
                let _requested=self.jobs().cancel(&id);
                match tokio::time::timeout(std::time::Duration::from_secs(5),self.jobs().wait_for_settlement(&id)).await {
                    Ok(Ok(_))=>anyhow::bail!("shell command cancelled; process tree settled"),
                    _=>anyhow::bail!("shell cancellation requested for {id}; settlement unconfirmed"),
                }
            }
        }
    }
}
