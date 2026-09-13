//! A08 deferred catalog and Code Mode scheduling over N01/A06.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_agent::{
    CodeModeCall, CodeModeSchedule, DeferredToolCatalog, DeferredToolError, DeferredToolProvider,
    DeferredToolProviderId, DeferredToolRequest, DeferredToolSelection,
};
use heycode_core::{NativeToolImplementationKind, NativeToolRoute, ToolSpec};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_session::SessionEventKind;
use heycode_tools::{Tool, ToolCtx, ToolError};
use tokio_util::sync::CancellationToken;

use super::turn::{World, build_provider_in, script_text};

struct SelectingProvider {
    id: DeferredToolProviderId,
    names: Vec<String>,
    seen: Arc<Mutex<Vec<DeferredToolRequest>>>,
}

struct ParkedProvider {
    id: DeferredToolProviderId,
    started: Arc<tokio::sync::Notify>,
    settled: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl DeferredToolProvider for ParkedProvider {
    fn id(&self) -> &DeferredToolProviderId {
        &self.id
    }

    async fn select(
        &self,
        _request: DeferredToolRequest,
        cancellation: CancellationToken,
    ) -> Result<DeferredToolSelection, DeferredToolError> {
        self.started.notify_one();
        cancellation.cancelled().await;
        self.settled.fetch_add(1, Ordering::SeqCst);
        Err(DeferredToolError::Cancelled)
    }
}

#[async_trait::async_trait]
impl DeferredToolProvider for SelectingProvider {
    fn id(&self) -> &DeferredToolProviderId {
        &self.id
    }

    async fn select(
        &self,
        request: DeferredToolRequest,
        cancellation: CancellationToken,
    ) -> Result<DeferredToolSelection, DeferredToolError> {
        if cancellation.is_cancelled() {
            return Err(DeferredToolError::Cancelled);
        }
        self.seen.lock().unwrap().push(request);
        DeferredToolSelection::new(self.names.clone())
    }
}

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

fn spec(index: usize, description_bytes: usize) -> ToolSpec {
    ToolSpec {
        name: format!("catalog_tool_{index:05}"),
        description: format!("tool {index} {}", "x".repeat(description_bytes)),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "value": { "type": "string", "description": "y".repeat(128) }
            },
            "required": ["value"]
        }),
    }
}

#[test]
fn large_catalog_fixture_reduces_request_context_and_schema_work() {
    let specs = (0..5_000).map(|index| spec(index, 512)).collect();
    let catalog = DeferredToolCatalog::new(specs, Vec::new()).unwrap();
    let selection = DeferredToolSelection::new([
        "catalog_tool_00007",
        "catalog_tool_02048",
        "catalog_tool_04999",
    ])
    .unwrap();
    let plan = catalog.apply(&selection).unwrap();
    let metrics = plan.metrics();

    assert_eq!(metrics.catalog_entries(), 5_000);
    assert_eq!(metrics.selected_entries(), 3);
    assert_eq!(plan.tool_specs().len(), 3);
    assert!(
        metrics.full_schema_bytes() > metrics.selected_schema_bytes() * 1_000,
        "the deterministic context proxy must materially improve: {metrics:?}"
    );
    assert!(
        metrics.full_schema_nodes() > metrics.selected_schema_nodes() * 1_000,
        "schema traversal work must shrink with the exposed catalog: {metrics:?}"
    );
}

#[test]
fn selection_is_unique_known_and_preserves_catalog_order() {
    let catalog =
        DeferredToolCatalog::new(vec![spec(2, 0), spec(0, 0), spec(1, 0)], Vec::new()).unwrap();
    let selection =
        DeferredToolSelection::new(["catalog_tool_00001", "catalog_tool_00002"]).unwrap();
    let plan = catalog.apply(&selection).unwrap();
    assert_eq!(
        plan.tool_specs()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["catalog_tool_00002", "catalog_tool_00001"],
        "selection changes membership, never the provider's stable tool order"
    );
    assert_eq!(
        DeferredToolSelection::new(["read", "read"]).unwrap_err(),
        DeferredToolError::DuplicateSelection
    );
    assert_eq!(
        catalog
            .apply(&DeferredToolSelection::new(["missing"]).unwrap())
            .unwrap_err(),
        DeferredToolError::UnknownSelection
    );
}

