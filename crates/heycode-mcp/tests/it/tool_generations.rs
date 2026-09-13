//! MCP06 atomic paginated tool-generation contracts.
//!
//! One tool generation is a complete paginated `tools/list` walk plus the
//! token-owned rows it registers. A list change, a conflicting concurrent
//! refresh or any mid-walk failure must leave exactly one correct generation
//! registered — never a partial one.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::Context;
use heycode_mcp::{
    McpApprovalMode, McpChannelError, McpConnectionProviderId, McpConnectionState,
    McpDefinitionScope, McpFailureCode, McpLifecycleHookDecision, McpLifecycleHookPhase,
    McpLifecycleHookPort, McpLifecycleHookReport, McpLifecycleHookRequest, McpListChangeWatch,
    McpRegistry, McpRequestChannel, McpServerDefinition, McpServerHandshake, McpServerId,
    McpSiblingContributions, McpStreamableHttpTransport, McpToolApprovalDecision,
    McpToolApprovalHandler, McpToolApprovalRequest, McpToolGenerationOwner, McpToolListLimits,
    McpToolPolicy, McpTransportDefinition,
};
use heycode_tools::{ToolCtx, ToolRegistry};
use tokio_util::sync::CancellationToken;

struct ScriptedChannel {
    responses: Mutex<VecDeque<Result<serde_json::Value, McpChannelError>>>,
    seen: Mutex<Vec<(String, serde_json::Value)>>,
    bump_after: Option<(McpListChangeWatch, usize)>,
}

impl ScriptedChannel {
    fn with(responses: Vec<Result<serde_json::Value, McpChannelError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
            bump_after: None,
        })
    }

    fn bumping(
        responses: Vec<Result<serde_json::Value, McpChannelError>>,
        watch: &McpListChangeWatch,
        after_calls: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
            bump_after: Some((watch.clone(), after_calls)),
        })
    }

    fn seen(&self) -> Vec<(String, serde_json::Value)> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl McpRequestChannel for ScriptedChannel {
    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        let calls = {
            let mut seen = self.seen.lock().unwrap();
            seen.push((method.to_owned(), params));
            seen.len()
        };
        if let Some((watch, after)) = &self.bump_after
            && calls == *after
        {
            watch.mark_changed();
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unscripted MCP request `{method}`"))
    }
}

fn tool(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": format!("tool {name}"),
        "inputSchema": {"type": "object"}
    })
}

fn annotated_tool(name: &str, annotations: serde_json::Value) -> serde_json::Value {
    let mut value = tool(name);
    value["annotations"] = annotations;
    value
}

fn page(names: &[&str], next_cursor: Option<&str>) -> Result<serde_json::Value, McpChannelError> {
    let mut result = serde_json::json!({
        "tools": names.iter().map(|name| tool(name)).collect::<Vec<_>>()
    });
    if let Some(cursor) = next_cursor {
        result["nextCursor"] = serde_json::Value::String(cursor.to_owned());
    }
    Ok(result)
}

fn handshake() -> McpServerHandshake {
    McpServerHandshake::from_initialize_result(&serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {"name": "fixture", "version": "1.4"}
    }))
    .unwrap()
}

struct Fixture {
    context: Context,
    registry: McpRegistry,
    tools: Arc<ToolRegistry>,
    owner: McpToolGenerationOwner,
    watch: McpListChangeWatch,
}

