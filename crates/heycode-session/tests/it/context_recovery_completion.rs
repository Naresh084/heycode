//! P2-CX03/P2-CX05 configuration lineage and recoverable-prefix boundaries.
//!
//! This is a deterministic session-domain fixture. It does not claim a native
//! provider cache hit or Claude/OpenAI runtime parity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind, RequestId, ToolSpec};
use heycode_session::{
    ProjectedInput, RequestAuthenticationSnapshot, RequestConfigurationChange,
    RequestContextSnapshot, RequestHeaderSnapshot, RequestOptionsSnapshot,
    RequestRetrySafetySnapshot, RequestRetrySnapshot, RequestTargetSnapshot, Role, Session,
    SessionEventKind, WireMessage, project_requests,
};

const PRIMARY_PROVIDER: &str = "openai";
const PRIMARY_MODEL: &str = "gpt-primary";
const CHANGED_MODEL: &str = "gpt-changed";
const FALLBACK_PROVIDER: &str = "anthropic";
const FALLBACK_MODEL: &str = "claude-fallback";

fn options(max_output_tokens: u64) -> RequestOptionsSnapshot {
    RequestOptionsSnapshot {
        input_modalities: vec!["text".to_owned()],
        reasoning_effort: Some("high".to_owned()),
        defaulted_reasoning_effort: false,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(max_output_tokens),
        defaulted_max_output_tokens: false,
        purpose: "conversation".to_owned(),
        retry: Some(RequestRetrySnapshot {
            max_attempts: 3,
            safety: RequestRetrySafetySnapshot::StatelessPreOutput,
        }),
    }
}

fn read_tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}}
        }),
    }
}

fn write_tool() -> ToolSpec {
    ToolSpec {
        name: "write".to_owned(),
        description: "Write a file".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "path": {"type": "string"}
            }
        }),
    }
}

fn header(
    provider: &str,
    model: &str,
    protocol: ProviderProtocol,
    tools: Vec<ToolSpec>,
    options: RequestOptionsSnapshot,
) -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        provider,
        model,
        protocol,
        RequestTargetSnapshot::Http {
            base_url: format!("https://{provider}.example.test/v1"),
        },
        RequestAuthenticationSnapshot::Credential {
            reference: format!("{provider}/test-key"),
        },
        Some("stable system guidance".to_owned()),
        tools,
        options,
    )
    .unwrap()
}

fn context() -> RequestContextSnapshot {
    RequestContextSnapshot::new(Some(128_000), Some(8_192), Some(7), Some(11), 20).unwrap()
}

fn append_request(
    session: &mut Session,
    turn: u64,
    step: u32,
    id: &str,
    mut header: RequestHeaderSnapshot,
) -> RequestHeaderSnapshot {
    let previous = session
        .events()
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::RequestHeader { header, .. } => Some(header.as_ref().clone()),
            _ => None,
        });
    header.record_configuration(previous.as_ref()).unwrap();
    let request_id = RequestId::from_raw(id);
    session
        .append(SessionEventKind::RequestHeader {
            turn,
            step,
            request_id: request_id.clone(),
            header: Box::new(header.clone()),
        })
        .unwrap();
    session
        .append(SessionEventKind::RequestContext {
            request_id,
            context: context(),
        })
        .unwrap();
    header
}

fn append_response(
    session: &mut Session,
    turn: u64,
    step: u32,
    request_id: &str,
    header: &RequestHeaderSnapshot,
    state_id: &str,
    text: &str,
) {
    let (kind, data) = match header.protocol {
        ProviderProtocol::OpenAiResponses => (
            ProviderStateKind::ResponseOutputItem,
            serde_json::json!({
                "id": state_id,
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [{"type": "output_text", "text": text}]
            }),
        ),
        ProviderProtocol::AnthropicMessages => (
            ProviderStateKind::AnthropicMessage,
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": text}]
            }),
        ),
        other => panic!("unsupported fixture protocol: {other:?}"),
    };
    let item = ProviderStateItem::new(&header.provider, &header.model, header.protocol, kind, data)
        .unwrap();
    session
        .append(SessionEventKind::AssistantProviderItem {
            turn,
            step,
            request_id: RequestId::from_raw(request_id),
            output_index: 0,
            item: Box::new(item),
        })
        .unwrap();
    session
        .append(SessionEventKind::AssistantMessage {
            turn,
            step,
            content: text.to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        })
        .unwrap();
}

fn configuration(
    request: &heycode_session::ProjectedRequest,
) -> &heycode_session::RequestConfigurationSnapshot {
    request
        .header
        .configuration
        .as_ref()
        .expect("every fixture request records configuration identity")
}

