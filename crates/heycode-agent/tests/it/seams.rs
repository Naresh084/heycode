//! A02 — the pre-step, request and request-error seams around the turn loop.
//!
//! Each seam is around-middleware over exactly one decision point. These tests
//! pin the three rules that make that true: the seam runs once per decision, a
//! layer that does not short-circuit delegates (GOTCHAS #16), and a layer that
//! does short-circuit has a defined effect on the turn and its durable record.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_agent::{
    Agent, AgentOptions, AutoApprove, CompactionPolicy, PreStepDecision, RequestDecision,
    RequestErrorDecision, RequestErrorStage, RequestVerdict, StepVerdict, UiEvent,
    agent_options_plugin, agent_plugin, approval_plugin, commands_plugin,
};
use heycode_core::{Layer, Next, Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, FinishReason, LlmSelection, Provider, ProviderInfo, StreamChunk,
    llm_plugin, model_catalog_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::{Session, SessionEventKind, TurnEndReason, project_repair, session_plugin};
use heycode_tools::tools_plugin;

// ---------------------------------------------------------------- harness ---

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

struct World {
    agent: Arc<Agent>,
    session: Arc<std::sync::Mutex<Session>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    ui: Arc<Mutex<Vec<UiEvent>>>,
    _dir: tempfile::TempDir,
}

fn world(scripts: Vec<Vec<StreamChunk>>) -> World {
    world_with(scripts, CompactionPolicy::default())
}

fn world_with(scripts: Vec<Vec<StreamChunk>>, compaction: CompactionPolicy) -> World {
    let dir = tempfile::tempdir().unwrap();
    let requests: Arc<Mutex<Vec<ChatRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(scripts),
        sink: requests.clone(),
    });
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.path().to_path_buf()),
        prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                dir.path().to_path_buf(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "test-model".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions {
            compaction,
            max_task_depth: 3,
            cwd: Some(dir.path().to_path_buf()),
            auto_title: false,
        }),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let ui: Arc<Mutex<Vec<UiEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = ui.clone();
    agent
        .ui()
        .on::<UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    World {
        agent,
        session,
        requests,
        ui,
        _dir: dir,
    }
}

impl World {
    fn kinds(&self) -> Vec<&'static str> {
        self.session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .map(|event| event.kind.name())
            .collect()
    }

    fn last_turn_end(&self) -> TurnEndReason {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        session
            .events()
            .iter()
            .rev()
            .find_map(|event| match event.kind {
                SessionEventKind::TurnEnd { reason, .. } => Some(reason),
                _ => None,
            })
            .expect("turn must be closed durably")
    }

    fn ui_errors(&self) -> Vec<String> {
        self.ui
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                UiEvent::Error { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn assert_no_open_records(&self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let repair = project_repair(session.events());
        assert!(
            repair.is_clean(),
            "a handled request failure must close every started record: {:?}",
            repair.open()
        );
    }
}

fn reply(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(FinishReason::Stop),
    ]
}

/// A call to a tool that is not registered. The turn records a tool error and
/// owes another step, which is the cheapest way to drive a two-step turn.
fn unknown_tool_call() -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some("c1".to_owned()),
            name: Some("no-such-tool".to_owned()),
            arguments_delta: "{}".to_owned(),
        },
        StreamChunk::Finish(FinishReason::ToolCalls),
    ]
}

/// Long enough that the ~4-chars-per-token estimate crosses a small window.
fn long_reply(tokens_worth: usize) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta("x".repeat(tokens_worth * 4)),
        StreamChunk::Finish(FinishReason::Stop),
    ]
}

// ----------------------------------------------------------------- layers ---

/// Ordered `(turn, step)` pairs a tally layer observed.
type StepLog = Arc<Mutex<Vec<(u64, u32)>>>;
/// Ordered failures a request-error layer observed.
type FailureLog = Arc<Mutex<Vec<(RequestErrorStage, String)>>>;