#[tokio::test]
async fn agent_hook_filters_client_catalog_before_provider_dispatch() {
    let world = world(vec![script_text("done")]);
    let prompt = world
        .ctx
        .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
        .unwrap();
    prompt
        .section_shared("fixture-tool-names", 100, |context| {
            format!("EXPOSED TOOLS: {}", context.tool_names.join(","))
        })
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let registration = world
        .agent
        .install_deferred_tool_provider(Arc::new(SelectingProvider {
            id: DeferredToolProviderId::new("fixture-selector").unwrap(),
            names: vec!["read".to_owned()],
            seen: seen.clone(),
        }))
        .unwrap();

    world.agent.send("read the selected file").await.unwrap();

    let requests = world.requests.lock().unwrap();
    let tools = requests[0].tools.as_ref().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read");
    assert!(
        requests[0].messages[0]
            .content
            .contains("EXPOSED TOOLS: read")
    );
    assert!(!requests[0].messages[0].content.contains("write"));
    let selection_requests = seen.lock().unwrap();
    assert_eq!(selection_requests.len(), 1);
    assert_eq!(selection_requests[0].provider(), "fake");
    assert_eq!(selection_requests[0].model(), "test-model");
    assert_eq!(selection_requests[0].query(), "read the selected file");
    let metrics = world.agent.deferred_tool_metrics().unwrap();
    assert!(metrics.catalog_entries() > metrics.selected_entries());
    drop(registration);
}

#[tokio::test]
async fn dropping_registration_restores_the_complete_catalog_on_the_next_turn() {
    let world = world(vec![script_text("first"), script_text("second")]);
    let registration = world
        .agent
        .install_deferred_tool_provider(Arc::new(SelectingProvider {
            id: DeferredToolProviderId::new("fixture-selector").unwrap(),
            names: vec!["read".to_owned()],
            seen: Arc::new(Mutex::new(Vec::new())),
        }))
        .unwrap();
    world.agent.send("first").await.unwrap();
    drop(registration);
    world.agent.send("second").await.unwrap();

    let requests = world.requests.lock().unwrap();
    assert_eq!(requests[0].tools.as_ref().unwrap().len(), 1);
    assert!(requests[1].tools.as_ref().unwrap().len() > 1);
}

#[tokio::test]
async fn cancellation_owns_and_settles_the_deferred_provider_before_aborting() {
    let world = world(vec![script_text("never dispatched")]);
    let started = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(AtomicUsize::new(0));
    let _registration = world
        .agent
        .install_deferred_tool_provider(Arc::new(ParkedProvider {
            id: DeferredToolProviderId::new("parked-selector").unwrap(),
            started: started.clone(),
            settled: settled.clone(),
        }))
        .unwrap();
    let caller = CancellationToken::new();
    let turn = tokio::spawn({
        let agent = world.agent.clone();
        let caller = caller.clone();
        async move { agent.send_cancellable("cancel selection", caller).await }
    });
    started.notified().await;
    caller.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(2), turn)
        .await
        .expect("cancelled deferred selection must settle")
        .unwrap()
        .unwrap();

    assert_eq!(report.reason, "aborted");
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    assert!(world.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn agent_hook_filters_n01_routes_with_the_same_selection() {
    let world = world(vec![script_text("done")]);
    let native = world
        .ctx
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    native
        .register(
            &world.ctx,
            heycode_native_tools::NativeToolImplementation::new(
                "provider_extra",
                "fake:provider-extra",
                NativeToolImplementationKind::Provider,
                Some("fake".to_owned()),
                100,
            )
            .unwrap(),
        )
        .unwrap();
    let registration = world
        .agent
        .install_deferred_tool_provider(Arc::new(SelectingProvider {
            id: DeferredToolProviderId::new("fixture-selector").unwrap(),
            names: vec!["read".to_owned()],
            seen: Arc::new(Mutex::new(Vec::new())),
        }))
        .unwrap();

    world.agent.send("read only").await.unwrap();

    let metrics = world.agent.deferred_tool_metrics().unwrap();
    assert!(metrics.native_routes() > metrics.selected_native_routes());
    assert_eq!(metrics.selected_native_routes(), 0);
    drop(registration);
}

struct CountingTool(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Tool for CountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_fixture".to_owned(),
            description: "Record one explicit Code Mode invocation".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "value": { "type": "integer" } },
                "required": ["value"]
            }),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(args)
    }
}

