//! R02 delegated-runtime event normalization and replay contracts.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use futures::{StreamExt as _, stream};
use heycode_core::{CallId, TokenUsage};
use heycode_runtime::{
    MAX_RUNTIME_EVENT_IDENTITIES, NormalizedRuntimeEvent, RuntimeEvent, RuntimeEventKind,
    RuntimeEventNormalizer, RuntimeEventStream, RuntimeEventViolationCode, RuntimeFinishReason,
    RuntimeRequestId, RuntimeTurnId, normalize_runtime_event_replay,
    normalize_runtime_event_stream,
};
use serde_json::json;

fn turn(value: &str) -> RuntimeTurnId {
    RuntimeTurnId::new(value).unwrap()
}

fn request(value: &str) -> RuntimeRequestId {
    RuntimeRequestId::new(value).unwrap()
}

fn event(sequence: u64, kind: RuntimeEventKind) -> RuntimeEvent {
    RuntimeEvent::new(sequence, kind)
}

fn complete_replay() -> Vec<RuntimeEvent> {
    vec![
        event(0, RuntimeEventKind::SessionReady),
        event(
            1,
            RuntimeEventKind::TurnStarted {
                turn: turn("turn-1"),
            },
        ),
        event(
            2,
            RuntimeEventKind::CommentaryDelta {
                text: "Inspecting the workspace".to_owned(),
            },
        ),
        event(
            3,
            RuntimeEventKind::ReasoningDelta {
                text: "A bounded safe thought".to_owned(),
            },
        ),
        event(
            4,
            RuntimeEventKind::ToolCall {
                call_id: CallId::from_raw("call-1"),
                name: "read".to_owned(),
                arguments: json!({"path": "src/lib.rs"}),
            },
        ),
        event(
            5,
            RuntimeEventKind::PermissionRequested {
                request_id: request("permission-1"),
                action: "Read source file".to_owned(),
                detail: "The delegated runtime requested workspace read access.".to_owned(),
            },
        ),
        event(
            6,
            RuntimeEventKind::ToolResult {
                call_id: CallId::from_raw("call-1"),
                result: json!({"content": "bounded result"}),
                is_error: false,
            },
        ),
        event(
            7,
            RuntimeEventKind::QuestionRequested {
                mode: heycode_core::QuestionMode::SingleChoice,
                progress: (1, 1),
                request_id: request("question-1"),
                header: None,
                prompt: "Continue with the focused fix?".to_owned(),
                choices: vec!["Continue".to_owned(), "Stop".to_owned()],
                choice_descriptions: vec![None, None],
            },
        ),
        event(
            8,
            RuntimeEventKind::FinalMessage {
                text: "The focused fix is complete.".to_owned(),
            },
        ),
        event(
            9,
            RuntimeEventKind::Usage {
                usage: TokenUsage {
                    prompt_tokens: 101,
                    completion_tokens: 23,
                },
                context: None,
            },
        ),
        event(
            10,
            RuntimeEventKind::TurnFinished {
                turn: turn("turn-1"),
                reason: RuntimeFinishReason::Stop,
            },
        ),
        event(
            11,
            RuntimeEventKind::Notice {
                code: "checkpoint.saved".to_owned(),
                message: "Runtime checkpoint saved".to_owned(),
            },
        ),
    ]
}