/// Records `(turn, step)` for every decision it sees, then delegates.
struct Tally<T> {
    seen: StepLog,
    _marker: std::marker::PhantomData<fn(T)>,
}

impl<T> Tally<T> {
    fn new() -> (Self, StepLog) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                seen: seen.clone(),
                _marker: std::marker::PhantomData,
            },
            seen,
        )
    }
}

#[async_trait::async_trait]
impl Layer<PreStepDecision> for Tally<PreStepDecision> {
    async fn handle(
        &self,
        input: &mut PreStepDecision,
        mut next: Next<'_, PreStepDecision>,
    ) -> anyhow::Result<()> {
        self.seen.lock().unwrap().push((input.turn, input.step));
        next.run(input).await
    }
}

#[async_trait::async_trait]
impl Layer<RequestDecision> for Tally<RequestDecision> {
    async fn handle(
        &self,
        input: &mut RequestDecision,
        mut next: Next<'_, RequestDecision>,
    ) -> anyhow::Result<()> {
        self.seen.lock().unwrap().push((input.turn, input.step));
        next.run(input).await
    }
}

#[async_trait::async_trait]
impl Layer<RequestErrorDecision> for Tally<RequestErrorDecision> {
    async fn handle(
        &self,
        input: &mut RequestErrorDecision,
        mut next: Next<'_, RequestErrorDecision>,
    ) -> anyhow::Result<()> {
        self.seen.lock().unwrap().push((input.turn, input.step));
        next.run(input).await
    }
}

/// Stops the turn on the named step; delegates on every other step.
struct StopOnStep {
    step: u32,
    reason: &'static str,
}

#[async_trait::async_trait]
impl Layer<PreStepDecision> for StopOnStep {
    async fn handle(
        &self,
        input: &mut PreStepDecision,
        mut next: Next<'_, PreStepDecision>,
    ) -> anyhow::Result<()> {
        if input.step == self.step {
            input.verdict = StepVerdict::StopTurn {
                reason: self.reason.to_owned(),
            };
            return Ok(()); // deliberate short-circuit: the step never runs
        }
        next.run(input).await
    }
}

// ---------------------------------------------------------------- pre-step ---

#[tokio::test]
async fn pre_step_seam_runs_exactly_once_per_step() {
    let world = world(vec![unknown_tool_call(), reply("done")]);
    let (tally, seen) = Tally::<PreStepDecision>::new();
    world.agent.pre_step_seam().push_shared(tally);

    world.agent.send("go").await.unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(1, 1), (1, 2)],
        "one pre-step decision per step, in order"
    );
}

#[tokio::test]
async fn pre_step_allow_path_delegates_to_later_layers() {
    let world = world(vec![reply("ok")]);
    world.agent.pre_step_seam().push_shared(StopOnStep {
        step: 99, // never matches: this layer must delegate on every real step
        reason: "unused",
    });
    let (tally, seen) = Tally::<PreStepDecision>::new();
    world.agent.pre_step_seam().push_shared(tally);

    world.agent.send("go").await.unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![(1, 1)],
        "a layer that does not stop the turn must call next"
    );
}

#[tokio::test]
async fn pre_step_stop_writes_no_step_and_closes_the_turn_with_the_reason() {
    let world = world(vec![reply("never dispatched")]);
    world.agent.pre_step_seam().push_shared(StopOnStep {
        step: 1,
        reason: "step budget exhausted",
    });
    let (tally, seen) = Tally::<PreStepDecision>::new();
    world.agent.pre_step_seam().push_shared(tally);

    let error = world.agent.send("go").await.unwrap_err();

    assert!(
        error.to_string().contains("step budget exhausted"),
        "caller receives the layer's reason: {error}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a stopping layer short-circuits: later layers must not run"
    );
    assert!(
        world.requests.lock().unwrap().is_empty(),
        "a stopped step must never reach the provider"
    );
    assert_eq!(
        world.kinds(),
        vec!["user/message", "turn/start", "turn/end"],
        "no step is announced for a step that never ran"
    );
    assert_eq!(world.last_turn_end(), TurnEndReason::Error);
    assert_eq!(world.ui_errors(), vec!["step budget exhausted".to_owned()]);
}

