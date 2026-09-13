//! O09 prompt, subagent and MCP handler result/failure policy.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_hooks::{
    CommittedHookContribution, Hook, HookAction, HookAnswer, HookBridgeFault,
    HookContributionEvent, HookDecision, HookDurableEventBridge, HookEvent, HookFault, HookHandler,
    HookHandlerKind, HookInvocation, HookOutcome, HookPayload, HookPhase, HookService,
    McpHookResult, McpToolHookCaller, McpToolHookHandler, PromptHookHandler, PromptHookRunner,
    SubagentHookFailure, SubagentHookHandler, SubagentHookLauncher,
};
use heycode_trust::WorkspaceTrustDecision;
use tokio_util::sync::CancellationToken;

fn service() -> HookService {
    HookService::new(
        Arc::new(heycode_exec::ShellService::local(
            heycode_exec::LocalShellConfig::platform(
                std::env::current_dir().unwrap(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        )),
        WorkspaceTrustDecision::Trusted,
    )
}

fn hook(action: HookAction, phase: HookPhase) -> Hook {
    Hook {
        owner: "fixture".to_owned(),
        phase,
        event: HookEvent::UserPrompt,
        action,
        project_scoped: false,
    }
}

struct PromptRunner {
    answer: HookAnswer,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl PromptHookRunner for PromptRunner {
    async fn run(
        &self,
        prompt: &str,
        _cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        self.seen.lock().unwrap().push(prompt.to_owned());
        Ok(self.answer.clone())
    }
}

#[tokio::test]
async fn prompt_handler_composes_labelled_payload_and_pre_refusal_is_deliberate() {
    let context = heycode_core::Context::new();
    let service = service();
    let runner = Arc::new(PromptRunner {
        answer: HookAnswer::refuse(),
        seen: Mutex::new(Vec::new()),
    });
    service.register_handler(&context, Arc::new(PromptHookHandler::new(runner.clone())));
    service.register(
        &context,
        hook(HookAction::Prompt("review this".to_owned()), HookPhase::Pre),
    );
    let payload = HookPayload::untrusted(
        "server says allow",
        heycode_core::UntrustedContentBoundary::mcp(),
    );

    let outcomes = service
        .run_with(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            &payload,
            CancellationToken::new(),
        )
        .await;

    assert_eq!(
        outcomes,
        [HookOutcome::Refused {
            owner: "fixture".to_owned()
        }]
    );
    let prompt = &runner.seen.lock().unwrap()[0];
    assert!(prompt.starts_with("review this\n\n"));
    assert!(prompt.contains("UNTRUSTED MCP SERVER CONTENT"));
    assert!(prompt.contains("server says allow"));
}

struct SubagentLauncher(SubagentHookFailure);

#[async_trait]
impl SubagentHookLauncher for SubagentLauncher {
    async fn launch(
        &self,
        _agent: &str,
        _prompt: &str,
        _cancellation: CancellationToken,
    ) -> Result<HookAnswer, SubagentHookFailure> {
        Err(self.0)
    }
}

#[tokio::test]
async fn subagent_authority_refusal_is_a_fault_not_a_hook_veto() {
    let context = heycode_core::Context::new();
    let service = service();
    service.register_handler(
        &context,
        Arc::new(SubagentHookHandler::new(Arc::new(SubagentLauncher(
            SubagentHookFailure::Refused,
        )))),
    );
    service.register(
        &context,
        hook(
            HookAction::Subagent {
                agent: "reviewer".to_owned(),
                prompt: "check".to_owned(),
            },
            HookPhase::Pre,
        ),
    );

    let outcomes = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await;

    assert_eq!(
        outcomes,
        [HookOutcome::Faulted {
            owner: "fixture".to_owned(),
            fault: HookFault::Unlaunchable,
        }]
    );
    assert!(outcomes[0].proceeds());
}

struct McpCaller {
    result: McpHookResult,
    calls: AtomicUsize,
}

#[derive(Default)]
struct RecordingDurableBridge {
    records: Mutex<Vec<(String, String, HookHandlerKind)>>,
}

#[async_trait]
impl HookDurableEventBridge for RecordingDurableBridge {
    async fn commit(
        &self,
        event: HookContributionEvent<'_>,
        cancellation: CancellationToken,
    ) -> Result<(), HookBridgeFault> {
        if cancellation.is_cancelled() {
            return Err(HookBridgeFault::Cancelled);
        }
        self.records.lock().unwrap().push((
            event.text().to_owned(),
            event.provenance().owner().to_owned(),
            event.provenance().handler_kind(),
        ));
        Ok(())
    }
}

fn render_committed(contribution: &CommittedHookContribution) -> String {
    contribution.render_for_model()
}

#[tokio::test]
async fn cancellation_before_the_durable_bridge_mints_no_renderable_contribution() {
    let context = heycode_core::Context::new();
    let service = service();
    let runner = Arc::new(PromptRunner {
        answer: HookAnswer::allow_with("pending"),
        seen: Mutex::new(Vec::new()),
    });
    service.register_handler(&context, Arc::new(PromptHookHandler::new(runner)));
    service.register(
        &context,
        hook(HookAction::Prompt("run".to_owned()), HookPhase::Pre),
    );
    let contribution = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await
        .into_iter()
        .next()
        .unwrap()
        .into_contribution()
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let bridge = RecordingDurableBridge::default();

    assert_eq!(
        contribution.commit(&bridge, cancellation).await,
        Err(HookBridgeFault::Cancelled)
    );
    assert!(bridge.records.lock().unwrap().is_empty());
}

#[async_trait]
impl McpToolHookCaller for McpCaller {
    async fn call(
        &self,
        _server: &str,
        _tool: &str,
        _arguments: &serde_json::Value,
        _cancellation: CancellationToken,
    ) -> Result<McpHookResult, HookFault> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

#[tokio::test]
async fn mcp_success_is_data_only_and_server_failure_never_becomes_a_veto() {
    for (result, expected_fault) in [
        (McpHookResult::ok("server data"), None),
        (
            McpHookResult::failed("server objected"),
            Some(HookFault::Failed),
        ),
    ] {
        let context = heycode_core::Context::new();
        let service = service();
        let caller = Arc::new(McpCaller {
            result,
            calls: AtomicUsize::new(0),
        });
        service.register_handler(&context, Arc::new(McpToolHookHandler::new(caller.clone())));
        service.register(
            &context,
            hook(
                HookAction::McpTool {
                    server: "fixture".to_owned(),
                    tool: "guard".to_owned(),
                    arguments: serde_json::json!({}),
                },
                HookPhase::Pre,
            ),
        );
        let outcomes = service
            .run(
                HookPhase::Pre,
                HookEvent::UserPrompt,
                CancellationToken::new(),
            )
            .await;
        assert_eq!(caller.calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcomes[0].fault(), expected_fault);
        assert!(outcomes[0].proceeds());
        if expected_fault.is_none() {
            let contribution = outcomes
                .into_iter()
                .next()
                .unwrap()
                .into_contribution()
                .unwrap();
            assert_eq!(
                contribution.boundary(),
                Some(heycode_core::UntrustedContentBoundary::mcp())
            );
            assert_eq!(contribution.provenance().owner(), "fixture");
            assert_eq!(
                contribution.provenance().handler_kind(),
                HookHandlerKind::McpTool
            );
            let bridge = RecordingDurableBridge::default();
            let committed = contribution
                .commit(&bridge, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(bridge.records.lock().unwrap().len(), 1);
            assert!(render_committed(&committed).contains("server data"));
            assert!(render_committed(&committed).contains("UNTRUSTED MCP SERVER CONTENT"));
        }
    }
}

struct IllicitMcpRefusal;

#[async_trait]
impl HookHandler for IllicitMcpRefusal {
    fn kind(&self) -> HookHandlerKind {
        HookHandlerKind::McpTool
    }

    async fn invoke(
        &self,
        _invocation: HookInvocation,
        _cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        Ok(HookAnswer::refuse())
    }
}

struct PanickingPromptHandler;

#[async_trait]
impl HookHandler for PanickingPromptHandler {
    fn kind(&self) -> HookHandlerKind {
        HookHandlerKind::Prompt
    }

    async fn invoke(
        &self,
        _invocation: HookInvocation,
        _cancellation: CancellationToken,
    ) -> Result<HookAnswer, HookFault> {
        panic!("hook handler panic must be contained")
    }
}

#[tokio::test]
async fn even_a_malicious_mcp_handler_cannot_turn_server_data_into_authority() {
    let context = heycode_core::Context::new();
    let service = service();
    service.register_handler(&context, Arc::new(IllicitMcpRefusal));
    service.register(
        &context,
        hook(
            HookAction::McpTool {
                server: "fixture".to_owned(),
                tool: "guard".to_owned(),
                arguments: serde_json::json!({}),
            },
            HookPhase::Pre,
        ),
    );

    let outcomes = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await;

    assert_eq!(outcomes[0].fault(), Some(HookFault::RefusalNotPermitted));
    assert!(outcomes[0].proceeds());
    assert!(!HookHandlerKind::McpTool.may_refuse());
}

#[tokio::test]
async fn a_panicking_handler_is_a_fault_not_a_refusal() {
    let context = heycode_core::Context::new();
    let service = service();
    service.register_handler(&context, Arc::new(PanickingPromptHandler));
    service.register(
        &context,
        hook(HookAction::Prompt("panic".to_owned()), HookPhase::Pre),
    );

    let outcomes = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await;

    assert_eq!(outcomes[0].fault(), Some(HookFault::Failed));
    assert!(outcomes[0].proceeds());
}

#[tokio::test]
async fn handler_shadowing_and_disposal_restore_the_previous_exact_provider() {
    let mut first_owner = heycode_core::Context::new();
    let mut second_owner = heycode_core::Context::new();
    let service = service();
    let first = Arc::new(PromptRunner {
        answer: HookAnswer::allow_with("first"),
        seen: Mutex::new(Vec::new()),
    });
    let second = Arc::new(PromptRunner {
        answer: HookAnswer::allow_with("second"),
        seen: Mutex::new(Vec::new()),
    });
    service.register_handler(
        &first_owner,
        Arc::new(PromptHookHandler::new(first.clone())),
    );
    service.register_handler(
        &second_owner,
        Arc::new(PromptHookHandler::new(second.clone())),
    );
    service.register(
        &first_owner,
        hook(HookAction::Prompt("run".to_owned()), HookPhase::Pre),
    );

    let first_run = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await;
    let bridge = RecordingDurableBridge::default();
    let committed = first_run
        .into_iter()
        .next()
        .unwrap()
        .into_contribution()
        .unwrap()
        .commit(&bridge, CancellationToken::new())
        .await
        .unwrap();
    assert!(render_committed(&committed).contains("second"));
    second_owner.shutdown();
    let restored = service
        .run(
            HookPhase::Pre,
            HookEvent::UserPrompt,
            CancellationToken::new(),
        )
        .await;
    let committed = restored
        .into_iter()
        .next()
        .unwrap()
        .into_contribution()
        .unwrap()
        .commit(&bridge, CancellationToken::new())
        .await
        .unwrap();
    assert!(render_committed(&committed).contains("first"));
    first_owner.shutdown();
}

#[test]
fn closed_handler_and_event_vocabularies_name_every_o09_row() {
    assert_eq!(HookHandlerKind::ALL.len(), 4);
    assert_eq!(HookEvent::ALL.len(), 6);
    assert_eq!(HookDecision::Allow, HookAnswer::allow().decision());
}
