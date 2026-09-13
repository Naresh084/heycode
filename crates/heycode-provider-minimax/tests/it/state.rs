//! PMM03: a replayed MiniMax assistant turn is the whole turn or nothing.

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_provider_minimax::{
    MiniMaxApiFamily, MiniMaxPlanId, MiniMaxProfile, MiniMaxReasoningGuarantee, MiniMaxRegion,
    MiniMaxStateDialect, MiniMaxStateError, MiniMaxStateRoute, MiniMaxTurnPart, PayAsYouGo,
    TokenPlan,
};
use std::collections::BTreeSet;

use serde_json::{Value, json};

const THINK_TAGS_TOOL_CALL: &str = include_str!("../fixtures/chat_think_tags_tool_call.json");
const THINK_TAGS_FINAL_ANSWER: &str = include_str!("../fixtures/chat_think_tags_final_answer.json");
const REASONING_SPLIT_TOOL_CALL: &str =
    include_str!("../fixtures/chat_reasoning_split_tool_call.json");
const ANTHROPIC_TOOL_USE: &str = include_str!("../fixtures/anthropic_thinking_tool_use.json");
const ANTHROPIC_UNSIGNED: &str = include_str!("../fixtures/anthropic_thinking_unsigned.json");

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap()
}

fn route(dialect: MiniMaxStateDialect) -> MiniMaxStateRoute<PayAsYouGo> {
    MiniMaxStateRoute::new(
        MiniMaxProfile::<PayAsYouGo>::international(),
        dialect,
        "MiniMax-M3",
        MiniMaxReasoningGuarantee::Always,
    )
    .unwrap()
}

fn optional(dialect: MiniMaxStateDialect) -> MiniMaxStateRoute<PayAsYouGo> {
    MiniMaxStateRoute::new(
        MiniMaxProfile::<PayAsYouGo>::international(),
        dialect,
        "MiniMax-M3",
        MiniMaxReasoningGuarantee::Optional,
    )
    .unwrap()
}

/// Every documented dialect and its fixture, so a new dialect cannot be added
/// without a fixture that proves it round-trips.
fn every_dialect() -> [(MiniMaxStateDialect, Value); 3] {
    [
        (
            MiniMaxStateDialect::ChatThinkTags,
            fixture(THINK_TAGS_TOOL_CALL),
        ),
        (
            MiniMaxStateDialect::ChatReasoningSplit,
            fixture(REASONING_SPLIT_TOOL_CALL),
        ),
        (
            MiniMaxStateDialect::AnthropicThinkingBlocks,
            fixture(ANTHROPIC_TOOL_USE),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Verbatim replay
// ---------------------------------------------------------------------------

#[test]
fn every_documented_dialect_replays_its_captured_turn_with_nothing_added_or_dropped() {
    for (dialect, message) in every_dialect() {
        let route = route(dialect);
        let state = route.capture(message.clone()).unwrap();
        let replayed = route.replay(&state).unwrap();
        assert_eq!(
            replayed, &message,
            "{dialect} did not replay its turn verbatim"
        );
    }
}

#[test]
fn capture_preserves_wire_fields_the_shared_chat_shape_does_not_name() {
    let message = fixture(THINK_TAGS_TOOL_CALL);
    let state = route(MiniMaxStateDialect::ChatThinkTags)
        .capture(message)
        .unwrap();
    // `index` on a tool call is MiniMax's documented response shape and is
    // absent from the OpenAI request shape a generic adapter reconstructs.
    assert_eq!(
        state.data()["tool_calls"][0]["index"],
        json!(0),
        "the documented tool-call `index` was dropped"
    );
}

#[test]
fn capture_preserves_reasoning_detail_fields_beyond_the_thinking_text() {
    let state = route(MiniMaxStateDialect::ChatReasoningSplit)
        .capture(fixture(REASONING_SPLIT_TOOL_CALL))
        .unwrap();
    let detail = &state.data()["reasoning_details"][0];
    assert_eq!(detail["id"], json!("reasoning-text-1"));
    assert_eq!(detail["format"], json!("MiniMax-response-v1"));
    assert_eq!(detail["index"], json!(0));
}

#[test]
fn capture_preserves_an_opaque_thinking_block_signature_it_cannot_interpret() {
    let state = route(MiniMaxStateDialect::AnthropicThinkingBlocks)
        .capture(fixture(ANTHROPIC_TOOL_USE))
        .unwrap();
    assert_eq!(
        state.data()["content"][0]["signature"],
        json!("EqQBCkYIBBgCKkDbUnQ1oTZ2Yw==")
    );
}

#[test]
fn an_anthropic_thinking_block_without_its_replay_signature_is_incomplete() {
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(fixture(ANTHROPIC_UNSIGNED)),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ContentBlock,
            field: "signature",
        })
    );
}

