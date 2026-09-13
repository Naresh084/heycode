//! Session envelope v1 read migration and v2 exhaustive round-trip contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    CURRENT_SESSION_LOG_VERSION, InboxDelivery, InboxMessage, InboxMessageId, InboxTarget,
    OpenError, RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, Role, Session, SessionCreationMetadata,
    SessionEventKind, SessionSource, TokenUsage, ToolCallOut, TurnEndReason, derive_messages,
};

fn seeded(contents: &str, name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let session_dir = root.path().join(name);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("session.jsonl"), contents).unwrap();
    (root, session_dir)
}

#[test]
fn golden_v1_replays_identically_and_next_append_is_v2_without_rewrite() {
    let original = include_str!("../fixtures/session-v1.jsonl");
    let (_root, session_dir) = seeded(original, "golden-v1");
    let mut session = Session::open(&session_dir).unwrap();
    assert!(session.events().iter().all(|event| event.v == 1));

    let messages = derive_messages(session.events());
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, Role::User);
    assert!(messages[0].content.contains("earlier setup"));
    assert_eq!(
        (messages[1].role, messages[1].content.as_str()),
        (Role::User, "hello")
    );
    assert_eq!(messages[2].role, Role::Assistant);
    assert_eq!(messages[2].tool_calls.as_ref().unwrap()[0].id, "call_1");
    assert_eq!(messages[3].role, Role::Tool);
    assert_eq!(
        messages[3].tool_call_id.as_ref().unwrap().as_str(),
        "call_1"
    );

    let appended = session
        .append(SessionEventKind::SessionTitle {
            title: "after migration".to_owned(),
        })
        .unwrap();
    assert_eq!(appended.v, CURRENT_SESSION_LOG_VERSION);
    drop(session);

    let raw = std::fs::read_to_string(session_dir.join("session.jsonl")).unwrap();
    assert!(raw.starts_with(original), "v1 bytes were rewritten");
    let lines: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 13);
    assert_eq!(lines[11]["v"], 1);
    assert_eq!(lines[12]["v"], CURRENT_SESSION_LOG_VERSION);

    let reopened = Session::open(&session_dir).unwrap();
    assert_eq!(reopened.events().len(), 13);
    assert!(reopened.events()[..12].iter().all(|event| event.v == 1));
    assert_eq!(reopened.events()[12].v, CURRENT_SESSION_LOG_VERSION);
}