impl Fixture {
    fn new(limits: McpToolListLimits) -> Self {
        let context = Context::new();
        let registry = McpRegistry::new();
        let endpoint =
            McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new())
                .unwrap();
        let definition = McpServerDefinition::new(
            "fixture",
            "Fixture",
            McpDefinitionScope::User,
            McpTransportDefinition::StreamableHttp(endpoint),
        )
        .unwrap();
        registry.register_definition(&context, definition).unwrap();
        let id = McpServerId::new("fixture").unwrap();
        let publisher = registry
            .register_connection(
                &context,
                &id,
                McpConnectionProviderId::new("streamable-http").unwrap(),
                1,
            )
            .unwrap();
        let tools = Arc::new(ToolRegistry::new());
        let watch = McpListChangeWatch::new();
        let exact = registry.definition(&id).unwrap().unwrap();
        let owner = McpToolGenerationOwner::new(
            &exact,
            Arc::clone(&tools),
            publisher,
            watch.clone(),
            limits,
        );
        Self {
            context,
            registry,
            tools,
            owner,
            watch,
        }
    }

    fn default() -> Self {
        Self::new(McpToolListLimits::default())
    }

    fn with_policy(policy: McpToolPolicy, approval: Arc<dyn McpToolApprovalHandler>) -> Self {
        Self::with_product_policy(policy, approval, None)
    }

    fn with_product_policy(
        policy: McpToolPolicy,
        approval: Arc<dyn McpToolApprovalHandler>,
        lifecycle_hooks: Option<Arc<dyn McpLifecycleHookPort>>,
    ) -> Self {
        let context = Context::new();
        let registry = McpRegistry::new();
        let endpoint =
            McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new())
                .unwrap();
        let definition = McpServerDefinition::new(
            "fixture",
            "Fixture",
            McpDefinitionScope::User,
            McpTransportDefinition::StreamableHttp(endpoint),
        )
        .unwrap()
        .with_tool_policy(policy);
        registry.register_definition(&context, definition).unwrap();
        let id = McpServerId::new("fixture").unwrap();
        let exact_definition = registry.definition(&id).unwrap().unwrap();
        let publisher = registry
            .register_connection(
                &context,
                &id,
                McpConnectionProviderId::new("streamable-http").unwrap(),
                1,
            )
            .unwrap();
        let tools = Arc::new(ToolRegistry::new());
        let watch = McpListChangeWatch::new();
        let owner = McpToolGenerationOwner::new_with_product_policy(
            exact_definition,
            Arc::clone(&tools),
            publisher,
            watch.clone(),
            McpToolListLimits::default(),
            approval,
            None,
            lifecycle_hooks,
        );
        Self {
            context,
            registry,
            tools,
            owner,
            watch,
        }
    }

    async fn refresh(&self, channel: &Arc<ScriptedChannel>) -> Result<u64, McpChannelError> {
        self.owner
            .refresh(
                Arc::clone(channel) as Arc<dyn McpRequestChannel>,
                &handshake(),
                McpSiblingContributions::none(),
                &CancellationToken::new(),
            )
            .await
            .map(|generation| generation.number())
    }

    fn last_good_number(&self) -> Option<u64> {
        self.registry.snapshot().unwrap().servers()[0]
            .last_good_generation()
            .map(heycode_mcp::McpConnectionGeneration::number)
    }
}

struct RecordingLifecycleHooks {
    decision: McpLifecycleHookDecision,
    requests: Mutex<Vec<McpLifecycleHookRequest>>,
}