#[test]
fn an_answer_turn_that_calls_no_tool_replays_with_its_thinking_intact() {
    let route = route(MiniMaxStateDialect::ChatThinkTags);
    let message = fixture(THINK_TAGS_FINAL_ANSWER);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

// ---------------------------------------------------------------------------
// Route identity
// ---------------------------------------------------------------------------

#[test]
fn captured_state_carries_the_shared_vocabulary_identity_of_its_dialect() {
    for (dialect, message) in every_dialect() {
        let state = route(dialect).capture(message).unwrap();
        assert_eq!(state.provider(), MiniMaxPlanId::PayAsYouGo.registry_name());
        assert_eq!(state.model(), "MiniMax-M3");
        assert_eq!(state.protocol(), dialect.protocol());
        assert_eq!(state.kind(), dialect.state_kind());
        assert_eq!(state.schema_version(), 1);
    }
}

#[test]
fn each_dialect_maps_to_the_documented_protocol_and_state_kind() {
    assert_eq!(
        MiniMaxStateDialect::ChatThinkTags.protocol(),
        ProviderProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        MiniMaxStateDialect::ChatThinkTags.state_kind(),
        ProviderStateKind::ChatAssistantMessage
    );
    assert_eq!(
        MiniMaxStateDialect::ChatReasoningSplit.protocol(),
        ProviderProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        MiniMaxStateDialect::ChatReasoningSplit.state_kind(),
        ProviderStateKind::ChatAssistantMessage
    );
    assert_eq!(
        MiniMaxStateDialect::AnthropicThinkingBlocks.protocol(),
        ProviderProtocol::AnthropicMessages
    );
    assert_eq!(
        MiniMaxStateDialect::AnthropicThinkingBlocks.state_kind(),
        ProviderStateKind::AnthropicMessage
    );
}

#[test]
fn each_dialect_names_the_api_family_and_reasoning_split_value_that_produces_it() {
    assert_eq!(
        MiniMaxStateDialect::ChatThinkTags.family(),
        MiniMaxApiFamily::OpenAiCompatible
    );
    assert_eq!(
        MiniMaxStateDialect::ChatThinkTags.reasoning_split(),
        Some(false)
    );
    assert_eq!(
        MiniMaxStateDialect::ChatReasoningSplit.family(),
        MiniMaxApiFamily::OpenAiCompatible
    );
    assert_eq!(
        MiniMaxStateDialect::ChatReasoningSplit.reasoning_split(),
        Some(true)
    );
    assert_eq!(
        MiniMaxStateDialect::AnthropicThinkingBlocks.family(),
        MiniMaxApiFamily::AnthropicCompatible
    );
    assert_eq!(
        MiniMaxStateDialect::AnthropicThinkingBlocks.reasoning_split(),
        None
    );
}

#[test]
fn a_token_plan_route_refuses_state_recorded_against_the_pay_as_you_go_product() {
    let recorded = route(MiniMaxStateDialect::ChatThinkTags)
        .capture(fixture(THINK_TAGS_TOOL_CALL))
        .unwrap();
    let token_plan = MiniMaxStateRoute::new(
        MiniMaxProfile::<TokenPlan>::international(),
        MiniMaxStateDialect::ChatThinkTags,
        "MiniMax-M3",
        MiniMaxReasoningGuarantee::Always,
    )
    .unwrap();
    assert_eq!(
        token_plan.replay(&recorded),
        Err(MiniMaxStateError::ForeignRoute {
            recorded: MiniMaxPlanId::PayAsYouGo.registry_name().to_owned(),
            expected: MiniMaxPlanId::TokenPlan.registry_name(),
        })
    );
}

#[test]
fn a_route_refuses_complete_state_recorded_for_another_model() {
    let recorded = ProviderStateItem::new(
        MiniMaxPlanId::PayAsYouGo.registry_name(),
        "MiniMax-M2.7",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        fixture(THINK_TAGS_TOOL_CALL),
    )
    .unwrap();
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).replay(&recorded),
        Err(MiniMaxStateError::ForeignModel)
    );
}

