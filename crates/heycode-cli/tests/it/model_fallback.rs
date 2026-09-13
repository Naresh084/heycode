//! Explicit fallback through the production routing owner and Agent loop.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use heycode_cli::testing::{ComposedTestWorld, RealCompositionHarness};
use heycode_llm::{
    ChatRequest, ChunkStream, FinishReason, LlmError, Provider, ProviderErrorClass,
    ProviderFailure, ProviderFailureOrigin, ProviderInfo, StreamChunk,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
struct FailPrimary {
    seen: Arc<Mutex<Vec<String>>>,
    partial: bool,
    all_fail: bool,
    replay_safe: bool,
}
impl Provider for FailPrimary {
    fn fallback_safety(&self, _request: &ChatRequest) -> heycode_llm::RetrySafety {
        if self.replay_safe {
            heycode_llm::RetrySafety::DefinitiveFailuresOnly
        } else {
            heycode_llm::RetrySafety::Never
        }
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "fake".into(),
            default_model: "fallback".into(),
        }
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.seen.lock().unwrap().push(request.model.clone());
        if request.model == "primary" || self.all_fail {
            let mut chunks = Vec::new();
            if self.partial {
                chunks.push(Ok(StreamChunk::TextDelta("partial".into())));
            }
            chunks.push(Err(LlmError::Provider(
                ProviderFailure::new(ProviderErrorClass::RateLimited, ProviderFailureOrigin::Http)
                    .with_status(429),
            )));
            Box::pin(futures::stream::iter(chunks))
        } else {
            Box::pin(futures::stream::iter(vec![
                Ok(StreamChunk::TextDelta("continued".into())),
                Ok(StreamChunk::Finish(FinishReason::Stop)),
            ]))
        }
    }
}
fn world(partial: bool, all_fail: bool) -> (ComposedTestWorld, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(FailPrimary {
            seen: seen.clone(),
            partial,
            all_fail,
            replay_safe: true,
        }));
    harness.config_mut().llm.model = "primary".into();
    harness.config_mut().ui.auto_title = false;
    (harness.compose().unwrap(), seen)
}
async fn configure(world: &ComposedTestWorld, args: &str) -> anyhow::Result<()> {
    let ctx = world.context();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    ctx.get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap()
        .get("fallback")
        .unwrap()
        .unwrap()
        .execute(&agent, args)
        .await
}
#[tokio::test]
async fn configured_fallback_persists_route_and_continues_same_turn_once() {
    let (world, seen) = world(false, false);
    configure(&world, "fake fallback").await.unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("complete this request").await.unwrap();
    assert_eq!(*seen.lock().unwrap(), ["primary", "fallback"]);
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    assert_eq!(routing.selection().unwrap().model(), "fallback");
    assert_eq!(agent.selection().model, "fallback");
    let session = agent.session().lock().unwrap();
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|e| matches!(
                e.kind,
                heycode_session::SessionEventKind::UserMessage { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|e| matches!(e.kind, heycode_session::SessionEventKind::StepEnd { .. }))
            .count(),
        2
    );
}
#[tokio::test]
async fn disabled_fallback_and_partial_output_leave_route_unchanged() {
    for partial in [false, true] {
        let (world, seen) = world(partial, false);
        if partial {
            configure(&world, "fake fallback").await.unwrap();
        }
        let agent = world
            .context()
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap();
        assert!(agent.send("request").await.is_err());
        assert_eq!(*seen.lock().unwrap(), ["primary"]);
        assert_eq!(agent.selection().model, "primary");
    }
}
#[tokio::test]
async fn failing_fallback_does_not_loop_and_can_be_disabled() {
    let (world, seen) = world(false, true);
    configure(&world, "fake fallback").await.unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(agent.send("request").await.is_err());
    assert_eq!(*seen.lock().unwrap(), ["primary", "fallback"]);
    configure(&world, "off").await.unwrap();
    assert!(agent.send("next request").await.is_err());
    assert_eq!(seen.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn unknown_fallback_route_is_refused_before_configuration() {
    let (world, _) = world(false, false);
    assert!(configure(&world, "missing fallback").await.is_err());
    assert!(configure(&world, "fake unproven-model").await.is_err());
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let session = agent.session().lock().unwrap();
    assert!(
        !session
            .path()
            .parent()
            .unwrap()
            .join("fallback.json")
            .exists()
    );
}

#[tokio::test]
async fn automatic_fallback_preserves_explicit_command_line_model_pin() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(FailPrimary {
            seen: seen.clone(),
            partial: false,
            all_fail: false,
            replay_safe: true,
        }));
    harness
        .config_mut()
        .apply_patch("llm.model=primary")
        .unwrap();
    harness.config_mut().ui.auto_title = false;
    let world = harness.compose().unwrap();
    configure(&world, "fake fallback").await.unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(agent.send("request").await.is_err());
    assert_eq!(*seen.lock().unwrap(), ["primary"]);
    assert_eq!(agent.selection().model, "primary");
    let settings = world
        .context()
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_routing::settings_namespace().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.override_layer().unwrap()["model"], "primary");
    assert_ne!(
        snapshot
            .user()
            .and_then(|v| v.get("model"))
            .and_then(Value::as_str),
        Some("fallback")
    );
}

#[tokio::test]
async fn provider_forbidding_replay_never_falls_back() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(FailPrimary {
            seen: seen.clone(),
            partial: false,
            all_fail: false,
            replay_safe: false,
        }));
    harness.config_mut().llm.model = "primary".into();
    harness.config_mut().ui.auto_title = false;
    let world = harness.compose().unwrap();
    configure(&world, "fake fallback").await.unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert!(agent.send("request").await.is_err());
    assert_eq!(*seen.lock().unwrap(), ["primary"]);
    assert_eq!(agent.selection().model, "primary");
}

#[tokio::test]
async fn successful_fallback_retains_matching_provider_override() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut harness = RealCompositionHarness::new()
        .unwrap()
        .with_provider(Arc::new(FailPrimary {
            seen: seen.clone(),
            partial: false,
            all_fail: false,
            replay_safe: true,
        }));
    harness.config_mut().llm.model = "primary".into();
    harness
        .config_mut()
        .apply_patch("llm.provider=fake")
        .unwrap();
    harness.config_mut().ui.auto_title = false;
    let world = harness.compose().unwrap();
    configure(&world, "fake fallback").await.unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    agent.send("request").await.unwrap();
    assert_eq!(*seen.lock().unwrap(), ["primary", "fallback"]);
    let settings = world
        .context()
        .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings
        .get(&heycode_routing::settings_namespace().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.override_layer().unwrap()["provider"], "fake");
}
