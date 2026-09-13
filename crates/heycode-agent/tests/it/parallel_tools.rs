//! A06: a step's tool calls run with overlap and commit in the model's order.
//!
//! These cases drive a fully composed world. `web_search` and `web_fetch` are
//! the two model-callable tools this world lets a test park and release, so
//! they stand in for any read-only tool; `write` stands in for a call that is
//! not parallel-safe. What is asserted is never "the scheduler did something"
//! but what a provider and a human actually observe: the order of the durable
//! `tool/result` records, the tool messages of the NEXT request, and the UI
//! event stream.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use heycode_agent::{AutoApprove, UiEvent};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_session::{Session, SessionEventKind};

use super::turn::{World, build_provider_in, script_text};

/// Bound on every wait here: a scheduler that stalls must fail by name.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// A web provider whose calls park until the test releases them, so a test
/// decides the order in which a batch's calls finish.
struct Parked {
    /// Queries currently parked inside `search`, in arrival order.
    live: Mutex<Vec<String>>,
    /// Queries the test has released.
    released: Mutex<BTreeSet<String>>,
    /// Every query the tool was ever entered with.
    entered: Mutex<Vec<String>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Parked {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Vec::new()),
            released: Mutex::new(BTreeSet::new()),
            entered: Mutex::new(Vec::new()),
            notify: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn entered(&self) -> Vec<String> {
        self.entered.lock().unwrap().clone()
    }

    fn live_count(&self) -> usize {
        self.live.lock().unwrap().len()
    }

    fn release(&self, query: &str) {
        self.released.lock().unwrap().insert(query.to_owned());
        self.notify.notify_waiters();
    }

    /// Yield until `count` searches are parked at the same time.
    async fn wait_for_live(&self, count: usize) {
        loop {
            if self.live_count() >= count {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Yield until the tool has been entered with `query`.
    async fn wait_for_entry(&self, query: &str) {
        loop {
            if self.entered().iter().any(|seen| seen == query) {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Release `query` and yield until that call has actually returned, so a
    /// test that wants a specific completion order gets the order it asked for
    /// rather than whatever the runtime happens to poll first.
    async fn finish(&self, query: &str) {
        self.release(query);
        loop {
            if !self.live.lock().unwrap().iter().any(|live| live == query) {
                return;
            }
            tokio::task::yield_now().await;
        }
    }
}

#[async_trait::async_trait]
impl heycode_web::WebProvider for Parked {
    fn descriptor(&self) -> heycode_web::WebProviderDescriptor {
        heycode_web::WebProviderDescriptor::new("parked-web", true, false).unwrap()
    }

    async fn search(
        &self,
        request: heycode_web::WebSearchRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Vec<heycode_web::WebSearchResult>, heycode_web::WebError> {
        let query = request.query().to_owned();
        self.entered.lock().unwrap().push(query.clone());
        self.live.lock().unwrap().push(query.clone());
        loop {
            if self.released.lock().unwrap().contains(&query) {
                break;
            }
            if cancellation.is_cancelled() {
                break;
            }
            let waiting = self.notify.notified();
            // Re-check after arming so a release that raced the arm is not lost.
            if self.released.lock().unwrap().contains(&query) {
                break;
            }
            tokio::select! {
                () = waiting => {}
                () = cancellation.cancelled() => {}
            }
        }
        self.live.lock().unwrap().retain(|live| live != &query);
        Ok(vec![
            heycode_web::WebSearchResult::new(
                format!("result for {query}"),
                format!("https://example.test/{query}"),
                format!("body for {query}"),
            )
            .unwrap(),
        ])
    }
}

fn search_call(index: u16, id: &str, query: &str) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index,
        id: Some(id.to_owned()),
        name: Some("web_search".to_owned()),
        arguments_delta: serde_json::json!({ "query": query }).to_string(),
    }
}

fn read_call(index: u16, id: &str, path: &str) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index,
        id: Some(id.to_owned()),
        name: Some("read".to_owned()),
        arguments_delta: serde_json::json!({ "path": path }).to_string(),
    }
}

fn write_call(index: u16, id: &str, path: &str, content: &str) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index,
        id: Some(id.to_owned()),
        name: Some("write".to_owned()),
        arguments_delta: serde_json::json!({ "path": path, "content": content }).to_string(),
    }
}

/// Compose a world whose only web provider is `parked`.
fn world_with(
    dir: tempfile::TempDir,
    scripts: Vec<Vec<StreamChunk>>,
    parked: &Arc<Parked>,
) -> World {
    struct Scripted {
        inner: FakeProvider,
        sink: Arc<Mutex<Vec<ChatRequest>>>,
    }
    impl Provider for Scripted {
        fn info(&self) -> ProviderInfo {
            self.inner.info()
        }
        fn stream(&self, request: ChatRequest) -> ChunkStream {
            self.sink.lock().unwrap().push(request.clone());
            self.inner.stream(request)
        }
    }
    let world = build_provider_in(
        dir,
        |sink| {
            Arc::new(Scripted {
                inner: FakeProvider::new(scripts),
                sink,
            })
        },
        Arc::new(AutoApprove),
    );
    let web = world
        .ctx
        .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
        .unwrap();
    web.register(&world.ctx, parked.clone()).unwrap();
    // This scheduler fixture explicitly opts into the portable client tool.
    // Production search is provider-native and does not register this fallback.
    world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .register_shared(Arc::new(heycode_tools::WebSearch::new(web)))
        .unwrap();
    world
}

/// Durable `tool/result` records in log order, as `(call_id, is_error)`.
fn tool_results(session: &Arc<std::sync::Mutex<Session>>) -> Vec<(String, bool)> {
    session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::ToolResult {
                call_id, is_error, ..
            } => Some((call_id.as_str().to_owned(), *is_error)),
            _ => None,
        })
        .collect()
}