#[test]
fn a_chat_route_refuses_state_recorded_in_the_anthropic_dialect() {
    let recorded = route(MiniMaxStateDialect::AnthropicThinkingBlocks)
        .capture(fixture(ANTHROPIC_TOOL_USE))
        .unwrap();
    assert!(matches!(
        route(MiniMaxStateDialect::ChatThinkTags).replay(&recorded),
        Err(MiniMaxStateError::ForeignShape { .. })
    ));
}

#[test]
fn a_route_needs_a_non_blank_trimmed_model_id() {
    for model in ["", "  ", " MiniMax-M3"] {
        assert_eq!(
            MiniMaxStateRoute::<PayAsYouGo>::new(
                MiniMaxProfile::international(),
                MiniMaxStateDialect::ChatThinkTags,
                model,
                MiniMaxReasoningGuarantee::Always,
            )
            .err(),
            Some(MiniMaxStateError::InvalidModel),
            "`{model}` was admitted as a model id"
        );
    }
}

// ---------------------------------------------------------------------------
// Interleaved-thinking completeness
// ---------------------------------------------------------------------------

#[test]
fn a_native_format_tool_call_turn_whose_think_block_was_stripped_is_refused() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = json!("");
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).capture(message),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::ChatThinkTags,
        })
    );
}

#[test]
fn a_split_format_tool_call_turn_that_dropped_reasoning_details_is_refused() {
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    message.as_object_mut().unwrap().remove("reasoning_details");
    assert_eq!(
        route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::ChatReasoningSplit,
        })
    );
}

#[test]
fn an_anthropic_tool_use_turn_that_dropped_its_thinking_block_is_refused() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    let blocks = message["content"].as_array().unwrap().clone();
    message["content"] = Value::Array(
        blocks
            .into_iter()
            .filter(|block| block["type"] != json!("thinking"))
            .collect(),
    );
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::AnthropicThinkingBlocks,
        })
    );
}

#[test]
fn an_empty_think_block_does_not_satisfy_the_interleaved_thinking_requirement() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = json!("<think>   </think>");
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).capture(message),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::ChatThinkTags,
        })
    );
}

#[test]
fn an_empty_reasoning_details_array_does_not_satisfy_the_requirement() {
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    message["reasoning_details"] = json!([]);
    assert_eq!(
        route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::ChatReasoningSplit,
        })
    );
}

#[test]
fn a_turn_that_calls_no_tool_may_carry_no_reasoning_at_all() {
    let message = json!({"role":"assistant","content":"It is 24℃ and sunny."});
    let route = route(MiniMaxStateDialect::ChatThinkTags);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

#[test]
fn a_route_without_a_reasoning_presence_guarantee_admits_a_tool_call_without_reasoning() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = Value::Null;
    let route = optional(MiniMaxStateDialect::ChatThinkTags);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

#[test]
fn an_optional_reasoning_route_still_refuses_a_truncated_thinking_chain() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = json!("<think>I was cut off mid");
    assert_eq!(
        optional(MiniMaxStateDialect::ChatThinkTags).capture(message),
        Err(MiniMaxStateError::UnbalancedThinking)
    );
}

// ---------------------------------------------------------------------------
// Truncation and malformed parts
// ---------------------------------------------------------------------------

#[test]
fn an_unbalanced_think_segment_is_refused_in_every_arrangement() {
    for content in [
        "<think>never closed",
        "</think>closed without opening",
        "<think>outer<think>nested</think></think>",
        "<think>first</think>tail<think>second",
    ] {
        let mut message = fixture(THINK_TAGS_TOOL_CALL);
        message["content"] = json!(content);
        assert_eq!(
            route(MiniMaxStateDialect::ChatThinkTags).capture(message),
            Err(MiniMaxStateError::UnbalancedThinking),
            "`{content}` was admitted as a complete thinking chain"
        );
    }
}

#[test]
fn a_reasoning_text_detail_with_no_text_is_refused_as_an_incomplete_chain() {
    for text in [json!(""), json!("   "), Value::Null, json!(7)] {
        let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
        message["reasoning_details"][0]["text"] = text.clone();
        assert_eq!(
            route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
            Err(MiniMaxStateError::Incomplete {
                part: MiniMaxTurnPart::ReasoningDetail,
                field: "text",
            }),
            "a `reasoning.text` detail carrying {text} was admitted"
        );
    }
}

