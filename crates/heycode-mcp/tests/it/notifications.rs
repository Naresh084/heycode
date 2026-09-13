//! Notification routing and the connection's single generation candidate.
//!
//! Three listings change independently, so three epochs must advance
//! independently, and one connection publishes one candidate carrying all three
//! counts. Everything here is driven through the injected request channel.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Plugin, compose};
use heycode_mcp::prompts::McpPromptGenerationOwner;
use heycode_mcp::resources::McpResourceCapability;
use heycode_mcp::{
    McpChannelError, McpConnectionProviderId, McpDefinitionScope, McpNotificationKind,
    McpNotificationRouter, McpRegistry, McpRequestChannel, McpServerDefinition, McpServerHandshake,
    McpServerId, McpSiblingContributions, McpStreamableHttpTransport, McpToolGenerationOwner,
    McpToolListLimits, McpTransportDefinition, SERVICE_MCP, mcp_registry_plugin,
};
use heycode_tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

// ── injected transport ──────────────────────────────────────────────────────

struct ScriptedChannel {
    responses: Mutex<VecDeque<Result<serde_json::Value, McpChannelError>>>,
    methods: Mutex<Vec<String>>,
}

impl ScriptedChannel {
    fn with(responses: Vec<Result<serde_json::Value, McpChannelError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            methods: Mutex::new(Vec::new()),
        })
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().unwrap().clone()
    }
}

#[async_trait]
impl McpRequestChannel for ScriptedChannel {
    async fn call(
        &self,
        method: &str,
        _params: serde_json::Value,
        _cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        self.methods.lock().unwrap().push(method.to_owned());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unscripted MCP request `{method}`"))
    }
}

// ── fixtures ────────────────────────────────────────────────────────────────

fn initialize_result(capabilities: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": capabilities,
        "serverInfo": {"name": "fixture", "version": "1.4"}
    })
}

fn handshake(capabilities: serde_json::Value) -> McpServerHandshake {
    McpServerHandshake::from_initialize_result(&initialize_result(capabilities)).unwrap()
}

fn notification(method: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "method": method})
}

fn resource_page(uris: &[&str]) -> Result<serde_json::Value, McpChannelError> {
    Ok(serde_json::json!({
        "resources": uris
            .iter()
            .map(|uri| serde_json::json!({"uri": uri, "name": "row"}))
            .collect::<Vec<_>>()
    }))
}

fn prompt_page(names: &[&str]) -> Result<serde_json::Value, McpChannelError> {
    Ok(serde_json::json!({
        "prompts": names
            .iter()
            .map(|name| serde_json::json!({"name": name}))
            .collect::<Vec<_>>()
    }))
}

fn tool_page(names: &[&str]) -> Result<serde_json::Value, McpChannelError> {
    Ok(serde_json::json!({
        "tools": names
            .iter()
            .map(|name| serde_json::json!({
                "name": name, "description": "t", "inputSchema": {"type": "object"}
            }))
            .collect::<Vec<_>>()
    }))
}

// ── router: one epoch per family ────────────────────────────────────────────

#[test]
fn each_family_advances_only_its_own_epoch() {
    let router = McpNotificationRouter::new();
    let resources = router.resources();

    router.observe(&notification(
        McpNotificationKind::ToolsListChanged.method(),
    ));

    assert_eq!(router.tools().epoch(), 1);
    assert_eq!(
        router.prompts().epoch(),
        0,
        "a tool change is not a prompt change"
    );
    assert_eq!(resources.inspect().list_change_epoch(), 0);

    router.observe(&notification(
        McpNotificationKind::PromptsListChanged.method(),
    ));

    assert_eq!(
        router.tools().epoch(),
        1,
        "a prompt change is not a tool change"
    );
    assert_eq!(router.prompts().epoch(), 1);
    assert_eq!(resources.inspect().list_change_epoch(), 0);

    router.observe(&notification(
        McpNotificationKind::ResourcesListChanged.method(),
    ));

    assert_eq!(router.tools().epoch(), 1);
    assert_eq!(router.prompts().epoch(), 1);
    assert_eq!(resources.inspect().list_change_epoch(), 1);
}

#[test]
fn the_router_hands_out_the_registry_its_updates_reach() {
    let router = McpNotificationRouter::new();
    let before = router.resources().inspect().list_change_epoch();

    router.observe(&notification(
        McpNotificationKind::ResourcesListChanged.method(),
    ));

    // Obtained separately from the observation, so this fails if `resources()`
    // ever hands out anything but the registry the router feeds.
    assert_eq!(router.resources().inspect().list_change_epoch(), before + 1);
}