/// The `tool/call` and `tool/result` kinds in log order.
fn call_and_result_kinds(session: &Arc<std::sync::Mutex<Session>>) -> Vec<String> {
    session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::ToolCall { call_id, .. } => {
                Some(format!("call:{}", call_id.as_str()))
            }
            SessionEventKind::ToolResult { call_id, .. } => {
                Some(format!("result:{}", call_id.as_str()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn three_read_only_calls_of_one_step_execute_at_the_same_time() {
    let dir = tempfile::tempdir().unwrap();
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![
            vec![
                search_call(0, "s1", "alpha"),
                search_call(1, "s2", "beta"),
                search_call(2, "s3", "gamma"),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );

    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("search three things").await });
    // Nothing is released until all three are inside the tool at once. A
    // one-at-a-time batch can never reach this point, so a scheduler that lost
    // its overlap fails here by timeout rather than by passing quietly.
    tokio::time::timeout(BUDGET, parked.wait_for_live(3))
        .await
        .expect("three read-only calls must be able to execute at the same time");
    for query in ["gamma", "beta", "alpha"] {
        parked.release(query);
    }
    let report = tokio::time::timeout(BUDGET, turn)
        .await
        .expect("the turn must settle inside the test budget")
        .unwrap()
        .unwrap();
    assert_eq!(report.text, "done");
    assert_eq!(
        parked.entered(),
        vec!["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()],
        "calls are admitted in model order"
    );
}

#[tokio::test]
async fn results_reach_the_next_request_in_model_order_whatever_order_they_finish_in() {
    let dir = tempfile::tempdir().unwrap();
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![
            vec![
                search_call(0, "s1", "alpha"),
                search_call(1, "s2", "beta"),
                search_call(2, "s3", "gamma"),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );

    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("search three things").await });
    tokio::time::timeout(BUDGET, parked.wait_for_live(3))
        .await
        .expect("all three calls must be executing before any is released");
    // Finish in the exact reverse of the model's order, one at a time, so the
    // observed completion order really is the reverse and not a coincidence.
    for query in ["gamma", "beta", "alpha"] {
        tokio::time::timeout(BUDGET, parked.finish(query))
            .await
            .expect("each released call must return before the next is released");
    }
    tokio::time::timeout(BUDGET, turn)
        .await
        .expect("the turn must settle inside the test budget")
        .unwrap()
        .unwrap();

    assert_eq!(
        tool_results(&world.session),
        vec![
            ("s1".to_owned(), false),
            ("s2".to_owned(), false),
            ("s3".to_owned(), false),
        ],
        "durable results follow the model's call order, not the completion order"
    );
    assert_eq!(
        call_and_result_kinds(&world.session),
        vec![
            "call:s1",
            "result:s1",
            "call:s2",
            "result:s2",
            "call:s3",
            "result:s3",
        ],
        "the durable batch is the one a serial batch would have written"
    );

    let requests = world.requests.lock().unwrap();
    let tool_ids: Vec<String> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == heycode_llm::Role::Tool)
        .map(|message| message.tool_call_id.clone().unwrap_or_default())
        .collect();
    assert_eq!(tool_ids, vec!["s1", "s2", "s3"]);
    let bodies: Vec<&str> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == heycode_llm::Role::Tool)
        .map(|message| message.content.as_str())
        .collect();
    assert!(bodies[0].contains("alpha"), "{bodies:?}");
    assert!(bodies[1].contains("beta"), "{bodies:?}");
    assert!(bodies[2].contains("gamma"), "{bodies:?}");
}

#[tokio::test]
async fn the_ui_announces_one_call_at_a_time_and_pairs_every_result_with_its_own_call() {
    let dir = tempfile::tempdir().unwrap();
    let present = dir.path().join("present.txt");
    std::fs::write(&present, "hello").unwrap();
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![
            vec![
                search_call(0, "s1", "alpha"),
                read_call(1, "r1", present.to_str().unwrap()),
                search_call(2, "s2", "beta"),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );

    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("mixed batch").await });
    tokio::time::timeout(BUDGET, parked.wait_for_live(2))
        .await
        .expect("both searches must be executing before either is released");
    for query in ["beta", "alpha"] {
        tokio::time::timeout(BUDGET, parked.finish(query))
            .await
            .expect("each released call must return before the next is released");
    }
    tokio::time::timeout(BUDGET, turn)
        .await
        .expect("the turn must settle inside the test budget")
        .unwrap()
        .unwrap();

    let surface: Vec<String> = world
        .ui_log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            UiEvent::ToolStarted { name, .. } => Some(format!("started:{name}")),
            UiEvent::ToolFinished { name, ok, .. } => Some(format!("finished:{name}:{ok}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        surface,
        vec![
            "started:web_search",
            "finished:web_search:true",
            "started:read",
            "finished:read:true",
            "started:web_search",
            "finished:web_search:true",
        ],
        "never more than one open tool card, and always in the model's order"
    );
}

#[tokio::test]
async fn a_failing_call_does_not_suppress_the_results_of_the_calls_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let present = dir.path().join("present.txt");
    std::fs::write(&present, "hello").unwrap();
    let missing = dir.path().join("nope.txt");
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![
            vec![
                search_call(0, "s1", "alpha"),
                read_call(1, "bad", missing.to_str().unwrap()),
                read_call(2, "good", present.to_str().unwrap()),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );

    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("one of these will fail").await });
    tokio::time::timeout(BUDGET, parked.wait_for_live(1))
        .await
        .expect("the search must be executing");
    parked.release("alpha");
    tokio::time::timeout(BUDGET, turn)
        .await
        .expect("the turn must settle inside the test budget")
        .unwrap()
        .unwrap();

    assert_eq!(
        tool_results(&world.session),
        vec![
            ("s1".to_owned(), false),
            ("bad".to_owned(), true),
            ("good".to_owned(), false),
        ],
        "a failure is a result at its own index and its successors still run"
    );
    let requests = world.requests.lock().unwrap();
    let tool_messages: Vec<_> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == heycode_llm::Role::Tool)
        .collect();
    assert_eq!(
        tool_messages.len(),
        3,
        "every declared call must be answered or the next request is unsendable"
    );
    assert_eq!(tool_messages[1].tool_result_is_error, Some(true));
    let session = world.session.lock().unwrap();
    let repair = heycode_session::project_repair(session.events());
    assert!(
        repair.is_clean(),
        "a tool failure is a durable result and must leave no open record: {:?}",
        repair.open()
    );
}