#[test]
fn a_reasoning_detail_of_an_unknown_type_is_kept_verbatim_rather_than_dissected() {
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    message["reasoning_details"] = json!([
        {"type":"reasoning.encrypted","id":"reasoning-encrypted-1","data":"b3BhcXVl"}
    ]);
    let route = route(MiniMaxStateDialect::ChatReasoningSplit);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

#[test]
fn a_reasoning_detail_with_no_type_is_refused() {
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    message["reasoning_details"][0]
        .as_object_mut()
        .unwrap()
        .remove("type");
    assert_eq!(
        route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ReasoningDetail,
            field: "type",
        })
    );
}

#[test]
fn a_reasoning_content_field_that_carries_no_thinking_is_refused() {
    for value in [json!(""), json!("  "), Value::Null, json!(["split"])] {
        let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
        message["reasoning_content"] = value.clone();
        assert_eq!(
            route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
            Err(MiniMaxStateError::Incomplete {
                part: MiniMaxTurnPart::ReasoningContent,
                field: "reasoning_content",
            }),
            "a `reasoning_content` of {value} was admitted"
        );
    }
}

#[test]
fn a_turn_with_no_reasoning_content_field_at_all_is_still_admitted() {
    // MiniMax's own `reasoning_split` samples show `reasoning_details` without
    // a top-level `reasoning_content`, so its absence is not a dropped chain.
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    message.as_object_mut().unwrap().remove("reasoning_content");
    let route = route(MiniMaxStateDialect::ChatReasoningSplit);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

#[test]
fn a_tool_call_missing_any_part_needed_to_replay_it_is_refused() {
    let cases: [(&str, Value, &'static str); 5] = [
        ("/id", Value::Null, "id"),
        ("/type", json!("custom"), "type"),
        ("/function/name", json!(""), "name"),
        ("/function/arguments", Value::Null, "arguments"),
        ("/function", Value::Null, "function"),
    ];
    for (pointer, replacement, field) in cases {
        let mut message = fixture(THINK_TAGS_TOOL_CALL);
        let target = message
            .pointer_mut(&format!("/tool_calls/0{pointer}"))
            .unwrap();
        *target = replacement;
        assert_eq!(
            route(MiniMaxStateDialect::ChatThinkTags).capture(message),
            Err(MiniMaxStateError::Incomplete {
                part: MiniMaxTurnPart::ToolCall,
                field,
            }),
            "a tool call with a broken `{pointer}` was admitted"
        );
    }
}

#[test]
fn duplicate_chat_tool_call_ids_are_not_a_complete_replayable_turn() {
    let mut message = fixture(REASONING_SPLIT_TOOL_CALL);
    let duplicate = message["tool_calls"][0].clone();
    message["tool_calls"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert_eq!(
        route(MiniMaxStateDialect::ChatReasoningSplit).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ToolCall,
            field: "id",
        })
    );
}

#[test]
fn duplicate_anthropic_tool_use_ids_are_not_a_complete_replayable_turn() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    let duplicate = message["content"][2].clone();
    message["content"].as_array_mut().unwrap().push(duplicate);
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ContentBlock,
            field: "id",
        })
    );
}

#[test]
fn an_anthropic_tool_use_block_missing_any_replay_part_is_refused() {
    let cases: [(&str, Value, &'static str); 3] = [
        ("id", Value::Null, "id"),
        ("name", json!(""), "name"),
        ("input", json!("not-an-object"), "input"),
    ];
    for (key, replacement, field) in cases {
        let mut message = fixture(ANTHROPIC_TOOL_USE);
        message["content"][2][key] = replacement;
        assert_eq!(
            route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
            Err(MiniMaxStateError::Incomplete {
                part: MiniMaxTurnPart::ContentBlock,
                field,
            }),
            "a `tool_use` block with a broken `{key}` was admitted"
        );
    }
}

#[test]
fn an_anthropic_thinking_block_with_a_blank_signature_is_refused() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    message["content"][0]["signature"] = json!("");
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ContentBlock,
            field: "signature",
        })
    );
}

#[test]
fn an_anthropic_thinking_block_with_no_thinking_text_is_refused() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    message["content"][0]["thinking"] = json!("   ");
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ContentBlock,
            field: "thinking",
        })
    );
}

#[test]
fn an_anthropic_text_block_that_lost_its_text_is_refused() {
    for value in [Value::Null, json!(7)] {
        let mut message = fixture(ANTHROPIC_TOOL_USE);
        message["content"][1]["text"] = value.clone();
        assert_eq!(
            route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
            Err(MiniMaxStateError::Incomplete {
                part: MiniMaxTurnPart::ContentBlock,
                field: "text",
            }),
            "a `text` block carrying {value} was admitted"
        );
    }
}

