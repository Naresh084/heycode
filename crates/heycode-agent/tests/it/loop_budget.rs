//! A09 restart-safe turn-loop budgets and hygiene.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_agent::{
    LOOP_BUDGET_SETTINGS_NAMESPACE, LoopBudgetLayer, LoopBudgetPolicy, LoopBudgetState, LoopClock,
    LoopStopReason, LoopUnknownUsagePolicy, PreStepDecision, StepVerdict, loop_budget_plugin,
    loop_budget_settings_definition,
};
use heycode_core::{Layer, Next, Waterfall};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_session::{Session, SessionEventKind, TurnEndReason};
use tokio_util::sync::CancellationToken;

use super::turn::{World, build_provider_in};

struct Recording {
    inner: FakeProvider,
    sink: Arc<Mutex<Vec<ChatRequest>>>,
}

impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.sink.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}

fn world(scripts: Vec<Vec<StreamChunk>>) -> World {
    build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(Recording {
                inner: FakeProvider::new(scripts),
                sink,
            })
        },
        Arc::new(heycode_agent::AutoApprove),
    )
}

fn tool_step(usage: Option<heycode_core::TokenUsage>, calls: usize) -> Vec<StreamChunk> {
    let mut chunks = (0..calls)
        .map(|index| StreamChunk::ToolCallDelta {
            index: u16::try_from(index).unwrap(),
            id: Some(format!("call_{index}")),
            name: Some("no-such-tool".to_owned()),
            arguments_delta: "{}".to_owned(),
        })
        .collect::<Vec<_>>();
    if let Some(usage) = usage {
        chunks.push(StreamChunk::Usage(usage));
    }
    chunks.push(StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls));
    chunks
}

fn reply(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

fn policy(
    max_steps: u32,
    max_tokens: u64,
    max_time: std::time::Duration,
    max_tools: u32,
) -> LoopBudgetPolicy {
    LoopBudgetPolicy::new(max_steps, max_tokens, max_time, max_tools).unwrap()
}

fn install(world: &World, policy: LoopBudgetPolicy, clock: Arc<dyn LoopClock>) {
    world
        .agent
        .pre_step_seam()
        .push_shared(LoopBudgetLayer::new(world.session.clone(), policy, clock));
}

fn last_turn_end(world: &World) -> TurnEndReason {
    world
        .session
        .lock()
        .unwrap()
        .events()
        .iter()
        .rev()
        .find_map(|event| match event.kind {
            SessionEventKind::TurnEnd { reason, .. } => Some(reason),
            _ => None,
        })
        .unwrap()
}

struct SystemOffsetClock(u64);

impl LoopClock for SystemOffsetClock {
    fn now_ms(&self) -> Option<u64> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|now| u64::try_from(now.as_millis()).ok())
            .and_then(|now| now.checked_add(self.0))
    }
}

#[tokio::test]
async fn max_turn_steps_stop_before_an_extra_request_and_close_durably() {
    let world = world(vec![
        tool_step(
            Some(heycode_core::TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
            }),
            1,
        ),
        reply("never"),
    ]);
    let limit = policy(1, 10_000, std::time::Duration::from_secs(60), 100);
    install(&world, limit, Arc::new(SystemOffsetClock(0)));

    let error = world.agent.send("loop").await.unwrap_err().to_string();

    assert!(error.contains("max_steps_per_turn"), "{error}");
    assert_eq!(world.requests.lock().unwrap().len(), 1);
    assert_eq!(last_turn_end(&world), TurnEndReason::MaxSteps);
}

#[tokio::test]
async fn reported_token_budget_uses_the_existing_durable_max_tokens_reason() {
    let world = world(vec![
        tool_step(
            Some(heycode_core::TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 4,
            }),
            1,
        ),
        reply("never"),
    ]);
    install(
        &world,
        policy(10, 10, std::time::Duration::from_secs(60), 100),
        Arc::new(SystemOffsetClock(0)),
    );

    let error = world.agent.send("loop").await.unwrap_err().to_string();

    assert!(error.contains("max_tokens_per_turn"), "{error}");
    assert_eq!(world.requests.lock().unwrap().len(), 1);
    assert_eq!(last_turn_end(&world), TurnEndReason::MaxTokens);
}

