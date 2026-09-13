//! O09 dependency-neutral Agent/subagent lifecycle call-site contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    LifecycleHookDecision, LifecycleHookEvent, LifecycleHookPhase, LifecycleHookPort,
    LifecycleHookReport, LifecycleHookRequest, SubagentCapabilities, SubagentContinuation,
    SubagentError, SubagentProvider, SubagentProviderDescriptor, SubagentRegistry, SubagentRequest,
    SubagentSeed, SubagentStarted,
};
use heycode_llm::{FinishReason, StreamChunk};
use tokio_util::sync::CancellationToken;

struct RecordingHooks {
    decision: LifecycleHookDecision,
    requests: Mutex<Vec<LifecycleHookRequest>>,
    session: Option<Arc<Mutex<heycode_session::Session>>>,
}

#[async_trait]
impl LifecycleHookPort for RecordingHooks {
    async fn run(
        &self,
        request: LifecycleHookRequest,
        _cancellation: CancellationToken,
    ) -> LifecycleHookReport {
        if request.phase() == LifecycleHookPhase::Pre
            && request.event() == LifecycleHookEvent::UserPrompt
            && self.decision == LifecycleHookDecision::Proceed
            && let Some(session) = &self.session
        {
            session
                .lock()
                .unwrap()
                .append(heycode_session::SessionEventKind::HookContribution {
                    contribution: Box::new(
                        heycode_session::HookContributionRecord::new(
                            "fixture-owner",
                            heycode_session::HookContributionPhase::Pre,
                            heycode_session::HookContributionEvent::UserPrompt,
                            heycode_session::HookContributionHandler::Prompt,
                            None,
                            "hook context",
                        )
                        .unwrap(),
                    ),
                })
                .unwrap();
        }
        self.requests.lock().unwrap().push(request);
        match self.decision {
            LifecycleHookDecision::Proceed => LifecycleHookReport::proceed(),
            LifecycleHookDecision::Refuse => LifecycleHookReport::refuse(0),
        }
    }
}

#[tokio::test]
async fn fresh_prompt_hooks_surround_admission_and_model_reads_only_the_durable_event() {
    let mut world = super::turn::build(
        vec![vec![
            StreamChunk::TextDelta("done".to_owned()),
            StreamChunk::Finish(FinishReason::Stop),
        ]],
        Arc::new(heycode_agent::AutoApprove),
    );
    let hooks = Arc::new(RecordingHooks {
        decision: LifecycleHookDecision::Proceed,
        requests: Mutex::new(Vec::new()),
        session: Some(world.session.clone()),
    });
    world
        .agent
        .attach_lifecycle_hooks(&world.ctx, hooks.clone())
        .unwrap();

    world.agent.send("hello").await.unwrap();

    let requests = hooks.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].phase(), LifecycleHookPhase::Pre);
    assert_eq!(requests[1].phase(), LifecycleHookPhase::Post);
    let events = world.session.lock().unwrap().events().to_vec();
    let hook_position = events
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                heycode_session::SessionEventKind::HookContribution { .. }
            )
        })
        .unwrap();
    let user_position = events
        .iter()
        .position(|event| {
            matches!(
                event.kind,
                heycode_session::SessionEventKind::UserMessage { .. }
            )
        })
        .unwrap();
    assert!(hook_position < user_position);
    let provider_request = &world.requests.lock().unwrap()[0];
    let visible = provider_request
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert!(
        visible
            .windows(2)
            .any(|pair| pair == ["hook context", "hello"])
    );

    world.ctx.shutdown();
}

#[tokio::test]
async fn prompt_pre_refusal_writes_no_user_or_turn_event() {
    let mut world = super::turn::build(
        vec![vec![StreamChunk::Finish(FinishReason::Stop)]],
        Arc::new(heycode_agent::AutoApprove),
    );
    world
        .agent
        .attach_lifecycle_hooks(
            &world.ctx,
            Arc::new(RecordingHooks {
                decision: LifecycleHookDecision::Refuse,
                requests: Mutex::new(Vec::new()),
                session: None,
            }),
        )
        .unwrap();

    let error = world.agent.send("blocked").await.unwrap_err();
    assert!(error.to_string().contains("refused by a lifecycle hook"));
    assert!(world.session.lock().unwrap().events().iter().all(|event| {
        !matches!(
            event.kind,
            heycode_session::SessionEventKind::UserMessage { .. }
                | heycode_session::SessionEventKind::TurnStart { .. }
        )
    }));
    assert!(world.requests.lock().unwrap().is_empty());
    world.ctx.shutdown();
}

struct CountingProvider {
    descriptor: SubagentProviderDescriptor,
    starts: AtomicUsize,
}

#[async_trait]
impl SubagentProvider for CountingProvider {
    fn descriptor(&self) -> &SubagentProviderDescriptor {
        &self.descriptor
    }

    async fn start(
        &self,
        _request: SubagentRequest,
        _cancellation: CancellationToken,
    ) -> Result<SubagentStarted, SubagentError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(SubagentStarted {
            id: heycode_agent::SubagentId::new("child-one").unwrap(),
            text: "done".to_owned(),
            handle: None,
        })
    }
}

fn provider() -> Arc<CountingProvider> {
    Arc::new(CountingProvider {
        descriptor: SubagentProviderDescriptor::new(
            "fixture",
            "Fixture",
            SubagentCapabilities {
                fork: heycode_llm::CapabilitySupport::Supported,
                continuation: heycode_llm::CapabilitySupport::Unsupported,
                interrupt: heycode_llm::CapabilitySupport::Unsupported,
            },
        )
        .unwrap(),
        starts: AtomicUsize::new(0),
    })
}

#[tokio::test]
async fn subagent_hooks_run_at_registry_admission_and_refusal_precedes_provider_start() {
    let mut context = heycode_core::Context::new();
    let registry = SubagentRegistry::new();
    let active_provider = provider();
    registry.register(active_provider.clone()).unwrap();
    let hooks = Arc::new(RecordingHooks {
        decision: LifecycleHookDecision::Proceed,
        requests: Mutex::new(Vec::new()),
        session: None,
    });
    registry
        .attach_lifecycle_hooks(&context, hooks.clone())
        .unwrap();
    let authority = registry.root_authority(heycode_agent::SubagentId::new("root").unwrap());
    registry
        .start(
            SubagentRequest::with_authority(
                "child",
                "inspect",
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
                authority,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(active_provider.starts.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.requests.lock().unwrap().len(), 2);

    context.shutdown();
    let refusing_registry = SubagentRegistry::new();
    let refusing_provider = provider();
    refusing_registry
        .register(refusing_provider.clone())
        .unwrap();
    let context = heycode_core::Context::new();
    refusing_registry
        .attach_lifecycle_hooks(
            &context,
            Arc::new(RecordingHooks {
                decision: LifecycleHookDecision::Refuse,
                requests: Mutex::new(Vec::new()),
                session: None,
            }),
        )
        .unwrap();
    let authority =
        refusing_registry.root_authority(heycode_agent::SubagentId::new("root").unwrap());
    let error = refusing_registry
        .start(
            SubagentRequest::with_authority(
                "child",
                "inspect",
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
                authority,
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), heycode_agent::SubagentErrorCode::Refused);
    assert_eq!(refusing_provider.starts.load(Ordering::SeqCst), 0);
}
