//! V2 provider-owned continuation state durability contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    ProviderProtocol, ProviderStateError, ProviderStateItem, ProviderStateKind, RequestId,
};
use heycode_session::{OpenError, Session, SessionEventKind};

fn responses_state() -> ProviderStateItem {
    ProviderStateItem::new(
        "openai",
        "gpt-test",
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "id":"rs_1","type":"reasoning","encrypted_content":"opaque",
            "summary":[],"phase":"analysis"
        }),
    )
    .unwrap()
}

fn chat_state() -> ProviderStateItem {
    ProviderStateItem::new(
        "deepseek",
        "deepseek-reasoner",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant","content":null,"reasoning_content":"why",
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{\"path\":\"a\"}"}
            }]
        }),
    )
    .unwrap()
}

fn anthropic_state() -> ProviderStateItem {
    ProviderStateItem::new(
        "anthropic",
        "claude-test",
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"thinking","thinking":"opaque","signature":"sig"},
                {"type":"text","text":"answer"}
            ]
        }),
    )
    .unwrap()
}

fn bedrock_state() -> ProviderStateItem {
    ProviderStateItem::new(
        "bedrock",
        "anthropic.claude-test",
        ProviderProtocol::BedrockConverse,
        ProviderStateKind::BedrockConverseMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[
                {"reasoningContent":{"text":"opaque","signature":"sig"}},
                {"text":"answer"}
            ]
        }),
    )
    .unwrap()
}

#[test]
fn provider_states_round_trip_losslessly_in_order() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let request_id = RequestId::from_raw("req_1");
    let expected = [
        responses_state(),
        chat_state(),
        anthropic_state(),
        bedrock_state(),
    ];
    for (output_index, item) in expected.iter().cloned().enumerate() {
        session
            .append(SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 2,
                request_id: request_id.clone(),
                output_index: u32::try_from(output_index).unwrap(),
                item: Box::new(item),
            })
            .unwrap();
    }
    let dir = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let reopened = Session::open(dir).unwrap();
    for (index, event) in reopened.events().iter().enumerate() {
        match &event.kind {
            SessionEventKind::AssistantProviderItem {
                request_id: found_id,
                output_index,
                item,
                ..
            } => {
                assert_eq!(found_id, &request_id);
                assert_eq!(*output_index as usize, index);
                assert_eq!(item.as_ref(), &expected[index]);
            }
            other => panic!("expected provider item, got {other:?}"),
        }
    }
    assert_eq!(reopened.events()[0].kind.name(), "assistant/provider-item");
}

#[test]
fn provider_item_kind_is_v2_only() {
    let line = serde_json::json!({
        "v":1,"seq":0,"time_ms":1,"kind":"assistant/provider-item",
        "data":{
            "turn":1,"step":1,"request_id":"req_1","output_index":0,
            "item":{
                "provider":"openai","model":"gpt-test","protocol":"open_ai_responses",
                "kind":"response_output_item","schema_version":1,
                "data":{"id":"x","type":"reasoning"}
            }
        }
    });
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("v1-provider-item");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("session.jsonl"), line.to_string() + "\n").unwrap();
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { kind, .. }) if kind == "assistant/provider-item"
    ));
}

#[test]
fn tampered_state_schema_and_non_object_data_fail_on_read() {
    for mutate in ["schema", "data"] {
        let root = tempfile::tempdir().unwrap();
        let mut session = Session::create(root.path()).unwrap();
        session
            .append(SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: RequestId::from_raw("req_1"),
                output_index: 0,
                item: Box::new(responses_state()),
            })
            .unwrap();
        let path = session.path().to_path_buf();
        let dir = path.parent().unwrap().to_path_buf();
        drop(session);
        let mut line: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        if mutate == "schema" {
            line["data"]["item"]["schema_version"] = serde_json::json!(2);
        } else {
            line["data"]["item"]["data"] = serde_json::json!("not-object");
        }
        std::fs::write(&path, line.to_string() + "\n").unwrap();
        assert!(matches!(
            Session::open(dir),
            Err(OpenError::InvalidEvent { line_no: 1, .. })
        ));
    }
}

#[test]
fn provider_state_constructor_enforces_protocol_kind_compatibility() {
    let error = ProviderStateItem::new(
        "provider",
        "model",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({"type":"reasoning"}),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ProviderStateError::ProtocolKindMismatch { .. }
    ));
}