#[tokio::test]
async fn pre_step_stop_on_a_later_step_keeps_the_completed_steps() {
    let world = world(vec![unknown_tool_call(), reply("unreachable")]);
    world.agent.pre_step_seam().push_shared(StopOnStep {
        step: 2,
        reason: "no second step",
    });

    let error = world.agent.send("go").await.unwrap_err();

    assert!(error.to_string().contains("no second step"));
    let kinds = world.kinds();
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "step/start").count(),
        1,
        "only the first step was ever announced"
    );
    assert_eq!(kinds.last().copied(), Some("turn/end"));
    assert_eq!(world.last_turn_end(), TurnEndReason::Error);
    assert_eq!(
        world.requests.lock().unwrap().len(),
        1,
        "the stopped second step dispatched nothing"
    );
}

// ----------------------------------------------------------------- request ---

#[tokio::test]
async fn request_seam_runs_once_per_step_and_sees_the_dispatched_request() {
    let world = world(vec![unknown_tool_call(), reply("done")]);
    let (tally, seen) = Tally::<RequestDecision>::new();
    world.agent.request_seam().push_shared(tally);

    world.agent.send("hello seam").await.unwrap();

    assert_eq!(*seen.lock().unwrap(), vec![(1, 1), (1, 2)]);
}

#[tokio::test]
async fn request_layer_edits_reach_the_provider() {
    struct Inject;
    #[async_trait::async_trait]
    impl Layer<RequestDecision> for Inject {
        async fn handle(
            &self,
            input: &mut RequestDecision,
            mut next: Next<'_, RequestDecision>,
        ) -> anyhow::Result<()> {
            input
                .request
                .messages
                .push(heycode_llm::ChatMessage::system("INJECTED-BY-LAYER"));
            next.run(input).await
        }
    }

    let world = world(vec![reply("ok")]);
    world.agent.request_seam().push_shared(Inject);

    world.agent.send("go").await.unwrap();

    let requests = world.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| message.content == "INJECTED-BY-LAYER"),
        "a delegating layer's edits must survive to dispatch"
    );
}

#[tokio::test]
async fn request_rebuild_short_circuits_and_dispatches_exactly_one_rebuilt_request() {
    /// Rewrites durable history, then declares the built request stale.
    struct RebuildOnce {
        runs: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Layer<RequestDecision> for RebuildOnce {
        async fn handle(
            &self,
            input: &mut RequestDecision,
            mut next: Next<'_, RequestDecision>,
        ) -> anyhow::Result<()> {
            if self.runs.fetch_add(1, Ordering::SeqCst) > 0 {
                return next.run(input).await;
            }
            // Marks the request it was handed, so a dispatched copy of it is
            // distinguishable from the one rebuilt out of the log.
            input
                .request
                .messages
                .push(heycode_llm::ChatMessage::system("STALE-REQUEST"));
            input.verdict = RequestVerdict::Rebuild {
                reason: "history changed".to_owned(),
            };
            Ok(()) // deliberate short-circuit: the built request is stale
        }
    }

    let world = world(vec![reply("ok")]);
    world.agent.request_seam().push_shared(RebuildOnce {
        runs: AtomicUsize::new(0),
    });
    let (tally, seen) = Tally::<RequestDecision>::new();
    world.agent.request_seam().push_shared(tally);

    let report = world.agent.send("go").await.unwrap();

    assert_eq!(report.reason, "stop");
    assert!(
        seen.lock().unwrap().is_empty(),
        "a rebuilding layer short-circuits: later layers must not inspect a stale request"
    );
    let requests = world.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "exactly one request is dispatched for the step: the rebuilt one"
    );
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|message| message.content == "STALE-REQUEST"),
        "the stale request the layer short-circuited must never be dispatched"
    );
}