#[test]
fn an_anthropic_block_type_minimax_has_not_documented_is_kept_verbatim() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    message["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"redacted_thinking","data":"b3BhcXVl"}));
    let route = route(MiniMaxStateDialect::AnthropicThinkingBlocks);
    let state = route.capture(message.clone()).unwrap();
    assert_eq!(route.replay(&state).unwrap(), &message);
}

#[test]
fn an_anthropic_content_block_with_no_type_is_refused() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    message["content"][1]["type"] = json!("  ");
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ContentBlock,
            field: "type",
        })
    );
}

#[test]
fn chat_content_that_is_neither_a_string_nor_null_is_refused() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = json!([{"type": "text", "text": "parts are not MiniMax's shape"}]);
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::Content,
            field: "content",
        })
    );
}

#[test]
fn anthropic_content_that_is_not_a_block_array_is_refused() {
    let mut message = fixture(ANTHROPIC_TOOL_USE);
    message["content"] = json!("a string is the chat shape, not the messages shape");
    assert_eq!(
        route(MiniMaxStateDialect::AnthropicThinkingBlocks).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::Content,
            field: "content",
        })
    );
}

#[test]
fn a_tool_calls_field_that_is_not_an_array_is_refused() {
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["tool_calls"] = json!({"id": "call_1"});
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).capture(message),
        Err(MiniMaxStateError::Incomplete {
            part: MiniMaxTurnPart::ToolCall,
            field: "tool_calls",
        })
    );
}

// ---------------------------------------------------------------------------
// Dialect confusion
// ---------------------------------------------------------------------------

#[test]
fn a_native_format_route_refuses_a_turn_carrying_split_format_reasoning_fields() {
    for field in ["reasoning_content", "reasoning_details"] {
        let mut message = fixture(THINK_TAGS_TOOL_CALL);
        message[field] = json!(if field == "reasoning_details" {
            json!([{"type":"reasoning.text","text":"split"}])
        } else {
            json!("split")
        });
        assert_eq!(
            route(MiniMaxStateDialect::ChatThinkTags).capture(message),
            Err(MiniMaxStateError::DialectMismatch {
                dialect: MiniMaxStateDialect::ChatThinkTags,
                field,
            }),
            "`{field}` was admitted on the native-format route"
        );
    }
}

#[test]
fn a_split_format_route_refuses_native_think_tags_even_without_a_tool_call() {
    assert_eq!(
        optional(MiniMaxStateDialect::ChatReasoningSplit).capture(fixture(THINK_TAGS_FINAL_ANSWER)),
        Err(MiniMaxStateError::DialectMismatch {
            dialect: MiniMaxStateDialect::ChatReasoningSplit,
            field: "content",
        })
    );
}