#[tokio::test]
async fn a_call_that_is_not_parallel_safe_waits_for_the_calls_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("written.txt");
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![
            vec![
                search_call(0, "s1", "alpha"),
                write_call(1, "w1", target.to_str().unwrap(), "by the batch"),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );

    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("search then write").await });
    tokio::time::timeout(BUDGET, parked.wait_for_entry("alpha"))
        .await
        .expect("the search must be executing");
    assert!(
        !target.exists(),
        "a write must not start while an earlier call is still running"
    );
    parked.release("alpha");
    tokio::time::timeout(BUDGET, turn)
        .await
        .expect("the turn must settle inside the test budget")
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "by the batch");
    assert_eq!(
        tool_results(&world.session),
        vec![("s1".to_owned(), false), ("w1".to_owned(), false)]
    );
}

#[tokio::test]
async fn cancelling_a_turn_starts_no_further_call_and_still_answers_every_declared_call() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("never-written.txt");
    let parked = Parked::new();
    let world = world_with(
        dir,
        vec![vec![
            search_call(0, "s1", "alpha"),
            // Not parallel-safe: it cannot start until the search commits, so
            // cancelling while the search is parked must leave it unlaunched.
            write_call(1, "w1", target.to_str().unwrap(), "must not appear"),
            search_call(2, "s2", "beta"),
            StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
        ]],
        &parked,
    );

    let token = world.agent.token();
    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("start then cancel").await });
    tokio::time::timeout(BUDGET, parked.wait_for_entry("alpha"))
        .await
        .expect("the search must be executing before the turn is cancelled");
    token.cancel();
    let report = tokio::time::timeout(BUDGET, turn)
        .await
        .expect("a cancelled turn must settle inside the test budget")
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, "aborted");

    assert!(
        !target.exists(),
        "the write must never have been launched: {:?}",
        parked.entered()
    );
    assert_eq!(
        parked.entered(),
        vec!["alpha".to_owned()],
        "the second search must never have been launched either"
    );
    let results = tool_results(&world.session);
    assert_eq!(
        results.len(),
        3,
        "every declared call still carries exactly one result: {results:?}"
    );
    assert_eq!(results[1], ("w1".to_owned(), true));
    assert_eq!(results[2], ("s2".to_owned(), true));
    let bodies: Vec<String> = world
        .session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert!(bodies[1].contains("not run"), "{bodies:?}");
    assert!(bodies[2].contains("not run"), "{bodies:?}");
}