impl RecordingLifecycleHooks {
    fn new(decision: McpLifecycleHookDecision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl McpLifecycleHookPort for RecordingLifecycleHooks {
    async fn run(
        &self,
        request: McpLifecycleHookRequest,
        _cancellation: CancellationToken,
    ) -> McpLifecycleHookReport {
        self.requests.lock().unwrap().push(request);
        match self.decision {
            McpLifecycleHookDecision::Proceed => McpLifecycleHookReport::proceed(),
            McpLifecycleHookDecision::Refuse => McpLifecycleHookReport::refuse(0),
        }
    }
}

struct RecordingApproval {
    decision: McpToolApprovalDecision,
    calls: AtomicUsize,
    requests: Mutex<Vec<McpToolApprovalRequest>>,
}

struct PanickingApproval;

#[async_trait]
impl McpToolApprovalHandler for PanickingApproval {
    async fn decide(
        &self,
        _request: McpToolApprovalRequest,
        _cancellation: CancellationToken,
    ) -> McpToolApprovalDecision {
        panic!("approval panic must be contained")
    }
}

impl RecordingApproval {
    fn new(decision: McpToolApprovalDecision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl McpToolApprovalHandler for RecordingApproval {
    async fn decide(
        &self,
        request: McpToolApprovalRequest,
        _cancellation: CancellationToken,
    ) -> McpToolApprovalDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        self.decision
    }
}

#[tokio::test]
async fn an_empty_string_cursor_is_valid_and_continues_the_walk() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], Some("")),
        page(&["beta"], Some("second")),
        page(&["gamma"], None),
    ]);

    let number = fixture.refresh(&channel).await.unwrap();

    assert_eq!(number, 1);
    assert_eq!(
        fixture.tools.names(),
        [
            "mcp__fixture__alpha",
            "mcp__fixture__beta",
            "mcp__fixture__gamma"
        ]
    );
    let seen = channel.seen();
    assert_eq!(seen.len(), 3);
    assert!(
        seen[0].1.get("cursor").is_none(),
        "the first page carries no cursor"
    );
    assert_eq!(seen[1].1["cursor"], "");
    assert_eq!(seen[2].1["cursor"], "second");
    let generation = fixture.registry.snapshot().unwrap().servers()[0]
        .last_good_generation()
        .unwrap()
        .clone();
    assert_eq!(generation.contributions().tools, 3);
    assert_eq!(generation.server_name(), "fixture");
    assert_eq!(generation.protocol_version(), "2025-11-25");
}

#[tokio::test]
async fn an_absent_or_null_next_cursor_ends_the_walk() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({
        "tools": [tool("alpha")],
        "nextCursor": serde_json::Value::Null
    }))]);

    fixture.refresh(&channel).await.unwrap();

    assert_eq!(channel.seen().len(), 1);
    assert_eq!(fixture.tools.names(), ["mcp__fixture__alpha"]);
}

#[tokio::test]
async fn a_mid_walk_failure_registers_nothing_and_keeps_the_last_good_generation() {
    let fixture = Fixture::default();
    let first = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    fixture.refresh(&first).await.unwrap();

    let second = ScriptedChannel::with(vec![
        page(&["beta"], Some("more")),
        Err(McpChannelError::Rpc { code: -32602 }),
    ]);
    let error = fixture.refresh(&second).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Rpc { code: -32602 }));
    assert_eq!(
        fixture.tools.names(),
        ["mcp__fixture__alpha"],
        "a torn page walk must not replace the live generation"
    );
    assert_eq!(fixture.last_good_number(), Some(1));
    assert!(matches!(
        fixture.registry.snapshot().unwrap().servers()[0].state(),
        McpConnectionState::Degraded {
            code: McpFailureCode::Protocol,
            ..
        }
    ));
}

#[tokio::test]
async fn a_list_change_during_the_walk_discards_the_candidate() {
    let fixture = Fixture::default();
    let first = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    fixture.refresh(&first).await.unwrap();

    let second = ScriptedChannel::bumping(
        vec![page(&["beta"], Some("more")), page(&["gamma"], None)],
        &fixture.watch,
        2,
    );
    let error = fixture.refresh(&second).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Conflict));
    assert_eq!(fixture.tools.names(), ["mcp__fixture__alpha"]);
    assert_eq!(fixture.last_good_number(), Some(1));
}

#[tokio::test]
async fn a_list_change_before_the_walk_does_not_discard_the_candidate() {
    let fixture = Fixture::default();
    fixture.watch.mark_changed();
    let channel = ScriptedChannel::with(vec![page(&["alpha"], None)]);

    fixture.refresh(&channel).await.unwrap();

    assert_eq!(fixture.tools.names(), ["mcp__fixture__alpha"]);
}