#[tokio::test]
async fn a_resource_update_reaches_a_subscription_through_the_router() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let router = McpNotificationRouter::new();
    let registry = router.resources();
    let channel = ScriptedChannel::with(vec![
        resource_page(&["file:///a"]),
        Ok(serde_json::json!({})),
    ]);
    registry
        .refresh(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            McpResourceCapability::from_initialize_result(&initialize_result(
                serde_json::json!({"resources": {"subscribe": true}}),
            ))
            .unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let _subscription = registry
        .subscribe(
            &context,
            "file:///a",
            move |_update| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    let routed = router.observe(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/resources/updated",
        "params": {"uri": "file:///a"}
    }));

    assert_eq!(routed, Some(McpNotificationKind::ResourceUpdated));
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    context.shutdown();
}

#[test]
fn a_response_is_never_routed_as_a_notification() {
    let router = McpNotificationRouter::new();

    // A response carries an id. Classifying it by method alone would let a
    // `tools/list` reply advance the epoch of the walk that requested it.
    let routed = router.observe(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "notifications/tools/list_changed"
    }));

    assert_eq!(routed, None);
    assert_eq!(router.tools().epoch(), 0);
}

#[test]
fn a_human_event_without_an_attached_sink_still_touches_no_listing_epoch() {
    let router = McpNotificationRouter::new();

    let routed = router.observe(&notification("notifications/message"));

    assert_eq!(routed, Some(McpNotificationKind::LogMessage));
    assert_eq!(router.tools().epoch(), 0);
    assert_eq!(router.prompts().epoch(), 0);
    assert_eq!(router.resources().inspect().list_change_epoch(), 0);
}

#[test]
fn a_clone_shares_one_connection_truth() {
    let router = McpNotificationRouter::new();
    let clone = router.clone();

    clone.observe(&notification(
        McpNotificationKind::PromptsListChanged.method(),
    ));

    assert_eq!(router.prompts().epoch(), 1);
}

#[test]
fn the_routed_methods_match_the_specification() {
    assert_eq!(
        McpNotificationKind::ToolsListChanged.method(),
        "notifications/tools/list_changed"
    );
    assert_eq!(
        McpNotificationKind::PromptsListChanged.method(),
        "notifications/prompts/list_changed"
    );
    assert_eq!(
        McpNotificationKind::ResourcesListChanged.method(),
        "notifications/resources/list_changed"
    );
    assert_eq!(
        McpNotificationKind::ResourceUpdated.method(),
        "notifications/resources/updated"
    );
    assert_eq!(
        McpNotificationKind::Progress.method(),
        "notifications/progress"
    );
    assert_eq!(
        McpNotificationKind::LogMessage.method(),
        "notifications/message"
    );
    assert_eq!(
        McpNotificationKind::Cancelled.method(),
        "notifications/cancelled"
    );
    assert_eq!(
        McpNotificationKind::ElicitationComplete.method(),
        "notifications/elicitation/complete"
    );
    let mut methods: Vec<&str> = McpNotificationKind::ALL
        .iter()
        .map(|k| k.method())
        .collect();
    methods.sort_unstable();
    methods.dedup();
    assert_eq!(methods.len(), McpNotificationKind::ALL.len());
}

// ── one handshake, three kinds of evidence ──────────────────────────────────

#[test]
fn one_initialize_result_yields_tools_resources_and_prompt_evidence() {
    let handshake = handshake(serde_json::json!({
        "tools": {},
        "resources": {"subscribe": true},
        "prompts": {"listChanged": true}
    }));

    assert!(handshake.capabilities().tools);
    assert!(handshake.resources().resources().is_supported());
    assert!(handshake.resources().subscribe().is_supported());
    assert!(handshake.prompts().prompts().notifies_on_change());
}

#[test]
fn server_instructions_are_reachable_from_the_handshake() {
    let mut result = initialize_result(serde_json::json!({"prompts": {}}));
    result["instructions"] = serde_json::Value::String("Use the review prompt.".to_owned());
    let handshake = McpServerHandshake::from_initialize_result(&result).unwrap();

    let instructions = handshake.instructions().present().expect("present");
    assert_eq!(instructions.untrusted_text(), "Use the review prompt.");
    assert!(
        instructions
            .render_for_model()
            .starts_with("[BEGIN UNTRUSTED MCP SERVER CONTENT")
    );
}

#[test]
fn a_malformed_prompts_capability_fails_the_whole_handshake() {
    let error = McpServerHandshake::from_initialize_result(&initialize_result(serde_json::json!({
        "prompts": {"listChanged": "yes"}
    })))
    .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "prompts.listChanged must be absent, null or a boolean"
        }
    ));
}

