//! Protocol-aware request reconstruction from durable v2 events.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind, RequestId};
use heycode_session::{
    InboxDelivery, InboxMessage, InboxMessageId, InboxTarget, ProjectedInput, ProjectionError,
    RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, Role, SessionCreation, SessionCreationMetadata,
    SessionEvent, SessionEventKind, SessionSource, ToolCallOut, WireMessage,
    project_inputs_for_route, project_requests,
};

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: 2,
        seq,
        time_ms: i64::try_from(seq).unwrap() + 1,
        kind,
    }
}

fn header(provider: &str, model: &str, protocol: ProviderProtocol) -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        provider,
        model,
        protocol,
        RequestTargetSnapshot::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::None,
        Some("system".to_owned()),
        Vec::new(),
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: Vec::new(),
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

fn responses_item(kind: &str, id: &str) -> ProviderStateItem {
    ProviderStateItem::new(
        "openai",
        "gpt-test",
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({"id":id,"type":kind,"phase":"analysis"}),
    )
    .unwrap()
}

fn chat_item() -> ProviderStateItem {
    ProviderStateItem::new(
        "deepseek",
        "deepseek-reasoner",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,"reasoning_content":"why",
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{}"}}]
        }),
    )
    .unwrap()
}

fn anthropic_item() -> ProviderStateItem {
    ProviderStateItem::new(
        "anthropic",
        "claude-test",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[{"type":"text","text":"answer"}]
        }),
    )
    .unwrap()
}

#[test]
fn responses_projection_replays_items_and_suppresses_duplicate_assistant_message() {
    let req1 = RequestId::from_raw("req_1");
    let req2 = RequestId::from_raw("req_2");
    let events = vec![
        event(
            0,
            SessionEventKind::UserMessage {
                text: "first".into(),
            },
        ),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: req1.clone(),
                header: Box::new(header(
                    "openai",
                    "gpt-test",
                    ProviderProtocol::OpenAiResponses,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: req1.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: req1.clone(),
                output_index: 0,
                item: Box::new(responses_item("reasoning", "rs_1")),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: req1.clone(),
                output_index: 1,
                item: Box::new(responses_item("function_call", "fc_1")),
            },
        ),
        event(
            5,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: String::new(),
                reasoning: Some("generic duplicate".to_owned()),
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".to_owned(),
                    name: "read".to_owned(),
                    arguments: "{}".to_owned(),
                }]),
                usage: None,
            },
        ),
        event(
            6,
            SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("call_1"),
                content: "contents".to_owned(),
                is_error: true,
                untrusted_content: None,
            },
        ),
        event(
            7,
            SessionEventKind::UserMessage {
                text: "next".into(),
            },
        ),
        event(
            8,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: req2.clone(),
                header: Box::new(header(
                    "openai",
                    "gpt-test",
                    ProviderProtocol::OpenAiResponses,
                )),
            },
        ),
        event(
            9,
            SessionEventKind::RequestContext {
                request_id: req2.clone(),
                context: context(),
            },
        ),
    ];

    let requests = project_requests(&events).unwrap();
    assert_eq!(requests.len(), 2);
    let projected = &requests[1];
    assert_eq!(projected.request_id, req2);
    assert_eq!(projected.header.protocol, ProviderProtocol::OpenAiResponses);
    assert_eq!(projected.inputs.len(), 5);
    assert!(
        matches!(&projected.inputs[0], ProjectedInput::Message(WireMessage { role: Role::User, content, .. }) if content == "first")
    );
    assert!(
        matches!(&projected.inputs[1], ProjectedInput::ProviderState(item) if item.data()["id"] == "rs_1")
    );
    assert!(
        matches!(&projected.inputs[2], ProjectedInput::ProviderState(item) if item.data()["id"] == "fc_1")
    );
    assert!(
        matches!(&projected.inputs[3], ProjectedInput::Message(WireMessage {
            role: Role::Tool,
            content,
            tool_result_is_error: Some(true),
            ..
        }) if content == "contents")
    );
    assert!(
        matches!(&projected.inputs[4], ProjectedInput::Message(WireMessage { role: Role::User, content, .. }) if content == "next")
    );
    assert!(!projected.inputs.iter().any(|input| matches!(
        input,
        ProjectedInput::Message(WireMessage {
            role: Role::Assistant,
            ..
        })
    )));
}