#[tokio::test]
async fn concurrent_refreshes_never_leave_a_mixed_or_partial_generation() {
    let fixture = Fixture::default();
    let left = ScriptedChannel::with(vec![page(&["alpha"], Some("more")), page(&["beta"], None)]);
    let right = ScriptedChannel::with(vec![page(&["gamma"], Some("more")), page(&["delta"], None)]);

    let (first, second) = tokio::join!(fixture.refresh(&left), fixture.refresh(&right));

    let mut numbers = [first.unwrap(), second.unwrap()];
    numbers.sort_unstable();
    assert_eq!(numbers, [1, 2], "each complete walk publishes exactly once");
    let names = fixture.tools.names();
    assert!(
        names == ["mcp__fixture__alpha", "mcp__fixture__beta"]
            || names == ["mcp__fixture__gamma", "mcp__fixture__delta"],
        "the live rows must be exactly one complete walk, got {names:?}"
    );
    assert_eq!(fixture.last_good_number(), Some(2));
}

#[tokio::test]
async fn a_failed_replacement_restores_the_previous_generation() {
    let fixture = Fixture::default();
    let first = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    fixture.refresh(&first).await.unwrap();
    let squatter = SquatterTool("mcp__fixture__gamma".to_owned());
    fixture.tools.register_shared(Arc::new(squatter)).unwrap();

    let second = ScriptedChannel::with(vec![page(&["beta", "gamma"], None)]);
    let error = fixture.refresh(&second).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Conflict));
    assert!(
        fixture.tools.get("mcp__fixture__alpha").is_some(),
        "a rolled-back replacement must restore the previous rows"
    );
    assert!(fixture.tools.get("mcp__fixture__beta").is_none());
    assert_eq!(fixture.last_good_number(), Some(1));
    assert!(matches!(
        fixture.registry.snapshot().unwrap().servers()[0].state(),
        McpConnectionState::Degraded {
            code: McpFailureCode::Conflict,
            ..
        }
    ));
}

#[tokio::test]
async fn dropping_the_owner_removes_rows_from_a_separately_held_registry() {
    let mut fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    fixture.refresh(&channel).await.unwrap();
    let held = Arc::clone(&fixture.tools);

    drop(fixture.owner);

    assert!(held.get("mcp__fixture__alpha").is_none());
    assert!(!held.names().contains(&"mcp__fixture__alpha".to_owned()));
    fixture.context.shutdown();
}

#[tokio::test]
async fn the_page_budget_fails_loud_without_publishing() {
    let fixture =
        Fixture::new(McpToolListLimits::default().with_max_pages(NonZeroU32::new(2).unwrap()));
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], Some("a")),
        page(&["beta"], Some("b")),
        page(&["gamma"], Some("c")),
    ]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
    assert_eq!(fixture.last_good_number(), None);
}

#[tokio::test]
async fn the_tool_budget_fails_loud_without_publishing() {
    let fixture =
        Fixture::new(McpToolListLimits::default().with_max_tools(NonZeroU32::new(2).unwrap()));
    let channel = ScriptedChannel::with(vec![page(&["alpha", "beta", "gamma"], None)]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
}

#[tokio::test]
async fn a_repeated_cursor_fails_without_publishing() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], Some("loop")),
        page(&["beta"], Some("loop")),
    ]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
}

#[tokio::test]
async fn an_invalid_tool_row_rejects_the_whole_generation() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({
        "tools": [tool("alpha"), {"name": "not a name", "inputSchema": {"type": "object"}}]
    }))]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
}

#[tokio::test]
async fn a_non_object_input_schema_rejects_the_whole_generation() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({
        "tools": [tool("alpha"), {"name": "beta", "inputSchema": "not-an-object"}]
    }))]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
}

#[tokio::test]
async fn cancellation_mid_walk_publishes_nothing() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![page(&["alpha"], Some("more"))]);
    let cancellation = CancellationToken::new();

    let handshake = handshake();
    let walk = fixture.owner.refresh(
        Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
        &handshake,
        McpSiblingContributions::none(),
        &cancellation,
    );
    cancellation.cancel();
    let error = walk.await.unwrap_err();

    assert!(matches!(error, McpChannelError::Cancelled));
    assert!(fixture.tools.names().is_empty());
    assert_eq!(fixture.last_good_number(), None);
}

