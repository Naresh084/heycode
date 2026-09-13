//! N02 durable normalized server-tool projection and correlation.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    CallId, ProviderProtocol, RequestId, ServerToolCall, ServerToolResult, ServerToolSource,
    ServerToolUsage, ServerToolUsageCost, ServerToolUsageEvidence, ServerToolWebMetadata,
    UrlCitation,
};
use heycode_session::{
    ProjectedServerToolEvent, ProjectionError, RequestAuthenticationSnapshot,
    RequestContextSnapshot, RequestHeaderSnapshot, RequestOptionsSnapshot, RequestTargetSnapshot,
    Session, SessionEvent, SessionEventKind, derive_messages, project_requests,
};

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: 2,
        seq,
        time_ms: i64::try_from(seq).unwrap() + 1,
        kind,
    }
}

fn header(provider: &str) -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        provider,
        "model-test",
        ProviderProtocol::AnthropicMessages,
        RequestTargetSnapshot::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::None,
        None,
        Vec::new(),
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: vec!["web".to_owned()],
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            defaulted_max_output_tokens: false,
            purpose: "conversation".to_owned(),
            retry: None,
        },
    )
    .unwrap()
}

fn context() -> RequestContextSnapshot {
    RequestContextSnapshot::new(Some(128_000), Some(8_192), Some(1), Some(10), 20).unwrap()
}

fn call() -> ServerToolCall {
    ServerToolCall::new(
        CallId::from_raw("srvtoolu_1"),
        "web_search",
        "web_search",
        serde_json::json!({"query":"rust"}),
    )
    .unwrap()
}

fn result() -> ServerToolResult {
    ServerToolResult::success(
        CallId::from_raw("srvtoolu_1"),
        Some(1),
        vec![
            ServerToolSource::new("https://example.test/rust", Some("Rust"))
                .unwrap()
                .with_web_metadata(
                    ServerToolWebMetadata::new(
                        "Example Docs",
                        "https://example.test/icon.png",
                        "ref_1",
                        "2026-08-29",
                    )
                    .unwrap(),
                )
                .unwrap(),
        ],
    )
    .unwrap()
}

fn citation() -> UrlCitation {
    UrlCitation::new(
        "https://example.test/rust",
        Some("Rust"),
        Some("Rust source"),
        None,
        None,
    )
    .unwrap()
}

fn aggregate_usage() -> ServerToolUsage {
    ServerToolUsage::new(
        "web_search",
        1,
        ServerToolUsageEvidence::ProviderAggregate,
        ServerToolUsageCost::Unknown,
    )
    .unwrap()
}

#[test]
fn request_projection_retains_ui_safe_events_and_provider_state_remains_the_replay_path() {
    let first = RequestId::from_raw("req_1");
    let second = RequestId::from_raw("req_2");
    let events = vec![
        event(
            0,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: first.clone(),
                header: Box::new(header("anthropic")),
            },
        ),
        event(
            1,
            SessionEventKind::RequestContext {
                request_id: first.clone(),
                context: context(),
            },
        ),
        event(
            2,
            SessionEventKind::ServerToolCall {
                turn: 1,
                step: 1,
                request_id: first.clone(),
                output_index: 0,
                call: Box::new(call()),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantCitation {
                turn: 1,
                step: 1,
                request_id: first,
                output_index: 1,
                citation: Box::new(citation()),
            },
        ),
        event(
            4,
            SessionEventKind::ServerToolUsage {
                turn: 1,
                step: 1,
                request_id: RequestId::from_raw("req_1"),
                usage: Box::new(aggregate_usage()),
            },
        ),
        event(
            5,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: second.clone(),
                header: Box::new(header("anthropic")),
            },
        ),
        event(
            6,
            SessionEventKind::RequestContext {
                request_id: second.clone(),
                context: context(),
            },
        ),
        event(
            7,
            SessionEventKind::ServerToolResult {
                turn: 1,
                step: 2,
                request_id: second,
                output_index: 0,
                result: Box::new(result()),
            },
        ),
    ];

    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    for event in &events {
        session.append(event.kind.clone()).unwrap();
    }
    drop(session);
    let reopened = Session::open(directory).unwrap();
    let SessionEventKind::ServerToolResult { result, .. } = &reopened.events()[7].kind else {
        panic!("the durable row changed kind")
    };
    assert_eq!(
        result.sources()[0].web_metadata().unwrap().published(),
        "2026-08-29"
    );

    let projected = project_requests(&events).unwrap();
    assert!(matches!(
        projected[0].server_tool_events.as_slice(),
        [
            ProjectedServerToolEvent::Call { call, .. },
            ProjectedServerToolEvent::Citation { citation, .. },
            ProjectedServerToolEvent::Usage { usage, .. }
        ] if call.logical() == "web_search"
            && citation.title() == Some("Rust")
            && usage.requests() == 1
    ));
    assert!(matches!(
        projected[1].server_tool_events.as_slice(),
        [ProjectedServerToolEvent::Result { result, .. }]
            if result.call_id() == &CallId::from_raw("srvtoolu_1")
                && result.sources()[0]
                    .web_metadata()
                    .is_some_and(|metadata| metadata.provider_reference() == "ref_1")
    ));
    assert!(derive_messages(&events).is_empty());
}