#[tokio::test]
async fn tool_budget_counts_durable_dispatches_not_provider_declarations() {
    let world = world(vec![
        tool_step(
            Some(heycode_core::TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
            }),
            2,
        ),
        reply("never"),
    ]);
    install(
        &world,
        policy(10, 10_000, std::time::Duration::from_secs(60), 2),
        Arc::new(SystemOffsetClock(0)),
    );

    let error = world.agent.send("loop").await.unwrap_err().to_string();

    assert!(error.contains("max_tool_calls_per_turn"), "{error}");
    assert_eq!(world.requests.lock().unwrap().len(), 1);
    assert_eq!(last_turn_end(&world), TurnEndReason::MaxToolCalls);
}

#[tokio::test]
async fn elapsed_budget_can_stop_the_first_step_without_provider_io() {
    let world = world(vec![reply("never")]);
    install(
        &world,
        policy(10, 10_000, std::time::Duration::from_secs(1), 100),
        Arc::new(SystemOffsetClock(10_000)),
    );

    let error = world.agent.send("too late").await.unwrap_err().to_string();

    assert!(error.contains("max_elapsed_per_turn"), "{error}");
    assert!(world.requests.lock().unwrap().is_empty());
    assert_eq!(last_turn_end(&world), TurnEndReason::MaxElapsed);
}

#[tokio::test]
async fn strict_token_budget_stops_when_prior_usage_was_not_reported() {
    let world = world(vec![tool_step(None, 1), reply("never")]);
    install(
        &world,
        policy(10, 10_000, std::time::Duration::from_secs(60), 100),
        Arc::new(SystemOffsetClock(0)),
    );

    let error = world.agent.send("loop").await.unwrap_err().to_string();

    assert!(error.contains("unreported_token_usage"), "{error}");
    assert_eq!(world.requests.lock().unwrap().len(), 1);
    assert_eq!(last_turn_end(&world), TurnEndReason::UnreportedTokenUsage);
}