fn message_count(inputs: &[ProjectedInput], role: Role, content: &str) -> usize {
    inputs
        .iter()
        .filter(|input| {
            matches!(
                input,
                ProjectedInput::Message(WireMessage {
                    role: actual_role,
                    content: actual_content,
                    ..
                }) if *actual_role == role && actual_content == content
            )
        })
        .count()
}

#[test]
fn configuration_and_prefix_boundaries_survive_resume_fallback_and_compaction() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();

    session
        .append(SessionEventKind::UserMessage {
            text: "original-user-canary".to_owned(),
        })
        .unwrap();
    let initial = append_request(
        &mut session,
        1,
        1,
        "request-initial",
        header(
            PRIMARY_PROVIDER,
            PRIMARY_MODEL,
            ProviderProtocol::OpenAiResponses,
            vec![read_tool()],
            options(1_024),
        ),
    );
    append_response(
        &mut session,
        1,
        1,
        "request-initial",
        &initial,
        "state-initial",
        "original-assistant-canary",
    );

    // Reopen before the next request: configuration lineage must come from the
    // durable log, while the exact same-route provider item replaces its neutral
    // assistant copy only once.
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let mut session = Session::open(&directory).unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: "resumed-user".to_owned(),
        })
        .unwrap();
    let resumed = append_request(
        &mut session,
        2,
        1,
        "request-resumed",
        header(
            PRIMARY_PROVIDER,
            PRIMARY_MODEL,
            ProviderProtocol::OpenAiResponses,
            vec![read_tool()],
            options(1_024),
        ),
    );
    let resumed_projection = project_requests(session.events()).unwrap();
    let resumed_request = resumed_projection.last().unwrap();
    assert_eq!(configuration(resumed_request).revision, 1);
    assert!(configuration(resumed_request).changed.is_empty());
    assert_eq!(
        configuration(resumed_request).sha256,
        configuration(&resumed_projection[0]).sha256
    );
    assert_eq!(
        message_count(&resumed_request.inputs, Role::User, "original-user-canary"),
        1
    );
    assert_eq!(
        message_count(
            &resumed_request.inputs,
            Role::Assistant,
            "original-assistant-canary"
        ),
        0
    );
    assert!(resumed_request.inputs.iter().any(|input| matches!(
        input,
        ProjectedInput::ProviderState(item)
            if item.data().get("id").and_then(serde_json::Value::as_str) == Some("state-initial")
    )));
    append_response(
        &mut session,
        2,
        1,
        "request-resumed",
        &resumed,
        "state-resumed",
        "resumed-answer",
    );

    session
        .append(SessionEventKind::UserMessage {
            text: "option-change-user".to_owned(),
        })
        .unwrap();
    let option_changed = append_request(
        &mut session,
        3,
        1,
        "request-options",
        header(
            PRIMARY_PROVIDER,
            PRIMARY_MODEL,
            ProviderProtocol::OpenAiResponses,
            vec![read_tool()],
            options(2_048),
        ),
    );
    append_response(
        &mut session,
        3,
        1,
        "request-options",
        &option_changed,
        "state-options",
        "option-answer",
    );

    session
        .append(SessionEventKind::UserMessage {
            text: "tool-change-user".to_owned(),
        })
        .unwrap();
    let tool_changed = append_request(
        &mut session,
        4,
        1,
        "request-tools",
        header(
            PRIMARY_PROVIDER,
            PRIMARY_MODEL,
            ProviderProtocol::OpenAiResponses,
            heycode_core::canonical_tool_specs(vec![write_tool(), read_tool()]),
            options(2_048),
        ),
    );
    append_response(
        &mut session,
        4,
        1,
        "request-tools",
        &tool_changed,
        "state-tools",
        "tool-answer",
    );

    // The primary request records no provider output. Fallback is a second
    // request step for the same user message: its prefix must be identical to
    // the primary step, while its route/configuration identity must advance.
    session
        .append(SessionEventKind::UserMessage {
            text: "fallback-user-once".to_owned(),
        })
        .unwrap();
    let changed_model = append_request(
        &mut session,
        5,
        1,
        "request-changed-model",
        header(
            PRIMARY_PROVIDER,
            CHANGED_MODEL,
            ProviderProtocol::OpenAiResponses,
            heycode_core::canonical_tool_specs(vec![write_tool(), read_tool()]),
            options(2_048),
        ),
    );
    let fallback = append_request(
        &mut session,
        5,
        2,
        "request-fallback",
        header(
            FALLBACK_PROVIDER,
            FALLBACK_MODEL,
            ProviderProtocol::AnthropicMessages,
            heycode_core::canonical_tool_specs(vec![write_tool(), read_tool()]),
            options(2_048),
        ),
    );
    append_response(
        &mut session,
        5,
        2,
        "request-fallback",
        &fallback,
        "state-fallback",
        "fallback-answer",
    );

    let before_compaction = project_requests(session.events()).unwrap();
    assert_eq!(before_compaction.len(), 6);
    assert_eq!(before_compaction[4].inputs, before_compaction[5].inputs);
    assert_eq!(
        message_count(
            &before_compaction[5].inputs,
            Role::User,
            "fallback-user-once"
        ),
        1
    );
    assert!(
        before_compaction[5]
            .inputs
            .iter()
            .all(|input| !matches!(input, ProjectedInput::ProviderState(_)))
    );
    assert_eq!(
        message_count(
            &before_compaction[5].inputs,
            Role::Assistant,
            "original-assistant-canary"
        ),
        1
    );

    let replaced_upto_seq = session.events().last().unwrap().seq;
    session
        .append(SessionEventKind::CompactionApplied {
            summary: "portable-summary-only".to_owned(),
            replaced_upto_seq,
        })
        .unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: "post-compaction-user".to_owned(),
        })
        .unwrap();
    let post_compaction = append_request(
        &mut session,
        6,
        1,
        "request-post-compaction",
        header(
            FALLBACK_PROVIDER,
            FALLBACK_MODEL,
            ProviderProtocol::AnthropicMessages,
            heycode_core::canonical_tool_specs(vec![write_tool(), read_tool()]),
            options(2_048),
        ),
    );

    let archived_values = session
        .events()
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect::<Vec<_>>();
    let raw = std::fs::read_to_string(session.path()).unwrap();
    drop(session);

    let reopened = Session::open(directory).unwrap();
    assert_eq!(
        reopened
            .events()
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>(),
        archived_values,
        "reopen must preserve every original event and value despite compaction"
    );
    assert!(raw.contains("original-user-canary"));
    assert!(raw.contains("original-assistant-canary"));
    assert!(raw.contains("portable-summary-only"));

    let requests = project_requests(reopened.events()).unwrap();
    assert_eq!(requests.len(), 7);
    let configurations = requests.iter().map(configuration).collect::<Vec<_>>();
    assert_eq!(configurations[0].revision, 1);
    assert_eq!(
        configurations[0].changed,
        [RequestConfigurationChange::Initial]
    );
    assert_eq!(configurations[1].revision, 1);
    assert!(configurations[1].changed.is_empty());
    assert_eq!(configurations[2].revision, 2);
    assert_eq!(
        configurations[2].changed,
        [RequestConfigurationChange::Options]
    );
    assert_eq!(configurations[3].revision, 3);
    assert_eq!(
        configurations[3].changed,
        [RequestConfigurationChange::Tools]
    );
    assert_eq!(configurations[4].revision, 4);
    assert_eq!(
        configurations[4].changed,
        [RequestConfigurationChange::Route]
    );
    assert_eq!(configurations[5].revision, 5);
    assert_eq!(
        configurations[5].changed,
        [RequestConfigurationChange::Route]
    );
    assert_eq!(configurations[6].revision, 5);
    assert!(configurations[6].changed.is_empty());

    assert_eq!(
        configurations[0].system_sha256,
        configurations[6].system_sha256
    );
    assert_eq!(
        configurations[0].tools_sha256,
        configurations[2].tools_sha256
    );
    assert_ne!(
        configurations[2].tools_sha256,
        configurations[3].tools_sha256
    );
    assert_eq!(
        configurations[1].options_sha256,
        configurations[0].options_sha256
    );
    assert_ne!(
        configurations[1].options_sha256,
        configurations[2].options_sha256
    );
    assert_eq!(
        configurations[3].route_sha256,
        configurations[2].route_sha256
    );
    assert_ne!(
        configurations[3].route_sha256,
        configurations[4].route_sha256
    );
    assert_ne!(
        configurations[4].route_sha256,
        configurations[5].route_sha256
    );
    assert_eq!(
        configuration(&requests[6]).sha256,
        configuration(&requests[5]).sha256
    );
    assert_eq!(
        post_compaction.configuration,
        requests[6].header.configuration
    );
    assert_eq!(
        changed_model.configuration,
        requests[4].header.configuration
    );

    let projected = &requests[6].inputs;
    assert!(matches!(
        projected.as_slice(),
        [
            ProjectedInput::Message(WireMessage { role: Role::User, content: summary, .. }),
            ProjectedInput::Message(WireMessage { role: Role::User, content: user, .. })
        ] if summary == "<compacted-summary>\nportable-summary-only\n</compacted-summary>"
            && user == "post-compaction-user"
    ));
    assert_eq!(
        message_count(projected, Role::User, "original-user-canary"),
        0
    );
    assert_eq!(
        message_count(projected, Role::Assistant, "original-assistant-canary"),
        0
    );
    assert!(
        projected
            .iter()
            .all(|input| !matches!(input, ProjectedInput::ProviderState(_)))
    );
}
