//! Inspector evidence must not conflate registration, setup and execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::turn::{build, script_text};
use heycode_agent::{AutoApprove, CommandRegistry, DenyAll, SERVICE_COMMANDS};
use heycode_session::SessionEventKind;
use std::sync::Arc;

#[tokio::test]
async fn tool_inspection_is_human_only_and_does_not_claim_unobserved_readiness() {
    let world = build(vec![], Arc::new(AutoApprove));
    let before = world.session.lock().unwrap().events().len();
    let catalog = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    assert!(catalog.registered_client_count > 0);
    assert_eq!(catalog.prepared_client_count, None);
    assert_eq!(catalog.enabled_count, None);
    let read = catalog.tools.iter().find(|row| row.name == "read").unwrap();
    assert_eq!(read.prepared, None);
    assert_eq!(read.successful_calls, Some(0));
    assert!(!read.owners.is_empty());
    let registry = world.ctx.get::<CommandRegistry>(SERVICE_COMMANDS).unwrap();
    for args in ["", "json", "read", "read json"] {
        registry
            .get("tools")
            .unwrap()
            .unwrap()
            .execute(&world.agent, args)
            .await
            .unwrap();
    }
    assert!(
        registry
            .get("tools")
            .unwrap()
            .unwrap()
            .execute(&world.agent, "no-such-tool")
            .await
            .is_err()
    );
    assert_eq!(world.session.lock().unwrap().events().len(), before);
    assert!(world.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tool_catalog_counts_only_correlated_successful_results_and_keeps_denial_visible() {
    let world = build(vec![], Arc::new(DenyAll));
    {
        let mut session = world.session.lock().unwrap();
        for (id, is_error) in [("succeeded", false), ("failed", true)] {
            let call_id = heycode_core::CallId::from_raw(id);
            session
                .append(SessionEventKind::ToolCall {
                    turn: 1,
                    call_id: call_id.clone(),
                    name: "read".into(),
                    args: serde_json::json!({"action":"status"}),
                })
                .unwrap();
            session
                .append(SessionEventKind::ToolResult {
                    call_id,
                    content: "fixture".into(),
                    is_error,
                    untrusted_content: None,
                })
                .unwrap();
        }
        session
            .append(SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("orphan"),
                content: "fixture".into(),
                is_error: false,
                untrusted_content: None,
            })
            .unwrap();
    }
    let catalog = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    let read = catalog.tools.iter().find(|row| row.name == "read").unwrap();
    assert_eq!(read.successful_calls, Some(1));
    assert_eq!(read.last_successful_action.as_deref(), Some("status"));
    assert!(read.reason.contains("denies calls"));
    assert_eq!(read.prepared, None);
}

#[tokio::test]
async fn tool_catalog_prepared_evidence_matches_the_actual_received_request() {
    let world = build(vec![script_text("done")], Arc::new(AutoApprove));
    world.agent.send("hello").await.unwrap();
    let catalog = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    let requests = world.requests.lock().unwrap();
    let request = requests.last().unwrap();
    // FakeProvider does not promise durable transport metadata. If the adapter
    // supplies no header the inspector must keep preparation unknown.
    if let Some(count) = catalog.prepared_client_count {
        assert_eq!(count, request.tools.as_deref().unwrap_or_default().len());
        for row in catalog.tools.iter().filter(|row| row.kind == "client") {
            assert_eq!(
                row.prepared,
                Some(
                    request
                        .tools
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|spec| spec.name == row.name)
                )
            );
        }
    } else {
        assert!(catalog.tools.iter().all(|row| row.prepared.is_none()));
    }
}