#[test]
fn projection_rejects_orphan_mismatched_duplicate_and_cross_route_server_events() {
    let request = RequestId::from_raw("req_1");
    let base = vec![
        event(
            0,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: request.clone(),
                header: Box::new(header("anthropic")),
            },
        ),
        event(
            1,
            SessionEventKind::RequestContext {
                request_id: request.clone(),
                context: context(),
            },
        ),
    ];

    let mut orphan = base.clone();
    orphan.push(event(
        2,
        SessionEventKind::ServerToolResult {
            turn: 1,
            step: 1,
            request_id: request.clone(),
            output_index: 0,
            result: Box::new(result()),
        },
    ));
    assert!(matches!(
        project_requests(&orphan),
        Err(ProjectionError::OrphanServerToolResult { .. })
    ));

    let mut mismatched = base.clone();
    mismatched.push(event(
        2,
        SessionEventKind::ServerToolCall {
            turn: 1,
            step: 2,
            request_id: request.clone(),
            output_index: 0,
            call: Box::new(call()),
        },
    ));
    assert!(matches!(
        project_requests(&mismatched),
        Err(ProjectionError::ServerToolStepMismatch { .. })
    ));

    let mut duplicate = base.clone();
    duplicate.push(event(
        2,
        SessionEventKind::ServerToolCall {
            turn: 1,
            step: 1,
            request_id: request.clone(),
            output_index: 0,
            call: Box::new(call()),
        },
    ));
    duplicate.push(event(
        3,
        SessionEventKind::ServerToolCall {
            turn: 1,
            step: 1,
            request_id: request,
            output_index: 1,
            call: Box::new(call()),
        },
    ));
    assert!(matches!(
        project_requests(&duplicate),
        Err(ProjectionError::DuplicateServerToolCall { .. })
    ));

    let first = RequestId::from_raw("req_a");
    let second = RequestId::from_raw("req_b");
    let cross_route = vec![
        event(
            0,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: first.clone(),
                header: Box::new(header("anthropic")),
            },
        ),
        event(
            1,
            SessionEventKind::RequestContext {
                request_id: first.clone(),
                context: context(),
            },
        ),
        event(
            2,
            SessionEventKind::ServerToolCall {
                turn: 1,
                step: 1,
                request_id: first,
                output_index: 0,
                call: Box::new(call()),
            },
        ),
        event(
            3,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: second.clone(),
                header: Box::new(header("other-provider")),
            },
        ),
        event(
            4,
            SessionEventKind::RequestContext {
                request_id: second.clone(),
                context: context(),
            },
        ),
        event(
            5,
            SessionEventKind::ServerToolResult {
                turn: 1,
                step: 2,
                request_id: second,
                output_index: 0,
                result: Box::new(result()),
            },
        ),
    ];
    assert!(matches!(
        project_requests(&cross_route),
        Err(ProjectionError::ServerToolRouteMismatch { .. })
    ));
}