fn raw_stream(events: Vec<RuntimeEvent>) -> RuntimeEventStream {
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[tokio::test]
async fn all_delegated_phases_validate_and_replay_without_loss_or_reordering() {
    let expected = complete_replay();
    let normalized = normalize_runtime_event_replay(raw_stream(expected.clone()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<NormalizedRuntimeEvent>, _>>()
        .unwrap();

    assert_eq!(normalized.len(), expected.len());
    for (actual, expected) in normalized.iter().zip(&expected) {
        assert_eq!(actual.sequence(), expected.sequence());
        assert_eq!(actual.kind(), expected.kind());
    }
}

#[tokio::test]
async fn opaque_reasoning_activity_validates_and_replays_without_text() {
    let mut expected = complete_replay();
    expected[3] = event(
        3,
        RuntimeEventKind::ReasoningDelta {
            text: String::new(),
        },
    );
    let normalized = normalize_runtime_event_replay(raw_stream(expected.clone()))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<NormalizedRuntimeEvent>, _>>()
        .unwrap();
    assert_eq!(normalized[3].kind(), expected[3].kind());
}

#[test]
fn sequence_session_and_turn_lifecycle_fail_closed_with_stable_codes() {
    let cases = [
        (
            vec![event(1, RuntimeEventKind::SessionReady)],
            RuntimeEventViolationCode::SequenceGap,
        ),
        (
            vec![event(
                0,
                RuntimeEventKind::TurnStarted {
                    turn: turn("turn-1"),
                },
            )],
            RuntimeEventViolationCode::SessionReadyRequired,
        ),
        (
            vec![
                event(0, RuntimeEventKind::SessionReady),
                event(1, RuntimeEventKind::SessionReady),
            ],
            RuntimeEventViolationCode::DuplicateSessionReady,
        ),
        (
            vec![
                event(0, RuntimeEventKind::SessionReady),
                event(
                    1,
                    RuntimeEventKind::CommentaryDelta {
                        text: "orphan".to_owned(),
                    },
                ),
            ],
            RuntimeEventViolationCode::TurnRequired,
        ),
        (
            vec![
                event(0, RuntimeEventKind::SessionReady),
                event(
                    1,
                    RuntimeEventKind::TurnStarted {
                        turn: turn("turn-1"),
                    },
                ),
                event(
                    2,
                    RuntimeEventKind::TurnStarted {
                        turn: turn("turn-2"),
                    },
                ),
            ],
            RuntimeEventViolationCode::TurnAlreadyActive,
        ),
        (
            vec![
                event(0, RuntimeEventKind::SessionReady),
                event(
                    1,
                    RuntimeEventKind::TurnStarted {
                        turn: turn("turn-1"),
                    },
                ),
                event(
                    2,
                    RuntimeEventKind::FinalMessage {
                        text: "done".to_owned(),
                    },
                ),
                event(
                    3,
                    RuntimeEventKind::TurnFinished {
                        turn: turn("turn-2"),
                        reason: RuntimeFinishReason::Stop,
                    },
                ),
            ],
            RuntimeEventViolationCode::TurnMismatch,
        ),
    ];

    for (events, expected) in cases {
        let mut normalizer = RuntimeEventNormalizer::new();
        let error = events
            .into_iter()
            .find_map(|event| normalizer.push(event).err())
            .expect("case must fail");
        assert_eq!(error.code(), expected);
        assert!(!error.to_string().contains("turn-1"));
        assert!(!error.to_string().contains("turn-2"));
    }
}

#[test]
fn tool_and_request_correlation_rejects_duplicates_unknown_results_and_open_calls() {
    let call = CallId::from_raw("private-call-canary");
    let prefix = || {
        vec![
            event(0, RuntimeEventKind::SessionReady),
            event(
                1,
                RuntimeEventKind::TurnStarted {
                    turn: turn("turn-1"),
                },
            ),
        ]
    };

    let mut unknown_result = prefix();
    unknown_result.push(event(
        2,
        RuntimeEventKind::ToolResult {
            call_id: call.clone(),
            result: json!(null),
            is_error: true,
        },
    ));
    assert_case_code(unknown_result, RuntimeEventViolationCode::UnknownToolCall);

    let mut duplicate_call = prefix();
    duplicate_call.extend([
        event(
            2,
            RuntimeEventKind::ToolCall {
                call_id: call.clone(),
                name: "bash".to_owned(),
                arguments: json!({}),
            },
        ),
        event(
            3,
            RuntimeEventKind::ToolCall {
                call_id: call.clone(),
                name: "bash".to_owned(),
                arguments: json!({}),
            },
        ),
    ]);
    assert_case_code(duplicate_call, RuntimeEventViolationCode::DuplicateToolCall);

    let mut duplicate_result = prefix();
    duplicate_result.extend([
        event(
            2,
            RuntimeEventKind::ToolCall {
                call_id: call.clone(),
                name: "bash".to_owned(),
                arguments: json!({}),
            },
        ),
        event(
            3,
            RuntimeEventKind::ToolResult {
                call_id: call.clone(),
                result: json!({"ok": true}),
                is_error: false,
            },
        ),
        event(
            4,
            RuntimeEventKind::ToolResult {
                call_id: call.clone(),
                result: json!({"ok": true}),
                is_error: false,
            },
        ),
    ]);
    assert_case_code(
        duplicate_result,
        RuntimeEventViolationCode::DuplicateToolResult,
    );

    let mut open_call = prefix();
    open_call.extend([
        event(
            2,
            RuntimeEventKind::ToolCall {
                call_id: call,
                name: "bash".to_owned(),
                arguments: json!({}),
            },
        ),
        event(
            3,
            RuntimeEventKind::TurnFinished {
                turn: turn("turn-1"),
                reason: RuntimeFinishReason::Error,
            },
        ),
    ]);
    assert_case_code(open_call, RuntimeEventViolationCode::UnsettledToolCall);

    let mut duplicate_request = prefix();
    duplicate_request.extend([
        event(
            2,
            RuntimeEventKind::PermissionRequested {
                request_id: request("shared-request"),
                action: "Run command".to_owned(),
                detail: "Permission detail".to_owned(),
            },
        ),
        event(
            3,
            RuntimeEventKind::QuestionRequested {
                mode: heycode_core::QuestionMode::FreeText,
                progress: (1, 1),
                request_id: request("shared-request"),
                header: None,
                prompt: "Proceed?".to_owned(),
                choices: vec![],
                choice_descriptions: vec![],
            },
        ),
    ]);
    assert_case_code(
        duplicate_request,
        RuntimeEventViolationCode::DuplicateRequest,
    );
}

#[test]
fn final_and_turn_settlement_are_exact_and_stop_requires_a_final_message() {
    let prefix = || {
        vec![
            event(0, RuntimeEventKind::SessionReady),
            event(
                1,
                RuntimeEventKind::TurnStarted {
                    turn: turn("turn-1"),
                },
            ),
        ]
    };

    let mut duplicate_final = prefix();
    duplicate_final.extend([
        event(
            2,
            RuntimeEventKind::FinalMessage {
                text: "first".to_owned(),
            },
        ),
        event(
            3,
            RuntimeEventKind::FinalMessage {
                text: "second".to_owned(),
            },
        ),
    ]);
    assert_case_code(
        duplicate_final,
        RuntimeEventViolationCode::DuplicateFinalMessage,
    );

    let repeated_usage = vec![
        event(0, RuntimeEventKind::SessionReady),
        event(
            1,
            RuntimeEventKind::TurnStarted {
                turn: turn("turn-1"),
            },
        ),
        event(
            2,
            RuntimeEventKind::Usage {
                usage: TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                },
                context: None,
            },
        ),
        event(
            3,
            RuntimeEventKind::CommentaryDelta {
                text: "A second internal step follows".to_owned(),
            },
        ),
        event(
            4,
            RuntimeEventKind::Usage {
                usage: TokenUsage {
                    prompt_tokens: 14,
                    completion_tokens: 5,
                },
                context: None,
            },
        ),
        event(
            5,
            RuntimeEventKind::FinalMessage {
                text: "done".to_owned(),
            },
        ),
        event(
            6,
            RuntimeEventKind::Usage {
                usage: TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                },
                context: None,
            },
        ),
        event(
            7,
            RuntimeEventKind::TurnFinished {
                turn: turn("turn-1"),
                reason: RuntimeFinishReason::Stop,
            },
        ),
    ];
    let mut normalizer = RuntimeEventNormalizer::new();
    for event in repeated_usage {
        normalizer.push(event).unwrap();
    }
    normalizer.finish().unwrap();

    let mut after_final = prefix();
    after_final.extend([
        event(
            2,
            RuntimeEventKind::FinalMessage {
                text: "done".to_owned(),
            },
        ),
        event(
            3,
            RuntimeEventKind::CommentaryDelta {
                text: "too late".to_owned(),
            },
        ),
    ]);
    assert_case_code(after_final, RuntimeEventViolationCode::PhaseAfterFinal);

    let mut missing_final = prefix();
    missing_final.push(event(
        2,
        RuntimeEventKind::TurnFinished {
            turn: turn("turn-1"),
            reason: RuntimeFinishReason::Stop,
        },
    ));
    assert_case_code(
        missing_final,
        RuntimeEventViolationCode::MissingFinalMessage,
    );

    let interrupted = vec![
        event(0, RuntimeEventKind::SessionReady),
        event(
            1,
            RuntimeEventKind::TurnStarted {
                turn: turn("turn-1"),
            },
        ),
        event(
            2,
            RuntimeEventKind::TurnFinished {
                turn: turn("turn-1"),
                reason: RuntimeFinishReason::Cancelled,
            },
        ),
        event(
            3,
            RuntimeEventKind::TurnStarted {
                turn: turn("turn-2"),
            },
        ),
        event(
            4,
            RuntimeEventKind::TurnFinished {
                turn: turn("turn-2"),
                reason: RuntimeFinishReason::Error,
            },
        ),
    ];
    let mut normalizer = RuntimeEventNormalizer::new();
    for event in interrupted {
        normalizer.push(event).unwrap();
    }
    normalizer.finish().unwrap();
    normalizer.finish().unwrap();
}

#[test]
fn text_json_and_choice_bounds_reject_terminal_injection_and_oversized_payloads() {
    let prefix = || {
        vec![
            event(0, RuntimeEventKind::SessionReady),
            event(
                1,
                RuntimeEventKind::TurnStarted {
                    turn: turn("turn-1"),
                },
            ),
        ]
    };

    let mut terminal_escape = prefix();
    terminal_escape.push(event(
        2,
        RuntimeEventKind::CommentaryDelta {
            text: "safe\u{1b}[31mcanary".to_owned(),
        },
    ));
    assert_case_code(terminal_escape, RuntimeEventViolationCode::InvalidPayload);

    let mut oversized_final = prefix();
    oversized_final.push(event(
        2,
        RuntimeEventKind::FinalMessage {
            text: "x".repeat(1024 * 1024 + 1),
        },
    ));
    assert_case_code(oversized_final, RuntimeEventViolationCode::InvalidPayload);

    let mut oversized_json = prefix();
    oversized_json.push(event(
        2,
        RuntimeEventKind::ToolCall {
            call_id: CallId::from_raw("call-1"),
            name: "write".to_owned(),
            arguments: json!({"content": "x".repeat(1024 * 1024 + 1)}),
        },
    ));
    assert_case_code(oversized_json, RuntimeEventViolationCode::InvalidPayload);

    for arguments in [
        json!({"nested": {"value": "safe\u{1b}[31mcanary"}}),
        json!({"unsafe\u{1b}[31mkey": "canary"}),
    ] {
        let mut terminal_escape_in_json = prefix();
        terminal_escape_in_json.push(event(
            2,
            RuntimeEventKind::ToolCall {
                call_id: CallId::from_raw("call-1"),
                name: "write".to_owned(),
                arguments,
            },
        ));
        assert_case_code(
            terminal_escape_in_json,
            RuntimeEventViolationCode::InvalidPayload,
        );
    }

    let mut scalar_arguments = prefix();
    scalar_arguments.push(event(
        2,
        RuntimeEventKind::ToolCall {
            call_id: CallId::from_raw("call-1"),
            name: "write".to_owned(),
            arguments: json!("not an argument object"),
        },
    ));
    assert_case_code(scalar_arguments, RuntimeEventViolationCode::InvalidPayload);

    let mut duplicate_choices = prefix();
    duplicate_choices.push(event(
        2,
        RuntimeEventKind::QuestionRequested {
            mode: heycode_core::QuestionMode::SingleChoice,
            progress: (1, 1),
            request_id: request("question-1"),
            header: None,
            prompt: "Choose".to_owned(),
            choices: vec!["same".to_owned(), "same".to_owned()],
            choice_descriptions: vec![None, None],
        },
    ));
    assert_case_code(duplicate_choices, RuntimeEventViolationCode::InvalidPayload);

    let mut zero_context_window = prefix();
    zero_context_window.push(event(
        2,
        RuntimeEventKind::Usage {
            usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 0,
            },
            context: Some(heycode_runtime::RuntimeContextUsage {
                resolved_model: None,
                tokens: 1,
                context_window: 0,
            }),
        },
    ));
    assert_case_code(
        zero_context_window,
        RuntimeEventViolationCode::InvalidPayload,
    );
}