fn all_v2_kinds() -> Vec<SessionEventKind> {
    vec![
        SessionEventKind::SessionCreated {
            creation: Box::new(
                heycode_session::SessionCreation::new(
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
        SessionEventKind::TurnStart { turn: 1 },
        SessionEventKind::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Stop,
        },
        SessionEventKind::StepStart { turn: 1, step: 2 },
        SessionEventKind::StepEnd { turn: 1, step: 2 },
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                InboxMessage::with_id(
                    InboxMessageId::new("inbox_1").unwrap(),
                    InboxDelivery::FollowUp,
                    "continue",
                )
                .unwrap(),
            ],
            outcome: None,
        },
        SessionEventKind::GoalChange {
            change: Box::new(heycode_session::GoalChange::snapshot(
                heycode_session::GoalOperation::Create,
                heycode_session::GoalSnapshot::new(
                    heycode_session::GoalId::new("goal_1").unwrap(),
                    1,
                    "finish the task",
                    heycode_session::GoalPhase::Active,
                    None,
                    8,
                )
                .unwrap(),
                0,
                1,
                1,
            )),
        },
        SessionEventKind::WorkflowChange {
            change: Box::new(heycode_session::WorkflowChange::start(
                heycode_session::WorkflowRunId::new("workflow_1").unwrap(),
                heycode_session::WorkflowDefinition::new(
                    "verify",
                    "verify the task",
                    vec![heycode_session::WorkflowCapability::Progress],
                    vec![
                        heycode_session::WorkflowStep::new(
                            "step_1",
                            "verify",
                            heycode_session::WorkflowAction::Emit {
                                value: serde_json::json!({"ok":true}),
                            },
                        )
                        .unwrap(),
                    ],
                )
                .unwrap(),
            )),
        },
        SessionEventKind::ScheduleChange {
            change: Box::new(heycode_session::ScheduleChange::create(
                heycode_session::ScheduleRecord::at(
                    heycode_session::ScheduleId::new("schedule_1").unwrap(),
                    "verify later",
                    1_730_000_001_000,
                )
                .unwrap(),
            )),
        },
        SessionEventKind::TeamChange {
            change: Box::new(
                heycode_session::TeamChange::created(
                    heycode_session::TeamId::new("team_1").unwrap(),
                    heycode_session::TeamMember::new(
                        heycode_session::TeamMemberId::new("lead").unwrap(),
                        "Lead",
                        heycode_session::TeamRole::Lead,
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
        },
        SessionEventKind::ReviewChange {
            change: Box::new(
                heycode_session::ReviewChange::started(
                    heycode_session::ReviewRunId::new("review_1").unwrap(),
                    "codex",
                    "0123456789012345678901234567890123456789",
                    "",
                    "review correctness",
                )
                .unwrap(),
            ),
        },
        SessionEventKind::HookContribution {
            contribution: Box::new(
                heycode_session::HookContributionRecord::new(
                    "fixture-owner",
                    heycode_session::HookContributionPhase::Pre,
                    heycode_session::HookContributionEvent::UserPrompt,
                    heycode_session::HookContributionHandler::Prompt,
                    None,
                    "hook context",
                )
                .unwrap(),
            ),
        },
        SessionEventKind::RequestHeader {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            header: Box::new(
                RequestHeaderSnapshot::new(
                    "provider",
                    "model",
                    heycode_core::ProviderProtocol::OpenAiChatCompletions,
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
                .unwrap(),
            ),
        },
        SessionEventKind::RequestContext {
            request_id: heycode_core::RequestId::from_raw("req_1"),
            context: RequestContextSnapshot::new(Some(100), Some(10), None, None, 1).unwrap(),
        },
        SessionEventKind::AttachmentAdded {
            attachment: Box::new(
                heycode_core::AttachmentMetadata::new(
                    heycode_core::AttachmentContentId::from_sha256([0x44; 32]),
                    heycode_core::AttachmentMediaType::new("image/png").unwrap(),
                    100,
                    Some("image.png".to_owned()),
                    Some(heycode_core::AttachmentDimensions::new(10, 10).unwrap()),
                )
                .unwrap(),
            ),
        },
        SessionEventKind::UserAttachments {
            attachments: vec![
                heycode_core::AttachmentMetadata::new(
                    heycode_core::AttachmentContentId::from_sha256([0x44; 32]),
                    heycode_core::AttachmentMediaType::new("image/png").unwrap(),
                    100,
                    Some("image.png".to_owned()),
                    Some(heycode_core::AttachmentDimensions::new(10, 10).unwrap()),
                )
                .unwrap(),
            ],
            document_routes: Vec::new(),
        },
        SessionEventKind::UserMessage {
            text: "hello".to_owned(),
        },
        SessionEventKind::AssistantChunk {
            turn: 1,
            step: 2,
            text: Some("x".to_owned()),
            reasoning: Some("r".to_owned()),
        },
        SessionEventKind::AssistantMessage {
            turn: 1,
            step: 2,
            content: "answer".to_owned(),
            reasoning: Some("reason".to_owned()),
            tool_calls: Some(vec![ToolCallOut {
                id: "call_1".to_owned(),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            }]),
            usage: Some(TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 2,
            }),
        },
        SessionEventKind::AssistantProviderItem {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            output_index: 0,
            item: Box::new(
                heycode_core::ProviderStateItem::new(
                    "provider",
                    "model",
                    heycode_core::ProviderProtocol::OpenAiChatCompletions,
                    heycode_core::ProviderStateKind::ChatAssistantMessage,
                    serde_json::json!({"role":"assistant","content":"answer"}),
                )
                .unwrap(),
            ),
        },
        SessionEventKind::AssistantResponseMetadata {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            metadata: Box::new(
                heycode_core::ProviderResponseMetadata::new(
                    Some(heycode_core::ProviderCacheUsage::new(10, 2, 4, 1).unwrap()),
                    Vec::new(),
                    None,
                )
                .unwrap(),
            ),
        },
        SessionEventKind::ServerToolCall {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            output_index: 0,
            call: Box::new(
                heycode_core::ServerToolCall::new(
                    heycode_core::CallId::from_raw("srvtoolu_1"),
                    "web_search",
                    "web_search",
                    serde_json::json!({"query":"rust"}),
                )
                .unwrap(),
            ),
        },
        SessionEventKind::ServerToolResult {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            output_index: 1,
            result: Box::new(
                heycode_core::ServerToolResult::success(
                    heycode_core::CallId::from_raw("srvtoolu_1"),
                    Some(0),
                    Vec::new(),
                )
                .unwrap(),
            ),
        },
        SessionEventKind::AssistantCitation {
            turn: 1,
            step: 2,
            request_id: heycode_core::RequestId::from_raw("req_1"),
            output_index: 2,
            citation: Box::new(
                heycode_core::UrlCitation::new(
                    "https://example.test/rust",
                    Some("Rust"),
                    None,
                    None,
                    None,
                )
                .unwrap(),
            ),
        },
        SessionEventKind::ToolCall {
            turn: 1,
            call_id: heycode_core::CallId::from_raw("call_1"),
            name: "read".to_owned(),
            args: serde_json::json!({}),
        },
        SessionEventKind::ToolResult {
            call_id: heycode_core::CallId::from_raw("call_1"),
            content: "ok".to_owned(),
            is_error: false,
            untrusted_content: None,
        },
        SessionEventKind::RichToolResult {
            call_id: heycode_core::CallId::from_raw("call_rich"),
            result: Box::new(
                heycode_core::DurableToolResult::new(
                    vec![heycode_core::DurableToolResultBlock::Text {
                        text: "rich".to_owned(),
                        metadata: heycode_core::ToolResultBlockMetadata::default(),
                    }],
                    heycode_core::ToolStructuredContent::Present(serde_json::Value::Null),
                    heycode_core::ToolResultSchemaCheck::Conforms,
                    serde_json::Map::new(),
                )
                .unwrap(),
            ),
            is_error: false,
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::mcp()),
        },
        SessionEventKind::CompactionApplied {
            summary: "summary".to_owned(),
            replaced_upto_seq: 1,
        },
        SessionEventKind::NativeCompactionApplied {
            strategy: "provider-native".to_owned(),
            replaced_upto_seq: 1,
            items: vec![
                heycode_core::ProviderStateItem::new(
                    "openai",
                    "gpt-5.6",
                    heycode_core::ProviderProtocol::OpenAiResponses,
                    heycode_core::ProviderStateKind::ResponseOutputItem,
                    serde_json::json!({
                        "type":"compaction",
                        "id":"cmp_1",
                        "encrypted_content":"opaque"
                    }),
                )
                .unwrap(),
            ],
            usage: None,
        },
        SessionEventKind::PlanMode { active: true },
        SessionEventKind::PlanReview {
            plan: "# Complete plan\n## Validation\nReopen this record".into(),
            decision: "stay_in_plan".into(),
            feedback: "Keep planning".into(),
        },
        SessionEventKind::SessionTitle {
            title: "title".to_owned(),
        },
    ]
}

#[test]
fn every_current_kind_round_trips_in_v2() {
    assert_eq!(CURRENT_SESSION_LOG_VERSION, 2);
    let root = tempfile::tempdir().unwrap();
    let expected = all_v2_kinds();
    let mut session = Session::create_with_metadata(
        root.path(),
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("/work/project")),
            Some("native".to_owned()),
            SessionSource::Interactive,
        )
        .unwrap(),
    )
    .unwrap();
    // `user/attachments` binds to the event that FOLLOWS it, so it is the one
    // kind the single-event append path must refuse: committing it alone would
    // durably write a log `Session::open` can never accept again. Round-tripping
    // it therefore goes through the atomic pair helper, and the refusal is
    // pinned here so this loop can no longer pass by accident of ordering.
    let mut index = 1;
    while index < expected.len() {
        match &expected[index] {
            SessionEventKind::UserAttachments {
                attachments,
                document_routes,
            } => {
                let error = session.append(expected[index].clone()).unwrap_err();
                assert!(
                    matches!(&error, heycode_session::AppendError::InvalidEvent { message }
                        if message.contains("user/message")),
                    "{error:?}"
                );
                let SessionEventKind::UserMessage { text } = &expected[index + 1] else {
                    panic!("a selection is only representable next to its user/message");
                };
                let event = session
                    .append_user_message_with_attachment_routes(
                        text.clone(),
                        attachments.clone(),
                        document_routes.clone(),
                    )
                    .unwrap();
                assert_eq!(event.v, 2);
                index += 2;
            }
            kind => {
                let event = session.append(kind.clone()).unwrap();
                assert_eq!(event.v, 2);
                index += 1;
            }
        }
    }
    let session_dir = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(session_dir).unwrap();
    assert_eq!(
        reopened
            .events()
            .iter()
            .map(|event| event.kind.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(reopened.events().iter().all(|event| event.v == 2));
}

#[test]
fn version_regression_and_outside_supported_range_fail_loud() {
    let v2 = serde_json::json!({
        "v":2,"seq":0,"time_ms":1,"kind":"user/message","data":{"text":"new"}
    });
    let v1 = serde_json::json!({
        "v":1,"seq":1,"time_ms":2,"kind":"user/message","data":{"text":"old"}
    });
    let (_root, regression_dir) = seeded(&format!("{v2}\n{v1}\n"), "regression");
    assert!(matches!(
        Session::open(regression_dir),
        Err(OpenError::VersionRegression {
            line_no: 2,
            previous: 2,
            found: 1,
        })
    ));

    for version in [0, 3] {
        let line = serde_json::json!({
            "v":version,"seq":0,"time_ms":1,
            "kind":"user/message","data":{"text":"x"}
        });
        let (_root, dir) = seeded(&(line.to_string() + "\n"), &format!("v{version}"));
        assert!(matches!(
            Session::open(dir),
            Err(OpenError::UnsupportedVersion {
                found,
                minimum: 1,
                maximum: 2,
            }) if found == version
        ));
    }
}

#[test]
fn v1_cannot_claim_the_v2_only_inbox_kind() {
    let line = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "agent/inbox/splice",
        "data": {
            "target": "next_turn",
            "start": 0,
            "inserted": [{"id":"inbox_1","delivery":"follow_up","text":"continue"}]
        }
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-inbox");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { line_no: 1, kind }) if kind == "agent/inbox/splice"
    ));
}

