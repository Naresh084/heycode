//! The single guarded execution pipeline for tool calls.

use heycode_core::Waterfall;

use crate::registry::ToolRegistry;
use crate::tool::ToolCtx;

/// Service key under which the pre-tool [`Waterfall`] is published.
pub const SEAM_PRE_TOOL: heycode_core::ServiceKey = heycode_core::ServiceKey::new("seam/pre_tool");

/// One model-issued tool call: target name plus raw JSON arguments.
#[derive(Debug, Clone)]
pub struct ToolCallInput {
    /// Tool name to dispatch to.
    pub name: String,
    /// Raw argument object exactly as the model produced it.
    pub args: serde_json::Value,
}

/// What a pre-tool layer decided about a call.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Let the pipeline continue to the next layer (and then the tool).
    Allow,
    /// Stop the pipeline; `reason` becomes the model-visible denial text.
    Deny {
        /// Why the call was refused.
        reason: String,
    },
}

/// The decision flowing through the [`SEAM_PRE_TOOL`] waterfall.
///
/// Layers mutate `verdict` in place. A denying layer must return without
/// calling `next` (deliberate short-circuit); once denied, the verdict stays
/// denied — later layers must not resurrect an Allow.
#[derive(Debug, Clone)]
pub struct PreToolDecision {
    /// The call being decided.
    pub call: ToolCallInput,
    /// Current verdict; starts as [`Verdict::Allow`].
    pub verdict: Verdict,
}

/// Result of one guarded execution.
#[derive(Debug)]
pub struct ToolOutcome {
    /// The tool's return value, or [`serde_json::Value::Null`] when denied.
    pub value: serde_json::Value,
    /// Typed rich result awaiting Agent-owned durable media admission.
    pub rich_result: Option<crate::PendingRichToolResult>,
    /// True when a successful protocol call reported a tool execution error.
    pub reported_error: bool,
    /// Set only when the seam denied the call; carries the refusal reason.
    pub denied_reason: Option<String>,
    /// Typed external-content boundary for a successful result.
    pub untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    /// Exact invocation token retained for ordered post-execution admission.
    pub operation_cancellation: Option<tokio_util::sync::CancellationToken>,
}

/// THE single guarded entry point: run the pre-tool waterfall, then the tool.
///
/// A `Deny` short-circuits — the tool never runs — and surfaces as
/// `denied_reason`. An unknown tool name is a hard error: it signals a broken
/// caller, not a recoverable tool failure. Logging is the agent's job and is
/// deliberately not done here.
///
/// # Errors
/// - any error a waterfall layer returns
/// - the requested tool name is not registered
pub async fn execute_tool(
    registry: &ToolRegistry,
    pre: &Waterfall<PreToolDecision>,
    call: ToolCallInput,
    cx: &ToolCtx,
) -> anyhow::Result<ToolOutcome> {
    execute_tool_observed(registry, pre, call, cx, None).await
}

/// The guarded pipeline with an optional live output observer.
///
/// # Errors
/// Same guard/lookup/tool failures as [`execute_tool`].
pub async fn execute_tool_observed(
    registry: &ToolRegistry,
    pre: &Waterfall<PreToolDecision>,
    call: ToolCallInput,
    cx: &ToolCtx,
    sink: Option<std::sync::Arc<dyn heycode_exec::ProcessOutputSink>>,
) -> anyhow::Result<ToolOutcome> {
    let mut call = call;
    if let Some(tool) = registry.get(&call.name) {
        call.name = tool.spec().name;
    }
    let mut decision = PreToolDecision {
        call,
        verdict: Verdict::Allow,
    };
    pre.run(&mut decision).await?;
    match decision.verdict {
        Verdict::Deny { reason } => Ok(ToolOutcome {
            value: serde_json::Value::Null,
            rich_result: None,
            reported_error: false,
            denied_reason: Some(reason),
            untrusted_content: None,
            operation_cancellation: Some(cx.cancellation.clone()),
        }),
        Verdict::Allow => {
            let tool = registry
                .get(&decision.call.name)
                .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", decision.call.name))?;
            let untrusted_content = tool.untrusted_content();
            let output = match sink {
                Some(sink) => {
                    tool.run_output_observed(decision.call.args, cx, sink)
                        .await?
                }
                None => tool.run_output(decision.call.args, cx).await?,
            };
            let (value, rich_result, reported_error) = output.into_parts();
            Ok(ToolOutcome {
                value,
                rich_result,
                reported_error,
                denied_reason: None,
                untrusted_content,
                operation_cancellation: Some(cx.cancellation.clone()),
            })
        }
    }
}