#[test]
fn a_turn_that_is_not_an_assistant_message_is_refused_in_every_dialect() {
    for (dialect, _) in every_dialect() {
        for message in [
            json!({"role":"user","content":"hello"}),
            json!({"content":"no role at all"}),
            json!("not an object"),
        ] {
            assert_eq!(
                route(dialect).capture(message.clone()),
                Err(MiniMaxStateError::NotAssistantMessage { dialect }),
                "{dialect} admitted {message}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Replay re-checks a stored item rather than trusting it
// ---------------------------------------------------------------------------

#[test]
fn replay_refuses_a_stored_turn_an_older_build_wrote_without_its_reasoning() {
    // The shared vocabulary accepts any `role: assistant` object, so a log
    // written before PMM03 can hold a tool-call turn with no thinking at all.
    let stripped = ProviderStateItem::new(
        MiniMaxPlanId::PayAsYouGo.registry_name(),
        "MiniMax-M3",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        json!({
            "role":"assistant",
            "content":null,
            "tool_calls":[{"id":"call_1","type":"function",
                "function":{"name":"get_weather","arguments":"{}"}}]
        }),
    )
    .unwrap();
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).replay(&stripped),
        Err(MiniMaxStateError::MissingReasoning {
            dialect: MiniMaxStateDialect::ChatThinkTags,
        })
    );
}

#[test]
fn replay_refuses_a_stored_turn_whose_thinking_was_truncated_after_capture() {
    let truncated = ProviderStateItem::new(
        MiniMaxPlanId::PayAsYouGo.registry_name(),
        "MiniMax-M3",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        json!({"role":"assistant","content":"<think>cut off"}),
    )
    .unwrap();
    assert_eq!(
        route(MiniMaxStateDialect::ChatThinkTags).replay(&truncated),
        Err(MiniMaxStateError::UnbalancedThinking)
    );
}

#[test]
fn a_captured_turn_survives_a_serialization_round_trip_through_the_session_log() {
    for (dialect, message) in every_dialect() {
        let route = route(dialect);
        let state = route.capture(message.clone()).unwrap();
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: ProviderStateItem = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            route.replay(&decoded).unwrap(),
            &message,
            "{dialect} did not survive the log"
        );
    }
}

// ---------------------------------------------------------------------------
// Route metadata
// ---------------------------------------------------------------------------

#[test]
fn a_route_reports_the_product_region_dialect_model_and_guarantee_it_was_built_for() {
    let route = optional(MiniMaxStateDialect::ChatReasoningSplit);
    assert_eq!(route.plan(), MiniMaxPlanId::PayAsYouGo);
    assert_eq!(route.region(), MiniMaxRegion::International);
    assert_eq!(route.dialect(), MiniMaxStateDialect::ChatReasoningSplit);
    assert_eq!(route.model(), "MiniMax-M3");
    assert_eq!(
        route.reasoning_guarantee(),
        MiniMaxReasoningGuarantee::Optional
    );
}

#[test]
fn a_route_debug_names_the_product_and_deployment_that_produced_its_turns() {
    let mainland = MiniMaxStateRoute::<TokenPlan>::new(
        MiniMaxProfile::new(MiniMaxRegion::MainlandChina),
        MiniMaxStateDialect::AnthropicThinkingBlocks,
        "MiniMax-M2",
        MiniMaxReasoningGuarantee::Always,
    )
    .unwrap();
    let rendered = format!("{mainland:?}");
    for expected in [
        MiniMaxPlanId::TokenPlan.registry_name(),
        "MainlandChina",
        "MiniMax-M2",
        "Always",
    ] {
        assert!(
            rendered.contains(expected),
            "`{expected}` missing: {rendered}"
        );
    }
    // The pay-as-you-go registry name is a prefix of the Token Plan one, so a
    // Debug that named the wrong product would still contain `minimax`.
    assert!(
        !rendered.contains(&format!(
            "\"{}\"",
            MiniMaxPlanId::PayAsYouGo.registry_name()
        )),
        "{rendered}"
    );
}

#[test]
fn every_dialect_has_a_distinct_label_that_the_refusals_naming_it_carry() {
    assert_eq!(MiniMaxStateDialect::ALL.len(), 3);
    let mut labels = BTreeSet::new();
    for dialect in MiniMaxStateDialect::ALL {
        let label = dialect.label();
        assert_eq!(dialect.to_string(), label);
        assert!(labels.insert(label), "`{label}` is used by two dialects");
        for rendered in [
            MiniMaxStateError::MissingReasoning { dialect }.to_string(),
            MiniMaxStateError::NotAssistantMessage { dialect }.to_string(),
        ] {
            assert!(rendered.contains(label), "{rendered}");
        }
    }
}

#[test]
fn every_turn_part_has_a_distinct_label_that_its_incompleteness_refusal_carries() {
    let parts = [
        MiniMaxTurnPart::Content,
        MiniMaxTurnPart::ToolCall,
        MiniMaxTurnPart::ReasoningContent,
        MiniMaxTurnPart::ReasoningDetail,
        MiniMaxTurnPart::ContentBlock,
    ];
    let mut labels = BTreeSet::new();
    for part in parts {
        let label = part.label();
        assert_eq!(part.to_string(), label);
        assert!(labels.insert(label), "`{label}` is used by two parts");
        let rendered = MiniMaxStateError::Incomplete {
            part,
            field: "somewhere",
        }
        .to_string();
        assert!(rendered.contains(label), "{rendered}");
        assert!(rendered.contains("somewhere"), "{rendered}");
    }
}

#[test]
fn every_refusal_names_the_minimax_fact_it_enforces_without_quoting_the_turn() {
    let secret = "shibboleth-that-must-not-leak";
    let mut message = fixture(THINK_TAGS_TOOL_CALL);
    message["content"] = json!(format!("<think>{secret}"));
    let error = route(MiniMaxStateDialect::ChatThinkTags)
        .capture(message)
        .unwrap_err();
    let rendered = error.to_string();
    assert!(!rendered.contains(secret), "{rendered}");
    assert!(rendered.contains("think"), "{rendered}");
}