#[test]
fn event_debug_is_body_redacted_and_violation_diagnostics_never_echo_provider_data() {
    let private = "subscription-secret-canary";
    let raw = event(
        0,
        RuntimeEventKind::Notice {
            code: "invalid code".to_owned(),
            message: private.to_owned(),
        },
    );
    let debug = format!("{raw:?}");
    assert!(!debug.contains(private));

    let mut normalizer = RuntimeEventNormalizer::new();
    let error = normalizer.push(raw).unwrap_err();
    assert_eq!(error.code(), RuntimeEventViolationCode::InvalidPayload);
    assert!(!error.to_string().contains(private));
    assert!(!format!("{error:?}").contains(private));

    let later = normalizer
        .push(event(0, RuntimeEventKind::SessionReady))
        .unwrap_err();
    assert_eq!(later.code(), RuntimeEventViolationCode::NormalizerFailed);

    let mut normalizer = RuntimeEventNormalizer::new();
    normalizer
        .push(event(0, RuntimeEventKind::SessionReady))
        .unwrap();
    normalizer
        .push(event(
            1,
            RuntimeEventKind::TurnStarted {
                turn: turn("turn-private-canary"),
            },
        ))
        .unwrap();
    let normalized = normalizer
        .push(event(
            2,
            RuntimeEventKind::FinalMessage {
                text: private.to_owned(),
            },
        ))
        .unwrap();
    let normalized_debug = format!("{normalized:?}");
    assert_eq!(
        normalized_debug,
        "NormalizedRuntimeEvent { sequence: 2, phase: \"final_message\" }"
    );
    assert!(!normalized_debug.contains(private));
}