/// Run a tool directly, bypassing [`SEAM_PRE_TOOL`]. For callers that already
/// resolved policy upstream (the agent applies approval first, then hands the
/// surviving call here) while still going through one shared dispatch point.
///
/// # Errors
/// - the requested tool name is not registered
/// - the tool itself failed
pub async fn run_tool(
    registry: &ToolRegistry,
    call: ToolCallInput,
    cx: &ToolCtx,
) -> anyhow::Result<ToolOutcome> {
    let tool = registry
        .get(&call.name)
        .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", call.name))?;
    let untrusted_content = tool.untrusted_content();
    let (value, rich_result, reported_error) = tool.run_output(call.args, cx).await?.into_parts();
    Ok(ToolOutcome {
        value,
        rich_result,
        reported_error,
        denied_reason: None,
        untrusted_content,
        operation_cancellation: Some(cx.cancellation.clone()),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use heycode_core::{Layer, Next, ToolSpec};

    use crate::tool::{Tool, ToolError};

    struct Fake {
        ran: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for Fake {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fake".to_owned(),
                description: "records that it ran".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn run(
            &self,
            _args: serde_json::Value,
            _cx: &ToolCtx,
        ) -> Result<serde_json::Value, ToolError> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(serde_json::json!({"echo": "done"}))
        }
    }

    struct DenyFake;

    #[async_trait::async_trait]
    impl Layer<PreToolDecision> for DenyFake {
        async fn handle(
            &self,
            input: &mut PreToolDecision,
            _next: Next<'_, PreToolDecision>,
        ) -> anyhow::Result<()> {
            assert_eq!(input.call.name, "fake", "layers see the full call");
            input.verdict = Verdict::Deny {
                reason: "blocked by guard".to_owned(),
            };
            Ok(()) // deliberate short-circuit: never delegates
        }
    }

    fn fixture(ran: Arc<AtomicBool>) -> (ToolRegistry, Waterfall<PreToolDecision>, ToolCtx) {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(Fake { ran })).unwrap();
        let cx = ToolCtx {
            cwd: std::env::temp_dir(),
            ..Default::default()
        };
        (reg, Waterfall::new(), cx)
    }

    fn fake_call() -> ToolCallInput {
        ToolCallInput {
            name: "fake".to_owned(),
            args: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn allow_path_runs_the_tool_and_returns_its_value() {
        let ran = Arc::new(AtomicBool::new(false));
        let (reg, pre, cx) = fixture(ran.clone());
        let out = execute_tool(&reg, &pre, fake_call(), &cx).await.unwrap();
        assert!(out.denied_reason.is_none());
        assert_eq!(out.value, serde_json::json!({"echo": "done"}));
        assert!(ran.load(Ordering::SeqCst), "tool must have executed");
    }

    #[tokio::test]
    async fn deny_short_circuits_and_the_tool_never_runs() {
        let ran = Arc::new(AtomicBool::new(false));
        let (reg, mut pre, cx) = fixture(ran.clone());
        pre.push(DenyFake);
        let out = execute_tool(&reg, &pre, fake_call(), &cx).await.unwrap();
        assert!(
            !ran.load(Ordering::SeqCst),
            "denied calls must not execute the tool"
        );
        assert_eq!(out.value, serde_json::Value::Null);
        assert_eq!(out.denied_reason.as_deref(), Some("blocked by guard"));
    }

    #[tokio::test]
    async fn unknown_tool_is_a_hard_error() {
        let (reg, pre, cx) = fixture(Arc::new(AtomicBool::new(false)));
        let call = ToolCallInput {
            name: "nope".to_owned(),
            args: serde_json::json!({}),
        };
        let err = execute_tool(&reg, &pre, call, &cx).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool: nope"));
    }

    #[tokio::test]
    async fn layer_errors_propagate_verbatim() {
        struct Boom;
        #[async_trait::async_trait]
        impl Layer<PreToolDecision> for Boom {
            async fn handle(
                &self,
                _input: &mut PreToolDecision,
                _next: Next<'_, PreToolDecision>,
            ) -> anyhow::Result<()> {
                Err(anyhow::anyhow!("layer exploded"))
            }
        }
        let (reg, mut pre, cx) = fixture(Arc::new(AtomicBool::new(false)));
        pre.push(Boom);
        let err = execute_tool(&reg, &pre, fake_call(), &cx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("layer exploded"));
    }
}