#[tokio::test]
async fn lower_bound_policy_can_continue_past_unreported_usage() {
    let world = world(vec![tool_step(None, 1), reply("done")]);
    let limit = policy(10, 10_000, std::time::Duration::from_secs(60), 100)
        .with_unknown_usage(LoopUnknownUsagePolicy::AllowLowerBound);
    install(&world, limit, Arc::new(SystemOffsetClock(0)));

    assert_eq!(world.agent.send("loop").await.unwrap().text, "done");
    assert_eq!(world.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn restart_projection_reconstructs_the_same_exhaustion_from_jsonl() {
    let world = world(vec![
        tool_step(
            Some(heycode_core::TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
            }),
            1,
        ),
        reply("never"),
    ]);
    let limit = policy(1, 10_000, std::time::Duration::from_secs(60), 100);
    install(&world, limit, Arc::new(SystemOffsetClock(0)));
    world.agent.send("loop").await.unwrap_err();
    let directory = {
        let session = world.session.lock().unwrap();
        session.flush().unwrap();
        session.path().parent().unwrap().to_path_buf()
    };
    let reopened = Session::open(directory).unwrap();
    let at_ms = reopened
        .events()
        .last()
        .and_then(|event| u64::try_from(event.time_ms).ok())
        .unwrap();
    let state = LoopBudgetState::project(reopened.events(), 1, at_ms).unwrap();

    assert_eq!(state.completed_steps(), 1);
    assert_eq!(state.tool_calls(), 1);
    assert_eq!(
        state.exhaustion(&limit, 2),
        Some(LoopStopReason::MaxStepsPerTurn)
    );
    assert!(matches!(
        reopened.events().last().unwrap().kind,
        SessionEventKind::TurnEnd {
            reason: TurnEndReason::MaxSteps,
            ..
        }
    ));
}

#[test]
fn zero_disables_limits_and_oversized_explicit_timer_is_refused() {
    for result in [
        LoopBudgetPolicy::new(0, 1, std::time::Duration::from_secs(1), 1),
        LoopBudgetPolicy::new(1, 0, std::time::Duration::from_secs(1), 1),
        LoopBudgetPolicy::new(1, 1, std::time::Duration::ZERO, 1),
        LoopBudgetPolicy::new(1, 1, std::time::Duration::from_secs(1), 0),
    ] {
        assert!(result.is_ok());
    }
    assert!(
        LoopBudgetPolicy::new(1, 1, std::time::Duration::from_secs(24 * 60 * 60 + 1), 1).is_err()
    );
}

#[test]
fn default_budget_settings_are_explicit_lower_bound_and_restart_applied() {
    let context = heycode_core::compose(&[heycode_settings::settings_plugin(
        heycode_settings::SettingsDocuments::new(),
    )])
    .unwrap();
    let settings = context
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .register(&context, loop_budget_settings_definition().unwrap())
        .unwrap();
    assert_eq!(
        snapshot.namespace().as_str(),
        LOOP_BUDGET_SETTINGS_NAMESPACE
    );
    assert_eq!(
        snapshot.applies(),
        heycode_settings::SettingsApplies::Restart
    );
    let policy = LoopBudgetPolicy::from_value(snapshot.resolved()).unwrap();
    assert_eq!(policy.max_steps_per_turn(), 0);
    assert_eq!(policy.max_tokens_per_turn(), 0);
    assert_eq!(policy.max_elapsed_ms(), 0);
    assert_eq!(policy.max_tool_calls_per_turn(), 0);
    assert!(policy.is_unlimited());
    assert_eq!(
        policy.unknown_usage(),
        LoopUnknownUsagePolicy::AllowLowerBound
    );
}

struct Tally(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Layer<PreStepDecision> for Tally {
    async fn handle(
        &self,
        input: &mut PreStepDecision,
        mut next: Next<'_, PreStepDecision>,
    ) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        next.run(input).await
    }
}

#[tokio::test]
async fn cancelled_budget_check_delegates_to_the_turn_cancellation_owner() {
    let directory = tempfile::tempdir().unwrap();
    let session = Arc::new(std::sync::Mutex::new(
        Session::create(directory.path()).unwrap(),
    ));
    let mut seam = Waterfall::new();
    seam.push(LoopBudgetLayer::new(
        session,
        policy(1, 1, std::time::Duration::from_secs(1), 1),
        Arc::new(SystemOffsetClock(0)),
    ));
    let tally = Arc::new(AtomicUsize::new(0));
    seam.push(Tally(tally.clone()));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut decision = PreStepDecision {
        turn: 1,
        step: 1,
        cancellation,
        verdict: StepVerdict::Proceed,
    };

    seam.run(&mut decision).await.unwrap();

    assert_eq!(tally.load(Ordering::SeqCst), 1);
    assert!(matches!(decision.verdict, StepVerdict::Proceed));
}

#[test]
fn plugin_declares_the_effect_owned_pre_step_layer() {
    let plugin = loop_budget_plugin(policy(10, 10_000, std::time::Duration::from_secs(60), 100));
    assert_eq!(plugin.name(), "loop-budget");
    assert_eq!(plugin.inject(), &[heycode_agent::SERVICE_AGENT]);
    assert!(plugin.inventory().iter().any(|row| {
        row.kind == heycode_core::ContributionKind::InterceptionLayer
            && row.name == "agent/pre-step:loop-budget"
    }));
}

#[tokio::test]
async fn unlimited_turn_crosses_old_step_token_time_and_tool_caps() {
    let mut scripts = Vec::new();
    for step in 0..66 {
        let mut chunks = tool_step(
            Some(heycode_core::TokenUsage {
                prompt_tokens: 20_000,
                completion_tokens: 20,
            }),
            4,
        );
        for chunk in &mut chunks {
            if let StreamChunk::ToolCallDelta { index, id, .. } = chunk {
                *id = Some(format!("step_{step}_call_{index}"));
            }
        }
        scripts.push(chunks);
    }
    scripts.push(reply("finished past every old default"));
    let world = world(scripts);
    install(
        &world,
        policy(0, 0, std::time::Duration::ZERO, 0),
        Arc::new(SystemOffsetClock(7_200_000)),
    );
    let report = world.agent.send("finish the work").await.unwrap();
    assert_eq!(report.text, "finished past every old default");
    assert_eq!(world.requests.lock().unwrap().len(), 67);
    assert_eq!(last_turn_end(&world), TurnEndReason::Stop);
}