#[test]
fn a_malformed_resources_capability_fails_the_whole_handshake() {
    let error = McpServerHandshake::from_initialize_result(&initialize_result(serde_json::json!({
        "resources": {"subscribe": "yes"}
    })))
    .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

// ── one connection, one candidate, three counts ─────────────────────────────

#[test]
fn an_advertised_prompt_listing_that_was_not_walked_refuses_publication() {
    let handshake = handshake(serde_json::json!({"tools": {}, "prompts": {}}));

    let error = handshake
        .candidate(3, McpSiblingContributions::none(), 0)
        .unwrap_err();

    assert!(matches!(
        error,
        McpChannelError::Protocol {
            requirement: "a generation cannot report an advertised listing that was never walked"
        }
    ));
}

#[test]
fn an_advertised_resource_listing_that_was_not_walked_refuses_publication() {
    let handshake = handshake(serde_json::json!({"tools": {}, "resources": {}}));

    let error = handshake
        .candidate(3, McpSiblingContributions::none(), 0)
        .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

#[test]
fn an_unadvertised_listing_reports_zero_without_a_walk() {
    let handshake = handshake(serde_json::json!({"tools": {}}));

    let candidate = handshake
        .candidate(3, McpSiblingContributions::none(), 0)
        .unwrap();

    // Zero against a false capability bit is honest: the bit says why.
    assert!(format!("{candidate:?}").contains("resources: 0"));
    assert!(format!("{candidate:?}").contains("prompts: 0"));
}

#[test]
fn a_walked_empty_listing_is_accepted_for_an_advertised_capability() {
    let handshake = handshake(serde_json::json!({"tools": {}, "prompts": {}}));

    let candidate = handshake
        .candidate(1, McpSiblingContributions::none().with_prompts(0), 0)
        .unwrap();

    // "Advertises prompts, listed none" is a fact a walk earned.
    assert!(format!("{candidate:?}").contains("prompts: 0"));
}

#[test]
fn a_count_without_the_advertised_capability_is_still_refused() {
    let handshake = handshake(serde_json::json!({"tools": {}}));

    let error = handshake
        .candidate(1, McpSiblingContributions::none().with_prompts(2), 0)
        .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
}

#[tokio::test]
async fn one_generation_publishes_all_three_counts() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://example.test/mcp", Default::default()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "fixture",
                "fixture",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("test").unwrap(),
            0,
        )
        .unwrap();
    let router = McpNotificationRouter::new();
    let handshake = handshake(serde_json::json!({
        "tools": {}, "resources": {}, "prompts": {}
    }));
    let channel = ScriptedChannel::with(vec![
        resource_page(&["file:///a", "file:///b"]),
        prompt_page(&["review"]),
        tool_page(&["echo"]),
    ]);
    let cancellation = CancellationToken::new();

    // The order one connection provider uses: siblings first, tools last,
    // because the tool owner is what publishes the single candidate.
    let resources = router
        .resources()
        .refresh(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            handshake.resources(),
            &cancellation,
        )
        .await
        .unwrap();
    let prompts = McpPromptGenerationOwner::new(router.prompts(), Default::default())
        .refresh(channel.as_ref(), handshake.prompts(), &cancellation)
        .await
        .unwrap();
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::new(ToolRegistry::new()),
        publisher,
        router.tools(),
        McpToolListLimits::default(),
    );
    let generation = owner
        .refresh(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            &handshake,
            McpSiblingContributions::none()
                .with_resources(u32::try_from(resources.resources().len()).unwrap())
                .with_prompts(prompts.count()),
            &cancellation,
        )
        .await
        .unwrap();

    assert_eq!(
        channel.methods(),
        ["resources/list", "prompts/list", "tools/list"]
    );
    assert_eq!(generation.contributions().tools, 1);
    assert_eq!(generation.contributions().resources, 2);
    assert_eq!(generation.contributions().prompts, 1);
    assert_eq!(generation.number(), 1, "exactly one generation, not three");
    context.shutdown();
}

#[tokio::test]
async fn a_tool_walk_cannot_publish_without_the_sibling_counts() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    let id = McpServerId::new("fixture").unwrap();
    let transport =
        McpStreamableHttpTransport::new("https://example.test/mcp", Default::default()).unwrap();
    registry
        .register_definition(
            &context,
            McpServerDefinition::new(
                "fixture",
                "fixture",
                McpDefinitionScope::User,
                McpTransportDefinition::StreamableHttp(transport),
            )
            .unwrap(),
        )
        .unwrap();
    let publisher = registry
        .register_connection(
            &context,
            &id,
            McpConnectionProviderId::new("test").unwrap(),
            0,
        )
        .unwrap();
    let router = McpNotificationRouter::new();
    let definition = registry.definition(&id).unwrap().unwrap();
    let owner = McpToolGenerationOwner::new(
        &definition,
        Arc::new(ToolRegistry::new()),
        publisher,
        router.tools(),
        McpToolListLimits::default(),
    );
    let channel = ScriptedChannel::with(vec![tool_page(&["echo"])]);

    let error = owner
        .refresh(
            Arc::clone(&channel) as Arc<dyn McpRequestChannel>,
            &handshake(serde_json::json!({"tools": {}, "prompts": {}})),
            McpSiblingContributions::none(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, McpChannelError::Protocol { .. }));
    assert!(
        registry.snapshot().unwrap().servers()[0]
            .last_good_generation()
            .is_none(),
        "nothing may be published"
    );
    context.shutdown();
}