#[tokio::test]
async fn stream_emits_one_fixed_protocol_error_for_invalid_event_or_incomplete_replay() {
    let invalid = vec![
        event(0, RuntimeEventKind::SessionReady),
        event(
            2,
            RuntimeEventKind::TurnStarted {
                turn: turn("private-turn"),
            },
        ),
        event(
            3,
            RuntimeEventKind::Notice {
                code: "not-reached".to_owned(),
                message: "not reached".to_owned(),
            },
        ),
    ];
    let output = normalize_runtime_event_stream(raw_stream(invalid))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output.len(), 2);
    assert!(output[0].is_ok());
    let error = output[1].as_ref().unwrap_err();
    assert_eq!(error.code(), heycode_runtime::RuntimeErrorCode::Protocol);
    assert_eq!(error.to_string(), "runtime event protocol failed");

    let incomplete = vec![
        event(0, RuntimeEventKind::SessionReady),
        event(
            1,
            RuntimeEventKind::TurnStarted {
                turn: turn("private-turn"),
            },
        ),
    ];
    let output = normalize_runtime_event_replay(raw_stream(incomplete))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output.len(), 3);
    assert!(output[0].is_ok());
    assert!(output[1].is_ok());
    assert_eq!(
        output[2].as_ref().unwrap_err().code(),
        heycode_runtime::RuntimeErrorCode::Protocol
    );
}