#[tokio::test]
async fn tool_catalog_does_not_reuse_an_older_matching_header_after_a_different_route() {
    use heycode_session::{
        RequestAuthenticationSnapshot, RequestHeaderSnapshot, RequestOptionsSnapshot,
        RequestTargetSnapshot,
    };
    let world = build(vec![], Arc::new(AutoApprove));
    let spec = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap()
        .tools
        .into_iter()
        .find(|row| row.name == "read")
        .unwrap()
        .schema
        .unwrap();
    let mut header = RequestHeaderSnapshot::new(
        "fake",
        "test-model",
        heycode_core::ProviderProtocol::OpenAiChatCompletions,
        RequestTargetSnapshot::Http {
            base_url: "http://127.0.0.1:1".into(),
        },
        RequestAuthenticationSnapshot::None,
        None,
        vec![spec],
        RequestOptionsSnapshot {
            input_modalities: vec!["text".into()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: vec![],
            native_tool_routes: vec![],
            provider_options: vec![],
            temperature: None,
            max_output_tokens: None,
            defaulted_max_output_tokens: false,
            purpose: "evaluation".into(),
            retry: None,
        },
    )
    .unwrap();
    let native = world
        .ctx
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    for provider in ["fake", "other"] {
        let candidate = heycode_native_tools::NativeToolImplementation::new(
            "web_search",
            format!("{provider}:web_search"),
            heycode_core::NativeToolImplementationKind::Provider,
            Some(provider.into()),
            10,
        )
        .unwrap();
        if provider == "fake" {
            header
                .options
                .native_tool_routes
                .push(candidate.route().clone());
        }
        native.register(&world.ctx, candidate).unwrap();
    }
    let native_request_id = heycode_core::RequestId::generate();
    world
        .session
        .lock()
        .unwrap()
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 1,
            request_id: native_request_id.clone(),
            header: Box::new(header.clone()),
        })
        .unwrap();
    let matching = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    assert_eq!(matching.prepared_client_count, Some(1));
    assert_eq!(
        matching
            .tools
            .iter()
            .find(|row| row.name == "other:web_search")
            .unwrap()
            .route_selected,
        Some(false)
    );
    assert!(
        matching
            .tools
            .iter()
            .find(|row| row.name == "other:web_search")
            .unwrap()
            .reason
            .contains("Incompatible")
    );
    {
        let mut session = world.session.lock().unwrap();
        let call_id = heycode_core::CallId::generate();
        session
            .append(SessionEventKind::ServerToolCall {
                turn: 1,
                step: 1,
                request_id: native_request_id.clone(),
                output_index: 0,
                call: Box::new(
                    heycode_core::ServerToolCall::new(
                        call_id.clone(),
                        "web_search",
                        "web_search",
                        serde_json::json!({"query":"fixture"}),
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(SessionEventKind::ServerToolResult {
                turn: 1,
                step: 1,
                request_id: native_request_id,
                output_index: 0,
                result: Box::new(
                    heycode_core::ServerToolResult::success(call_id, Some(0), vec![]).unwrap(),
                ),
            })
            .unwrap();
    }
    let observed = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    assert_eq!(
        observed
            .tools
            .iter()
            .find(|row| row.name == "fake:web_search")
            .unwrap()
            .successful_calls,
        Some(1)
    );
    assert_eq!(
        observed
            .tools
            .iter()
            .find(|row| row.name == "other:web_search")
            .unwrap()
            .successful_calls,
        Some(0)
    );
    assert_eq!(
        matching
            .tools
            .iter()
            .find(|row| row.name == "read")
            .unwrap()
            .prepared,
        Some(true)
    );
    assert_eq!(
        matching
            .tools
            .iter()
            .find(|row| row.name == "write")
            .unwrap()
            .prepared,
        Some(false)
    );
    header.model = "different-model".into();
    world
        .session
        .lock()
        .unwrap()
        .append(SessionEventKind::RequestHeader {
            turn: 2,
            step: 1,
            request_id: heycode_core::RequestId::generate(),
            header: Box::new(header),
        })
        .unwrap();
    let changed = world
        .agent
        .tool_catalog(&world.ctx.plugin_inventory())
        .unwrap();
    assert_eq!(changed.prepared_client_count, None);
    assert!(changed.tools.iter().all(|row| row.prepared.is_none()));
}