#[test]
fn v1_cannot_claim_team_or_review_domains() {
    for kind in ["team/change", "review/change"] {
        let line = serde_json::json!({
            "v":1,"seq":0,"time_ms":1,"kind":kind,"data":{}
        });
        let (_root, dir) = seeded(
            &(line.to_string() + "\n"),
            &format!("v1-{}", kind.replace('/', "-")),
        );
        assert!(matches!(
            Session::open(dir),
            Err(OpenError::UnknownKind { line_no: 1, kind: found }) if found == kind
        ));
    }
}

#[test]
fn v1_cannot_claim_detailed_provider_response_metadata() {
    let line = serde_json::json!({
        "v":1,"seq":0,"time_ms":1,"kind":"assistant/response-metadata",
        "data":{
            "turn":1,"step":1,"request_id":"req_1",
            "metadata":{
                "schema_version":1,
                "cache_usage":{
                    "schema_version":1,"input_tokens":10,"output_tokens":2,
                    "cache_read_tokens":4,"cache_write_tokens":1
                }
            }
        }
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-response-metadata");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { kind, .. }) if kind == "assistant/response-metadata"
    ));
}

#[test]
fn v1_cannot_claim_native_compaction_state() {
    let line = serde_json::json!({
        "v": 1,
        "seq": 1,
        "time_ms": 1,
        "kind": "compaction/native",
        "data": {
            "strategy":"provider-native",
            "replaced_upto_seq":0,
            "items":[]
        }
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-native-compaction");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { line_no: 1, kind }) if kind == "compaction/native"
    ));
}