#[tokio::test]
async fn unsolicited_live_eof_is_a_protocol_failure_even_between_settled_turns() {
    let output = normalize_runtime_event_stream(raw_stream(complete_replay()))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output.len(), complete_replay().len() + 1);
    assert!(output[..output.len() - 1].iter().all(Result::is_ok));
    assert_eq!(
        output.last().unwrap().as_ref().unwrap_err().code(),
        heycode_runtime::RuntimeErrorCode::Protocol
    );
}

#[tokio::test]
async fn upstream_failure_is_preserved_and_never_synthesizes_successful_settlement() {
    let private = "private upstream body";
    let upstream = heycode_runtime::RuntimeError::internal(private);
    let source: RuntimeEventStream = Box::pin(stream::iter([
        Ok(event(0, RuntimeEventKind::SessionReady)),
        Err(upstream.clone()),
    ]));
    let output = normalize_runtime_event_stream(source)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(output.len(), 2);
    assert!(output[0].is_ok());
    assert_eq!(output[1].as_ref().unwrap_err(), &upstream);
    assert!(
        !output[1]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains(private)
    );
}

#[test]
fn identity_tracking_is_bounded_and_fails_before_unbounded_session_growth() {
    let mut normalizer = RuntimeEventNormalizer::new();
    normalizer
        .push(event(0, RuntimeEventKind::SessionReady))
        .unwrap();
    let mut sequence = 1_u64;
    for index in 0..MAX_RUNTIME_EVENT_IDENTITIES {
        let turn_id = turn(&format!("turn-{index}"));
        normalizer
            .push(event(
                sequence,
                RuntimeEventKind::TurnStarted {
                    turn: turn_id.clone(),
                },
            ))
            .unwrap();
        sequence += 1;
        normalizer
            .push(event(
                sequence,
                RuntimeEventKind::TurnFinished {
                    turn: turn_id,
                    reason: RuntimeFinishReason::Cancelled,
                },
            ))
            .unwrap();
        sequence += 1;
    }

    let error = normalizer
        .push(event(
            sequence,
            RuntimeEventKind::TurnStarted {
                turn: turn("one-turn-too-many"),
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code(),
        RuntimeEventViolationCode::CorrelationCapacityExceeded
    );
}

fn assert_case_code(events: Vec<RuntimeEvent>, expected: RuntimeEventViolationCode) {
    let mut normalizer = RuntimeEventNormalizer::new();
    let error = events
        .into_iter()
        .find_map(|event| normalizer.push(event).err())
        .expect("case must fail");
    assert_eq!(error.code(), expected);
}