#[test]
fn chat_projection_replays_complete_chat_state() {
    let req1 = RequestId::from_raw("chat_1");
    let req2 = RequestId::from_raw("chat_2");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: req1.clone(),
                header: Box::new(header(
                    "deepseek",
                    "deepseek-reasoner",
                    ProviderProtocol::OpenAiChatCompletions,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: req1.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: req1,
                output_index: 0,
                item: Box::new(chat_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: String::new(),
                reasoning: Some("duplicate".into()),
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                }]),
                usage: None,
            },
        ),
        event(
            5,
            SessionEventKind::ToolResult {
                call_id: heycode_core::CallId::from_raw("call_1"),
                content: "ok".into(),
                is_error: false,
                untrusted_content: None,
            },
        ),
        event(
            6,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: req2.clone(),
                header: Box::new(header(
                    "deepseek",
                    "deepseek-reasoner",
                    ProviderProtocol::OpenAiChatCompletions,
                )),
            },
        ),
        event(
            7,
            SessionEventKind::RequestContext {
                request_id: req2,
                context: context(),
            },
        ),
    ];
    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert_eq!(projected.inputs.len(), 3);
    assert!(
        matches!(&projected.inputs[1], ProjectedInput::ProviderState(item)
        if item.kind() == ProviderStateKind::ChatAssistantMessage
            && item.data()["reasoning_content"] == "why")
    );
    assert!(matches!(
        &projected.inputs[2],
        ProjectedInput::Message(WireMessage {
            role: Role::Tool,
            ..
        })
    ));
}