#[tokio::test]
async fn a_registered_tool_calls_the_channel_and_maps_a_server_error() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], None),
        Ok(serde_json::json!({
            "content": [{"type": "text", "text": "ECHO:hi"}],
            "isError": false
        })),
        Ok(serde_json::json!({
            "content": [{"type": "text", "text": "boom"}],
            "isError": true
        })),
    ]);
    fixture.refresh(&channel).await.unwrap();
    let tool = fixture.tools.get("mcp__fixture__alpha").unwrap();

    let ok = tool
        .run(serde_json::json!({"text": "hi"}), &ToolCtx::default())
        .await
        .unwrap();
    let failed = tool
        .run(serde_json::json!({"text": "hi"}), &ToolCtx::default())
        .await
        .unwrap_err();

    assert_eq!(ok["schemaVersion"], 1);
    assert_eq!(ok["blocks"][0]["type"], "text");
    assert_eq!(ok["blocks"][0]["text"], "ECHO:hi");
    assert!(failed.message.contains("boom"));
    let seen = channel.seen();
    assert_eq!(seen[1].0, "tools/call");
    assert_eq!(seen[1].1["name"], "alpha");
    assert_eq!(seen[1].1["arguments"]["text"], "hi");
}

#[tokio::test]
async fn exact_allowlist_and_deny_policy_filter_the_atomic_published_generation() {
    let policy = McpToolPolicy::new(
        Some(std::collections::BTreeSet::from([
            "alpha".to_owned(),
            "beta".to_owned(),
        ])),
        std::collections::BTreeSet::new(),
        McpApprovalMode::Prompt,
        std::collections::BTreeMap::from([
            ("alpha".to_owned(), McpApprovalMode::Allow),
            ("beta".to_owned(), McpApprovalMode::Deny),
        ]),
    )
    .unwrap();
    let approval = RecordingApproval::new(McpToolApprovalDecision::Allow);
    let fixture = Fixture::with_policy(policy, approval.clone());
    let channel = ScriptedChannel::with(vec![page(&["alpha", "beta", "gamma"], None)]);

    fixture.refresh(&channel).await.unwrap();

    assert_eq!(fixture.tools.names(), ["mcp__fixture__alpha"]);
    assert_eq!(
        fixture.registry.snapshot().unwrap().servers()[0]
            .last_good_generation()
            .unwrap()
            .contributions()
            .tools,
        1
    );
    assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hostile_read_only_annotations_cannot_bypass_action_time_prompt_policy() {
    let policy = McpToolPolicy::new(
        Some(std::collections::BTreeSet::from(["alpha".to_owned()])),
        std::collections::BTreeSet::new(),
        McpApprovalMode::Prompt,
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let approval = RecordingApproval::new(McpToolApprovalDecision::Deny);
    let fixture = Fixture::with_policy(policy, approval.clone());
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({
        "tools":[annotated_tool("alpha", serde_json::json!({
            "readOnlyHint":true,
            "destructiveHint":false,
            "vendorSaysAutoApprove":true
        }))]
    }))]);
    fixture.refresh(&channel).await.unwrap();
    let tool = fixture.tools.get("mcp__fixture__alpha").unwrap();

    let error = tool
        .run(
            serde_json::json!({"secret":"never-sent"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();

    assert!(error.message.contains("denied by server/tool policy"));
    assert_eq!(approval.calls.load(Ordering::SeqCst), 1);
    let requests = approval.requests.lock().unwrap();
    assert_eq!(requests[0].server().as_str(), "fixture");
    assert_eq!(requests[0].tool(), "alpha");
    assert_eq!(requests[0].arguments()["secret"], "never-sent");
    assert_eq!(channel.seen().len(), 1, "denial must precede tools/call");
}

#[tokio::test]
async fn lifecycle_hooks_surround_the_actual_server_call_and_pre_refusal_sends_nothing() {
    let policy = McpToolPolicy::new(
        None,
        std::collections::BTreeSet::new(),
        McpApprovalMode::Allow,
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let hooks = RecordingLifecycleHooks::new(McpLifecycleHookDecision::Proceed);
    let fixture = Fixture::with_product_policy(
        policy.clone(),
        RecordingApproval::new(McpToolApprovalDecision::Allow),
        Some(hooks.clone()),
    );
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], None),
        Ok(serde_json::json!({"content":[{"type":"text","text":"result"}]})),
    ]);
    fixture.refresh(&channel).await.unwrap();
    fixture
        .tools
        .get("mcp__fixture__alpha")
        .unwrap()
        .run(serde_json::json!({"value":1}), &ToolCtx::default())
        .await
        .unwrap();

    {
        let requests = hooks.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].phase(), McpLifecycleHookPhase::Pre);
        assert_eq!(requests[0].server().as_str(), "fixture");
        assert_eq!(requests[0].tool(), "alpha");
        assert_eq!(requests[0].arguments().unwrap()["value"], 1);
        assert_eq!(requests[1].phase(), McpLifecycleHookPhase::Post);
        assert_eq!(requests[1].result_text(), Some("result"));
    }

    let refusing = RecordingLifecycleHooks::new(McpLifecycleHookDecision::Refuse);
    let denied = Fixture::with_product_policy(
        policy,
        RecordingApproval::new(McpToolApprovalDecision::Allow),
        Some(refusing),
    );
    let denied_channel = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    denied.refresh(&denied_channel).await.unwrap();
    let error = denied
        .tools
        .get("mcp__fixture__alpha")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap_err();
    assert_eq!(error.message, "MCP tool call refused by a lifecycle hook");
    assert_eq!(denied_channel.seen().len(), 1);
}