#[tokio::test]
async fn code_mode_materializes_provider_calls_but_a06_is_the_only_executor() {
    let selected = DeferredToolSelection::new(["code_fixture"]).unwrap();
    let schedule = CodeModeSchedule::new(
        vec![
            CodeModeCall::new(
                "code_call_1",
                "code_fixture",
                serde_json::json!({ "value": 7 }),
            )
            .unwrap(),
        ],
        &selected,
    )
    .unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    assert_eq!(runs.load(Ordering::SeqCst), 0, "planning executes nothing");
    let world = world(vec![schedule.stream_chunks(), script_text("done")]);
    let tools = world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    tools
        .register_shared(Arc::new(CountingTool(runs.clone())))
        .unwrap();

    world.agent.send("run code mode").await.unwrap();

    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let kinds = world
        .session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match event.kind {
            SessionEventKind::ToolCall { .. } => Some("call"),
            SessionEventKind::ToolResult { .. } => Some("result"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["call", "result"]);
}

#[test]
fn code_mode_cannot_schedule_a_tool_the_deferred_provider_did_not_select() {
    let selected = DeferredToolSelection::new(["read"]).unwrap();
    let error = CodeModeSchedule::new(
        vec![CodeModeCall::new("c1", "write", serde_json::json!({})).unwrap()],
        &selected,
    )
    .unwrap_err();
    assert_eq!(error, DeferredToolError::UnselectedCodeModeCall);
}

#[test]
fn empty_code_mode_schedule_cannot_claim_a_tool_call_finish() {
    let selected = DeferredToolSelection::new(["read"]).unwrap();
    assert_eq!(
        CodeModeSchedule::new(Vec::new(), &selected).unwrap_err(),
        DeferredToolError::EmptyCodeModeSchedule
    );
}

#[test]
fn code_mode_can_materialize_strict_events_without_executing() {
    let selected = DeferredToolSelection::new(["read"]).unwrap();
    let schedule = CodeModeSchedule::new(
        vec![CodeModeCall::new("c1", "read", serde_json::json!({ "path": "x" })).unwrap()],
        &selected,
    )
    .unwrap();
    let events = schedule.inference_events("response_1").unwrap();
    assert!(matches!(
        events.first(),
        Some(heycode_llm::InferenceEvent::ResponseStarted { response_id })
            if response_id == "response_1"
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        heycode_llm::InferenceEvent::ToolCallDelta { name: Some(name), .. } if name == "read"
    )));
    assert!(matches!(
        events.last(),
        Some(heycode_llm::InferenceEvent::Finish(
            heycode_llm::FinishReason::ToolCalls
        ))
    ));
}

#[test]
fn deferred_boundary_has_no_direct_execution_handle() {
    let source = include_str!("../../src/deferred_tools.rs");
    for forbidden in ["execute_tool(", "run_tool(", "ToolRegistry"] {
        assert!(
            !source.contains(forbidden),
            "deferred selection must not gain hidden execution through {forbidden}"
        );
    }
}

#[test]
fn native_route_fixture_uses_the_shared_n01_vocabulary() {
    let route = NativeToolRoute::new(
        "web_search",
        "fixture:web",
        NativeToolImplementationKind::Provider,
        Some("fake".to_owned()),
    )
    .unwrap();
    let catalog = DeferredToolCatalog::new(vec![spec(0, 0)], vec![route]).unwrap();
    assert!(
        catalog
            .entries()
            .iter()
            .any(|entry| { entry.name() == "web_search" && entry.kind().is_provider_native() })
    );
}
