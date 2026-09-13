//! C15 — a real 1,000-turn append/reopen/projection compaction stress.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind, RequestId};
use heycode_session::{
    ProjectedInput, RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, Session, SessionCreationMetadata,
    SessionEventKind, SessionSource, TurnEndReason, project_inputs_for_route, project_requests,
};

const PROVIDER: &str = "openai";
const MODEL: &str = "gpt-stress";
const TURNS: u64 = 1_000;
const KEEP_TURNS: usize = 10;

fn header() -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        PROVIDER,
        MODEL,
        ProviderProtocol::OpenAiResponses,
        RequestTargetSnapshot::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::None,
        Some("stress system".to_owned()),
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
            max_output_tokens: Some(1_024),
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

fn response_message(turn: u64) -> ProviderStateItem {
    ProviderStateItem::new(
        PROVIDER,
        MODEL,
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "id":format!("msg_{turn}"),
            "type":"message",
            "role":"assistant",
            "phase":"final_answer",
            "content":[{"type":"output_text","text":format!("answer {turn}")}]
        }),
    )
    .unwrap()
}

fn native_checkpoint(turn: u64) -> ProviderStateItem {
    ProviderStateItem::new(
        PROVIDER,
        MODEL,
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        serde_json::json!({
            "id":format!("cmp_{turn}"),
            "type":"compaction",
            "encrypted_content":format!("opaque-{turn}")
        }),
    )
    .unwrap()
}

fn replacement_boundary(session: &Session) -> u64 {
    let retained_start = session
        .events()
        .iter()
        .rev()
        .filter(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        .nth(KEEP_TURNS - 1)
        .expect("ten complete turns exist")
        .seq;
    retained_start
        .checked_sub(1)
        .expect("the prefix is nonempty")
}

#[test]
fn one_thousand_turns_survive_repeated_portable_and_native_compaction() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create_with_metadata(
        root.path(),
        SessionCreationMetadata::new(
            Some(std::path::PathBuf::from("/work/stress")),
            Some("native".to_owned()),
            SessionSource::Headless,
        )
        .unwrap(),
    )
    .unwrap();
    let mut compaction_index = 0_u64;
    let mut original_events = Vec::new();
    for turn in 0..TURNS {
        let request_id = RequestId::from_raw(format!("req_{turn}"));
        session
            .append(SessionEventKind::UserMessage {
                text: format!("question {turn}"),
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnStart { turn })
            .unwrap();
        session
            .append(SessionEventKind::StepStart { turn, step: 0 })
            .unwrap();
        session
            .append(SessionEventKind::RequestHeader {
                turn,
                step: 0,
                request_id: request_id.clone(),
                header: Box::new(header()),
            })
            .unwrap();
        session
            .append(SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: context(),
            })
            .unwrap();
        session
            .append(SessionEventKind::AssistantProviderItem {
                turn,
                step: 0,
                request_id,
                output_index: 0,
                item: Box::new(response_message(turn)),
            })
            .unwrap();
        session
            .append(SessionEventKind::AssistantMessage {
                turn,
                step: 0,
                content: format!("answer {turn}"),
                reasoning: None,
                tool_calls: None,
                usage: None,
            })
            .unwrap();
        session
            .append(SessionEventKind::StepEnd { turn, step: 0 })
            .unwrap();
        session
            .append(SessionEventKind::TurnEnd {
                turn,
                reason: TurnEndReason::Stop,
            })
            .unwrap();

        // Capture each newly appended original before any compaction can hide it
        // from the model projection. Later compactions must not alter this archive.
        original_events.extend(
            session.events()[original_events.len()..]
                .iter()
                .map(|event| serde_json::to_value(event).unwrap()),
        );
        if (turn + 1) % 100 == 0 {
            let replaced_upto_seq = replacement_boundary(&session);
            let final_compaction = turn + 1 == TURNS;
            if compaction_index.is_multiple_of(2) || final_compaction {
                session
                    .append(SessionEventKind::NativeCompactionApplied {
                        strategy: "provider-native".to_owned(),
                        replaced_upto_seq,
                        items: vec![native_checkpoint(turn)],
                        usage: None,
                    })
                    .unwrap();
            } else {
                session
                    .append(SessionEventKind::CompactionApplied {
                        summary: format!("portable checkpoint through turn {turn}"),
                        replaced_upto_seq,
                    })
                    .unwrap();
            }
            compaction_index += 1;
        }
    }

    original_events.extend(
        session.events()[original_events.len()..]
            .iter()
            .map(|event| serde_json::to_value(event).unwrap()),
    );
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(directory).unwrap();
    assert_eq!(
        reopened
            .events()
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>(),
        original_events,
        "compaction and reopen must preserve every original event and content identity"
    );
    assert_eq!(
        reopened
            .events()
            .iter()
            .enumerate()
            .find(|(index, event)| event.seq != *index as u64),
        None,
        "logical sequence must remain contiguous after reopen"
    );
    assert_eq!(
        project_requests(reopened.events()).unwrap().len(),
        TURNS as usize
    );

    let exact = project_inputs_for_route(
        reopened.events(),
        PROVIDER,
        MODEL,
        ProviderProtocol::OpenAiResponses,
    )
    .unwrap();
    let state_ids = exact
        .iter()
        .filter_map(|input| match input {
            ProjectedInput::ProviderState(item) => {
                item.data().get("id").and_then(serde_json::Value::as_str)
            }
            ProjectedInput::Message(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(state_ids.first().copied(), Some("cmp_999"));
    for turn in 990..TURNS {
        assert!(state_ids.contains(&format!("msg_{turn}").as_str()));
    }

    let incompatible = project_inputs_for_route(
        reopened.events(),
        "anthropic",
        "claude-stress",
        ProviderProtocol::AnthropicMessages,
    )
    .unwrap();
    assert!(
        incompatible
            .iter()
            .all(|input| matches!(input, ProjectedInput::Message(_))),
        "opaque OpenAI state must never cross into Anthropic input"
    );
    assert!(incompatible.iter().any(|input| matches!(
        input,
        ProjectedInput::Message(message) if message.content == "question 999"
    )));
}