#[test]
fn v1_tool_result_cannot_claim_the_untrusted_content_field() {
    let line = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "tool/result",
        "data": {
            "call_id": "call_1",
            "content": "external",
            "is_error": false,
            "untrusted_content": {"source":"web"}
        }
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-untrusted");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::InvalidEvent { line_no: 1, message })
            if message.contains("requires session envelope v2")
    ));
}

#[test]
fn v1_cannot_claim_the_v2_only_rich_tool_result_kind() {
    let line = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "tool/rich-result",
        "data": {}
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-rich-tool-result");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { line_no: 1, kind }) if kind == "tool/rich-result"
    ));
}

#[test]
fn v1_cannot_claim_the_v2_only_creation_kind() {
    let line = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "session/created",
        "data": {
            "creation": {
                "metadata": {"cwd":"/work/project","runtime":"native","source":"interactive"}
            }
        }
    });
    let (_root, dir) = seeded(&(line.to_string() + "\n"), "v1-created");
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { line_no: 1, kind }) if kind == "session/created"
    ));
}

#[test]
fn v1_cannot_claim_server_tool_or_citation_kinds() {
    for kind in [
        "server-tool/call",
        "server-tool/result",
        "server-tool/usage",
        "assistant/citation",
    ] {
        let line = serde_json::json!({
            "v": 1,
            "seq": 0,
            "time_ms": 1,
            "kind": kind,
            "data": {}
        });
        let (_root, dir) = seeded(&(line.to_string() + "\n"), &kind.replace('/', "-"));
        assert!(matches!(
            Session::open(dir),
            Err(OpenError::UnknownKind { line_no: 1, kind: found }) if found == kind
        ));
    }
}