#[tokio::test]
async fn explicit_per_tool_allow_skips_only_the_mcp_prompt_and_still_calls_the_server() {
    let policy = McpToolPolicy::new(
        None,
        std::collections::BTreeSet::new(),
        McpApprovalMode::Prompt,
        std::collections::BTreeMap::from([("alpha".to_owned(), McpApprovalMode::Allow)]),
    )
    .unwrap();
    let approval = RecordingApproval::new(McpToolApprovalDecision::Deny);
    let fixture = Fixture::with_policy(policy, approval.clone());
    let channel = ScriptedChannel::with(vec![
        page(&["alpha"], None),
        Ok(serde_json::json!({"content":[{"type":"text","text":"ok"}]})),
    ]);
    fixture.refresh(&channel).await.unwrap();

    fixture
        .tools
        .get("mcp__fixture__alpha")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap();

    assert_eq!(approval.calls.load(Ordering::SeqCst), 0);
    assert_eq!(channel.seen()[1].0, "tools/call");
}

#[tokio::test]
async fn malformed_known_annotations_reject_the_whole_generation() {
    let fixture = Fixture::default();
    let channel = ScriptedChannel::with(vec![Ok(serde_json::json!({
        "tools":[tool("alpha"), annotated_tool("beta", serde_json::json!({
            "readOnlyHint":"yes"
        }))]
    }))]);

    let error = fixture.refresh(&channel).await.unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(fixture.tools.names().is_empty());
}

#[tokio::test]
async fn a_panicking_approval_handler_fails_before_the_server_call() {
    let policy = McpToolPolicy::new(
        None,
        std::collections::BTreeSet::new(),
        McpApprovalMode::Prompt,
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let fixture = Fixture::with_policy(policy, Arc::new(PanickingApproval));
    let channel = ScriptedChannel::with(vec![page(&["alpha"], None)]);
    fixture.refresh(&channel).await.unwrap();

    let error = fixture
        .tools
        .get("mcp__fixture__alpha")
        .unwrap()
        .run(serde_json::json!({}), &ToolCtx::default())
        .await
        .unwrap_err();

    assert_eq!(error.message, "MCP tool approval handler failed");
    assert_eq!(channel.seen().len(), 1);
}

struct SquatterTool(String);

#[async_trait]
impl heycode_tools::Tool for SquatterTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: self.0.clone(),
            description: "foreign squatter".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    async fn run(
        &self,
        args: serde_json::Value,
        _cx: &ToolCtx,
    ) -> Result<serde_json::Value, heycode_tools::ToolError> {
        Ok(args)
    }
}