#[tokio::test]
async fn live_execution_events_expose_overlap_and_reverse_completion_before_ordered_commit() {
    use heycode_agent::{ToolExecutionEvent, ToolExecutionPhase};
    let parked = Parked::new();
    let world = world_with(
        tempfile::tempdir().unwrap(),
        vec![
            vec![
                search_call(0, "timeline-a", "alpha"),
                search_call(1, "timeline-b", "beta"),
                StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
            ],
            script_text("done"),
        ],
        &parked,
    );
    let events = Arc::new(Mutex::new(Vec::<ToolExecutionEvent>::new()));
    let sink = events.clone();
    world
        .agent
        .ui()
        .on::<ToolExecutionEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("overlap two tools").await });
    tokio::time::timeout(BUDGET, parked.wait_for_live(2))
        .await
        .unwrap();
    {
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.phase == ToolExecutionPhase::Running)
                .count(),
            2
        );
        assert!(!events.iter().any(|e| matches!(
            e.phase,
            ToolExecutionPhase::Finished { .. } | ToolExecutionPhase::Committed { .. }
        )));
    }
    parked.release("beta");
    tokio::time::timeout(BUDGET, async {
        loop {
            if events.lock().unwrap().iter().any(|e| {
                e.call_id.as_str() == "timeline-b"
                    && matches!(e.phase, ToolExecutionPhase::Finished { .. })
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tool_results(&world.session).is_empty(),
        "beta must remain uncommitted behind alpha"
    );
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e.phase, ToolExecutionPhase::Committed { .. }))
    );
    parked.release("alpha");
    tokio::time::timeout(BUDGET, turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let events = events.lock().unwrap();
    let finished = events
        .iter()
        .filter(|e| matches!(e.phase, ToolExecutionPhase::Finished { .. }))
        .map(|e| e.call_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(finished, ["timeline-b", "timeline-a"]);
    let committed = events
        .iter()
        .filter(|e| matches!(e.phase, ToolExecutionPhase::Committed { .. }))
        .map(|e| e.call_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(committed, ["timeline-a", "timeline-b"]);
}