#[test]
fn incompatible_route_uses_neutral_assistant_fallback_not_opaque_state() {
    let chat_req = RequestId::from_raw("chat_1");
    let responses_req = RequestId::from_raw("responses_1");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: chat_req.clone(),
                header: Box::new(header(
                    "deepseek",
                    "deepseek-reasoner",
                    ProviderProtocol::OpenAiChatCompletions,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: chat_req.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: chat_req,
                output_index: 0,
                item: Box::new(chat_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: "fallback".into(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
        event(
            5,
            SessionEventKind::RequestHeader {
                turn: 2,
                step: 1,
                request_id: responses_req.clone(),
                header: Box::new(header(
                    "openai",
                    "gpt-test",
                    ProviderProtocol::OpenAiResponses,
                )),
            },
        ),
        event(
            6,
            SessionEventKind::RequestContext {
                request_id: responses_req,
                context: context(),
            },
        ),
    ];
    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert!(
        projected
            .inputs
            .iter()
            .all(|input| !matches!(input, ProjectedInput::ProviderState(_)))
    );
    assert!(
        matches!(&projected.inputs[1], ProjectedInput::Message(WireMessage { role:Role::Assistant, content, .. }) if content == "fallback")
    );
}

#[test]
fn anthropic_projection_replays_complete_message_state_without_generic_duplicate() {
    let first_request = RequestId::from_raw("anthropic_1");
    let next_request = RequestId::from_raw("anthropic_2");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: first_request.clone(),
                header: Box::new(header(
                    "anthropic",
                    "claude-test",
                    ProviderProtocol::AnthropicMessages,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: first_request.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: first_request,
                output_index: 0,
                item: Box::new(anthropic_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: "answer".into(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
        event(
            5,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: next_request.clone(),
                header: Box::new(header(
                    "anthropic",
                    "claude-test",
                    ProviderProtocol::AnthropicMessages,
                )),
            },
        ),
        event(
            6,
            SessionEventKind::RequestContext {
                request_id: next_request,
                context: context(),
            },
        ),
    ];

    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert_eq!(projected.inputs.len(), 2);
    assert!(matches!(
        &projected.inputs[1],
        ProjectedInput::ProviderState(item)
            if item.kind() == ProviderStateKind::AnthropicMessage
                && item.data()["content"][0]["text"] == "answer"
    ));
    assert!(!projected.inputs.iter().any(|input| matches!(
        input,
        ProjectedInput::Message(WireMessage {
            role: Role::Assistant,
            ..
        })
    )));
}

fn gemini_item() -> ProviderStateItem {
    ProviderStateItem::new(
        "google",
        "gemini-test",
        ProviderProtocol::GeminiGenerateContent,
        ProviderStateKind::GeminiModelContent,
        serde_json::json!({
            "role":"model",
            "parts":[{
                "functionCall":{"id":"fc-1","name":"read","args":{}},
                "thoughtSignature":"Signature-A"
            }]
        }),
    )
    .unwrap()
}

#[test]
fn gemini_projection_replays_the_model_content_without_generic_duplicate() {
    // The neutral assistant copy has no slot for a `thoughtSignature`, so
    // leaving it in the projection alongside the state would give the adapter a
    // signature-less turn to send.
    let first_request = RequestId::from_raw("gemini_1");
    let next_request = RequestId::from_raw("gemini_2");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: first_request.clone(),
                header: Box::new(header(
                    "google",
                    "gemini-test",
                    ProviderProtocol::GeminiGenerateContent,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: first_request.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: first_request,
                output_index: 0,
                item: Box::new(gemini_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: String::new(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
        event(
            5,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: next_request.clone(),
                header: Box::new(header(
                    "google",
                    "gemini-test",
                    ProviderProtocol::GeminiGenerateContent,
                )),
            },
        ),
        event(
            6,
            SessionEventKind::RequestContext {
                request_id: next_request,
                context: context(),
            },
        ),
    ];

    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert_eq!(projected.inputs.len(), 2);
    assert!(matches!(
        &projected.inputs[1],
        ProjectedInput::ProviderState(item)
            if item.kind() == ProviderStateKind::GeminiModelContent
                && item.data()["parts"][0]["thoughtSignature"] == "Signature-A"
    ));
    assert!(!projected.inputs.iter().any(|input| matches!(
        input,
        ProjectedInput::Message(WireMessage {
            role: Role::Assistant,
            ..
        })
    )));
}

#[test]
fn a_gemini_model_state_is_excluded_from_a_non_gemini_route_projection() {
    // Cross-route state is the desync C05 exists to catch: a Gemini `Content`
    // is not an Anthropic message, and the neutral fallback must remain.
    let first_request = RequestId::from_raw("gemini_cross_1");
    let next_request = RequestId::from_raw("anthropic_cross_2");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: first_request.clone(),
                header: Box::new(header(
                    "google",
                    "gemini-test",
                    ProviderProtocol::GeminiGenerateContent,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: first_request.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: first_request,
                output_index: 0,
                item: Box::new(gemini_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: "fallback".into(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
        event(
            5,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: next_request.clone(),
                header: Box::new(header(
                    "anthropic",
                    "claude-test",
                    ProviderProtocol::AnthropicMessages,
                )),
            },
        ),
        event(
            6,
            SessionEventKind::RequestContext {
                request_id: next_request,
                context: context(),
            },
        ),
    ];

    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert!(
        projected
            .inputs
            .iter()
            .all(|input| !matches!(input, ProjectedInput::ProviderState(_)))
    );
    assert!(matches!(
        &projected.inputs[1],
        ProjectedInput::Message(WireMessage { role: Role::Assistant, content, .. })
            if content == "fallback"
    ));
}

#[test]
fn creation_and_pending_inbox_are_not_admitted_into_a_request_projection() {
    let request_id = RequestId::from_raw("req_pending");
    let pending = InboxMessage::with_id(
        InboxMessageId::new("follow-1").unwrap(),
        InboxDelivery::FollowUp,
        "not admitted yet",
    )
    .unwrap();
    let events = vec![
        event(
            0,
            SessionEventKind::SessionCreated {
                creation: Box::new(
                    SessionCreation::new(
                        SessionCreationMetadata::new(
                            Some(std::path::PathBuf::from("/work/project")),
                            Some("native".to_owned()),
                            SessionSource::Interactive,
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                ),
            },
        ),
        event(
            1,
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: None,
                inserted: vec![pending],
                outcome: None,
            },
        ),
        event(
            2,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                header: Box::new(header(
                    "openai",
                    "gpt-test",
                    ProviderProtocol::OpenAiResponses,
                )),
            },
        ),
        event(
            3,
            SessionEventKind::RequestContext {
                request_id,
                context: context(),
            },
        ),
    ];

    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert!(projected.inputs.is_empty());
}

#[test]
fn missing_context_or_orphan_mismatched_state_fails_loud() {
    let request_id = RequestId::from_raw("req_1");
    let header_event = event(
        0,
        SessionEventKind::RequestHeader {
            turn: 1,
            step: 1,
            request_id: request_id.clone(),
            header: Box::new(header(
                "openai",
                "gpt-test",
                ProviderProtocol::OpenAiResponses,
            )),
        },
    );
    assert!(matches!(
        project_requests(std::slice::from_ref(&header_event)),
        Err(ProjectionError::MissingContext { request_id: found }) if found == request_id
    ));

    let orphan = event(
        0,
        SessionEventKind::AssistantProviderItem {
            turn: 1,
            step: 1,
            request_id: request_id.clone(),
            output_index: 0,
            item: Box::new(responses_item("reasoning", "rs")),
        },
    );
    assert!(matches!(
        project_requests(&[orphan]),
        Err(ProjectionError::OrphanProviderState { request_id: found, .. }) if found == request_id
    ));

    let mismatched = vec![
        header_event,
        event(
            1,
            SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: context(),
            },
        ),
        event(
            2,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                output_index: 0,
                item: Box::new(
                    ProviderStateItem::new(
                        "other",
                        "model",
                        ProviderProtocol::OpenAiResponses,
                        ProviderStateKind::ResponseOutputItem,
                        serde_json::json!({"id":"x","type":"reasoning"}),
                    )
                    .unwrap(),
                ),
            },
        ),
    ];
    assert!(matches!(
        project_requests(&mismatched),
        Err(ProjectionError::ProviderStateRouteMismatch { request_id: found, .. }) if found == request_id
    ));
}

#[test]
fn route_projection_restores_same_route_state_and_keeps_incompatible_fallback() {
    let request_id = RequestId::from_raw("chat_1");
    let events = vec![
        event(0, SessionEventKind::UserMessage { text: "go".into() }),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                header: Box::new(header(
                    "deepseek",
                    "deepseek-reasoner",
                    ProviderProtocol::OpenAiChatCompletions,
                )),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: context(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id,
                output_index: 0,
                item: Box::new(chat_item()),
            },
        ),
        event(
            4,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 1,
                content: "fallback".into(),
                reasoning: None,
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                }]),
                usage: None,
            },
        ),
    ];

    let same_route = project_inputs_for_route(
        &events,
        "deepseek",
        "deepseek-reasoner",
        ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert!(matches!(
        &same_route[1],
        ProjectedInput::ProviderState(item)
            if item.kind() == ProviderStateKind::ChatAssistantMessage
    ));
    assert!(!same_route.iter().any(|input| matches!(
        input,
        ProjectedInput::Message(WireMessage {
            role: Role::Assistant,
            ..
        })
    )));

    let incompatible = project_inputs_for_route(
        &events,
        "openai",
        "gpt-test",
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    assert!(
        incompatible
            .iter()
            .all(|input| !matches!(input, ProjectedInput::ProviderState(_)))
    );
    assert!(matches!(
        &incompatible[1],
        ProjectedInput::Message(WireMessage {
            role: Role::Assistant,
            content,
            ..
        }) if content == "fallback"
    ));
}

#[test]
fn route_projection_excludes_creation_and_pending_inbox_and_validates_history() {
    let pending = InboxMessage::with_id(
        InboxMessageId::new("follow-route").unwrap(),
        InboxDelivery::FollowUp,
        "not admitted yet",
    )
    .unwrap();
    let metadata = SessionCreationMetadata::new(
        Some(std::path::PathBuf::from("/work/project")),
        Some("native".to_owned()),
        SessionSource::Interactive,
    )
    .unwrap();
    let clean = vec![
        event(
            0,
            SessionEventKind::SessionCreated {
                creation: Box::new(SessionCreation::new(metadata).unwrap()),
            },
        ),
        event(
            1,
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: None,
                inserted: vec![pending],
                outcome: None,
            },
        ),
        event(
            2,
            SessionEventKind::UserMessage {
                text: "ready".into(),
            },
        ),
    ];
    let inputs = project_inputs_for_route(
        &clean,
        "deepseek",
        "deepseek-reasoner",
        ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert_eq!(inputs.len(), 1);
    assert!(matches!(
        &inputs[0],
        ProjectedInput::Message(WireMessage {
            role: Role::User,
            content,
            ..
        }) if content == "ready"
    ));

    let orphan = event(
        0,
        SessionEventKind::AssistantProviderItem {
            turn: 1,
            step: 1,
            request_id: RequestId::from_raw("orphan-route"),
            output_index: 0,
            item: Box::new(chat_item()),
        },
    );
    assert!(matches!(
        project_inputs_for_route(
            &[orphan],
            "deepseek",
            "deepseek-reasoner",
            ProviderProtocol::OpenAiChatCompletions,
        ),
        Err(ProjectionError::OrphanProviderState { .. })
    ));
}