#[tokio::test]
async fn request_layer_parks_and_is_released_through_the_turn_cancellation() {
    struct Park {
        entered: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl Layer<RequestDecision> for Park {
        async fn handle(
            &self,
            input: &mut RequestDecision,
            mut next: Next<'_, RequestDecision>,
        ) -> anyhow::Result<()> {
            self.entered.notify_one();
            input.cancellation.cancelled().await;
            next.run(input).await
        }
    }

    let world = world(vec![reply("never reached")]);
    let entered = Arc::new(tokio::sync::Notify::new());
    world.agent.request_seam().push_shared(Park {
        entered: entered.clone(),
    });

    let token = world.agent.token();
    let agent = world.agent.clone();
    let task = tokio::spawn(async move { agent.send("go").await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("the request seam must reach the layer");
    token.cancel();

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("a parked seam layer must be released by the turn token")
        .unwrap()
        .unwrap();

    assert_eq!(report.reason, "aborted");
    assert!(world.requests.lock().unwrap().is_empty());
    assert_eq!(world.last_turn_end(), TurnEndReason::Aborted);
}

/// The caller's token is an independent source: it is not a parent of the
/// operation token inside the decision, so only the agent cancelling that
/// operation can release a layer parked on it.
#[tokio::test]
async fn request_layer_parks_and_is_released_through_caller_cancellation() {
    struct Park {
        entered: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl Layer<RequestDecision> for Park {
        async fn handle(
            &self,
            input: &mut RequestDecision,
            mut next: Next<'_, RequestDecision>,
        ) -> anyhow::Result<()> {
            self.entered.notify_one();
            input.cancellation.cancelled().await;
            next.run(input).await
        }
    }

    let world = world(vec![reply("never reached")]);
    let entered = Arc::new(tokio::sync::Notify::new());
    world.agent.request_seam().push_shared(Park {
        entered: entered.clone(),
    });

    let caller = tokio_util::sync::CancellationToken::new();
    let agent = world.agent.clone();
    let token = caller.clone();
    let task = tokio::spawn(async move { agent.send_cancellable("go", token).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("the request seam must reach the layer");
    caller.cancel();

    let report = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("a parked seam layer must be released by the caller token too")
        .unwrap()
        .unwrap();

    assert_eq!(report.reason, "aborted");
    assert!(world.requests.lock().unwrap().is_empty());
}

// ------------------------------------------------------- migrated consumer ---

#[tokio::test]
async fn auto_compaction_is_a_request_seam_layer_that_short_circuits_on_pressure() {
    // Four turns: the keep-window only clears the head of the log on the
    // fourth, which is the turn whose request the pressure check folds.
    let world = world_with(
        vec![
            long_reply(200),
            reply("second"),
            reply("third"),
            reply("AUTO-SUMMARY"),
            reply("fourth"),
        ],
        CompactionPolicy {
            auto: true,
            threshold_ratio: 0.005,
            context_window: 20_000,
        },
    );
    let (tally, seen) = Tally::<RequestDecision>::new();
    world.agent.request_seam().push_shared(tally);

    world.agent.send("one").await.unwrap();
    world.agent.send("two").await.unwrap();
    world.agent.send("three").await.unwrap();
    world.agent.send("four").await.unwrap();

    let folded = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .iter()
        .any(|event| matches!(event.kind, SessionEventKind::CompactionApplied { .. }));
    assert!(folded, "pressure crossing must still fold history");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(1, 1), (2, 1), (3, 1)],
        "the built-in pressure check is an early layer: on turn 4 it folds, \
         short-circuits, and later layers never see the stale request"
    );
    assert_eq!(
        world.requests.lock().unwrap().len(),
        5,
        "turns 1-3, the summarizer, and one rebuilt turn-4 request"
    );
}

#[tokio::test]
async fn disabled_auto_compaction_mounts_no_request_seam_layer() {
    let world = world_with(
        vec![long_reply(200), long_reply(200)],
        CompactionPolicy {
            auto: false,
            threshold_ratio: 0.005,
            context_window: 20_000,
        },
    );
    assert_eq!(
        world.agent.request_seam().len(),
        0,
        "no auto-compaction consumer means no layer on the seam"
    );
    let (tally, seen) = Tally::<RequestDecision>::new();
    world.agent.request_seam().push_shared(tally);

    world.agent.send("a").await.unwrap();
    world.agent.send("b").await.unwrap();

    assert!(
        !world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::CompactionApplied { .. }))
    );
    assert_eq!(*seen.lock().unwrap(), vec![(1, 1), (2, 1)]);
}

#[tokio::test]
async fn enabled_auto_compaction_mounts_exactly_one_request_seam_layer() {
    let world = world_with(
        vec![reply("ok")],
        CompactionPolicy {
            auto: true,
            ..CompactionPolicy::default()
        },
    );
    assert_eq!(
        world.agent.request_seam().len(),
        1,
        "the pressure check lives on the seam, not inline in the step loop"
    );
}

// ----------------------------------------------------------- request error ---

/// Records the stage and message of every failure it observes, then delegates.
struct Observe {
    seen: FailureLog,
}

impl Observe {
    fn new() -> (Self, FailureLog) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        (Self { seen: seen.clone() }, seen)
    }
}

#[async_trait::async_trait]
impl Layer<RequestErrorDecision> for Observe {
    async fn handle(
        &self,
        input: &mut RequestErrorDecision,
        mut next: Next<'_, RequestErrorDecision>,
    ) -> anyhow::Result<()> {
        self.seen
            .lock()
            .unwrap()
            .push((input.stage, input.message.clone()));
        next.run(input).await
    }
}

#[tokio::test]
async fn unknown_provider_reaches_the_request_error_seam_once_at_stage_prepare() {
    let world = world(vec![reply("never")]);
    let (observe, seen) = Observe::new();
    world.agent.request_error_seam().push_shared(observe);
    world.agent.set_provider("not-composed");

    let error = world.agent.send("go").await.unwrap_err();

    assert!(error.to_string().contains("unknown provider"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one failure, one closure, one seam run");
    assert_eq!(seen[0].0, RequestErrorStage::Prepare);
    assert!(seen[0].1.contains("unknown provider"));
    assert_eq!(world.last_turn_end(), TurnEndReason::Error);
    world.assert_no_open_records();
}

#[tokio::test]
async fn request_error_publication_follows_durable_step_and_turn_closure() {
    let world = world(vec![reply("never")]);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    let session = world.session.clone();
    world.agent.ui().on::<UiEvent>(move |event| {
        if matches!(event, UiEvent::Error { .. }) {
            let kinds = session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .events()
                .iter()
                .map(|event| event.kind.name())
                .collect::<Vec<_>>();
            sink.lock().unwrap().push(kinds);
        }
    });
    world.agent.set_provider("not-composed");

    world.agent.send("go").await.unwrap_err();

    assert_eq!(
        observed.lock().unwrap().as_slice(),
        &[vec![
            "user/message",
            "turn/start",
            "step/start",
            "step/end",
            "turn/end",
        ]],
        "failure UI may publish only after the durable scopes are closed"
    );
}

#[tokio::test]
async fn a_failing_provider_stream_reaches_the_seam_at_stage_stream() {
    struct Boom;
    impl Provider for Boom {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "fake".to_owned(),
                default_model: "test-model".into(),
            }
        }
        fn stream(&self, _request: ChatRequest) -> ChunkStream {
            Box::pin(futures::stream::iter([Err(
                heycode_llm::LlmError::Transport("upstream refused".to_owned()),
            )]))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(dir.path().to_path_buf()),
        prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                dir.path().to_path_buf(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "test-model".into(),
            },
            vec![Arc::new(Boom) as Arc<dyn Provider>],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let (observe, seen) = Observe::new();
    agent.request_error_seam().push_shared(observe);

    let error = agent.send("go").await.unwrap_err();

    assert!(error.to_string().contains("upstream refused"));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, RequestErrorStage::Stream);
    let session = session.lock().unwrap_or_else(|error| error.into_inner());
    let repair = project_repair(session.events());
    assert!(
        repair.is_clean(),
        "a provider stream failure must close its step and turn: {:?}",
        repair.open()
    );
}

#[tokio::test]
async fn a_pre_step_stop_closes_through_the_same_request_error_seam() {
    let world = world(vec![reply("never")]);
    let (observe, seen) = Observe::new();
    world.agent.request_error_seam().push_shared(observe);
    world.agent.pre_step_seam().push_shared(StopOnStep {
        step: 1,
        reason: "budget",
    });

    world.agent.send("go").await.unwrap_err();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "every turn failure closes at one place");
    assert_eq!(seen[0].0, RequestErrorStage::PreStep);
    assert_eq!(seen[0].1, "budget");
}

#[tokio::test]
async fn a_request_error_layer_rewrites_the_announced_and_returned_message() {
    struct Rewrite;
    #[async_trait::async_trait]
    impl Layer<RequestErrorDecision> for Rewrite {
        async fn handle(
            &self,
            input: &mut RequestErrorDecision,
            mut next: Next<'_, RequestErrorDecision>,
        ) -> anyhow::Result<()> {
            input.message = format!("rewritten: {}", input.message);
            next.run(input).await
        }
    }

    let world = world(vec![reply("never")]);
    world.agent.request_error_seam().push_shared(Rewrite);
    world.agent.set_provider("not-composed");

    let error = world.agent.send("go").await.unwrap_err();

    assert!(error.to_string().starts_with("rewritten: unknown provider"));
    assert_eq!(world.ui_errors().len(), 1);
    assert!(world.ui_errors()[0].starts_with("rewritten: unknown provider"));
}

#[tokio::test]
async fn a_request_error_layer_that_returns_without_next_finalizes_the_message() {
    struct Finalize;
    #[async_trait::async_trait]
    impl Layer<RequestErrorDecision> for Finalize {
        async fn handle(
            &self,
            input: &mut RequestErrorDecision,
            _next: Next<'_, RequestErrorDecision>,
        ) -> anyhow::Result<()> {
            input.message = "final word".to_owned();
            Ok(()) // deliberate short-circuit: no later layer may revise this
        }
    }
    struct Overwrite {
        ran: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Layer<RequestErrorDecision> for Overwrite {
        async fn handle(
            &self,
            input: &mut RequestErrorDecision,
            mut next: Next<'_, RequestErrorDecision>,
        ) -> anyhow::Result<()> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            input.message = "should never be seen".to_owned();
            next.run(input).await
        }
    }

    let world = world(vec![reply("never")]);
    let ran = Arc::new(AtomicUsize::new(0));
    world.agent.request_error_seam().push_shared(Finalize);
    world
        .agent
        .request_error_seam()
        .push_shared(Overwrite { ran: ran.clone() });
    world.agent.set_provider("not-composed");

    let error = world.agent.send("go").await.unwrap_err();

    assert_eq!(error.to_string(), "final word");
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(world.ui_errors(), vec!["final word".to_owned()]);
    assert_eq!(world.last_turn_end(), TurnEndReason::Error);
}

#[tokio::test]
async fn a_failing_seam_layer_still_closes_the_turn_durably() {
    struct Boom;
    #[async_trait::async_trait]
    impl Layer<PreStepDecision> for Boom {
        async fn handle(
            &self,
            _input: &mut PreStepDecision,
            _next: Next<'_, PreStepDecision>,
        ) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("layer exploded"))
        }
    }

    let world = world(vec![reply("never")]);
    world.agent.pre_step_seam().push_shared(Boom);

    let error = world.agent.send("go").await.unwrap_err();

    assert!(error.to_string().contains("layer exploded"));
    assert_eq!(
        world.kinds(),
        vec!["user/message", "turn/start", "turn/end"],
        "a layer failure must not leave an open turn"
    );
    assert_eq!(world.last_turn_end(), TurnEndReason::Error);
}
