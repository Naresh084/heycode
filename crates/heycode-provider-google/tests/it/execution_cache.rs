//! PGCP06 provider-owned code-execution events and context-cache usage.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{Plugin, ServerToolOutcome, compose};
use heycode_llm::InferenceEvent;
use heycode_provider_google::{
    CacheMetadataError, CodeExecutionError, CodeExecutionProjector, CodeExecutionRequest,
    GOOGLE_CODE_EXECUTION_IMPLEMENTATION, GOOGLE_CODE_EXECUTION_OPTION_KIND,
    GOOGLE_CODE_EXECUTION_TOOL_NAME, GOOGLE_CONTEXT_CACHE_OPTION_KIND,
    GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL, GOOGLE_VERTEX_PROVIDER, GeminiCacheMode,
    GeminiCacheRequest, GeminiCacheUsage, GeminiModality,
    google_code_execution_native_tools_plugin,
};

#[test]
fn the_code_execution_request_uses_the_generate_content_tool_shape() {
    let request = CodeExecutionRequest::new();
    assert_eq!(
        request.tool_entry(),
        serde_json::json!({ "codeExecution": {} })
    );
    let option = request.provider_option().unwrap();
    assert_eq!(option.provider(), "google");
    assert_eq!(option.kind(), GOOGLE_CODE_EXECUTION_OPTION_KIND);
    assert_eq!(
        option.data(),
        &serde_json::json!({ "tool": { "codeExecution": {} } })
    );
}

#[test]
fn one_code_part_and_its_result_normalize_to_a_correlated_pair() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let call = projector
        .observe_part(
            2,
            &serde_json::json!({
                "executableCode": {
                    "id": "code_1",
                    "language": "PYTHON",
                    "code": "print(42)",
                }
            }),
        )
        .unwrap();
    assert_eq!(call.len(), 1);
    let InferenceEvent::ServerToolCall { output_index, call } = &call[0] else {
        panic!("executable code must normalize to one server-tool call");
    };
    assert_eq!(*output_index, 2);
    assert_eq!(call.id().as_str(), "resp_1/code/0");
    assert_eq!(call.logical(), GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL);
    assert_eq!(call.provider_name(), GOOGLE_CODE_EXECUTION_TOOL_NAME);
    assert_eq!(
        call.input(),
        &serde_json::json!({
            "language": "python",
            "code": "print(42)",
            "provider_id": "code_1"
        })
    );

    let result = projector
        .observe_part(
            3,
            &serde_json::json!({
                "codeExecutionResult": {
                    "id": "code_1",
                    "outcome": "OUTCOME_OK",
                    "output": "42\n",
                }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolResult {
        output_index,
        result,
    } = &result[0]
    else {
        panic!("code result must normalize to one server-tool result");
    };
    assert_eq!(*output_index, 3);
    assert_eq!(result.call_id(), call.id());
    assert_eq!(result.outcome(), ServerToolOutcome::Success);
    assert_eq!(result.output_count(), None);
    assert!(result.sources().is_empty());
    projector.finish().unwrap();
}

#[test]
fn provider_reported_failure_and_deadline_are_distinct_safe_codes() {
    for (outcome, expected) in [
        ("OUTCOME_FAILED", "execution_failed"),
        ("OUTCOME_DEADLINE_EXCEEDED", "deadline_exceeded"),
    ] {
        let mut projector =
            CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
        projector
            .observe_part(
                0,
                &serde_json::json!({
                    "executableCode": { "language": "PYTHON", "code": "raise Exception()" }
                }),
            )
            .unwrap();
        let events = projector
            .observe_part(
                1,
                &serde_json::json!({
                    "codeExecutionResult": { "outcome": outcome, "output": "provider body" }
                }),
            )
            .unwrap();
        let InferenceEvent::ServerToolResult { result, .. } = &events[0] else {
            panic!("a provider outcome must normalize to a result");
        };
        assert_eq!(result.outcome(), ServerToolOutcome::Error);
        assert_eq!(result.error_code(), Some(expected));
        assert!(!format!("{result:?}").contains("provider body"));
        projector.finish().unwrap();
    }
}

#[test]
fn unnamed_code_and_result_pair_by_order_without_inventing_an_id() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let call = projector
        .observe_part(
            0,
            &serde_json::json!({
                "executableCode": { "language": "PYTHON", "code": "print(1)" }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolCall { call, .. } = &call[0] else {
        panic!("expected call");
    };
    assert_eq!(call.id().as_str(), "resp_1/code/0");
    let result = projector
        .observe_part(
            1,
            &serde_json::json!({
                "codeExecutionResult": { "outcome": "OUTCOME_OK" }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolResult { result, .. } = &result[0] else {
        panic!("expected result");
    };
    assert_eq!(result.call_id(), call.id());
    projector.finish().unwrap();
}

#[test]
fn an_oversized_code_body_is_disclosed_as_omitted_in_the_normalized_plane() {
    let code = "x".repeat(16 * 1024 + 1);
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let events = projector
        .observe_part(
            0,
            &serde_json::json!({
                "executableCode": { "language": "PYTHON", "code": code }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolCall { call, .. } = &events[0] else {
        panic!("expected call");
    };
    assert_eq!(
        call.input(),
        &serde_json::json!({
            "language": "python",
            "code_omitted": true,
            "code_bytes": 16 * 1024 + 1,
        })
    );
    assert!(!format!("{projector:?}").contains(&"x".repeat(64)));
}

#[test]
fn malformed_or_unsolicited_execution_never_becomes_a_durable_event() {
    let part = serde_json::json!({
        "executableCode": { "language": "PYTHON", "code": "print(1)" }
    });
    let mut unsolicited = CodeExecutionProjector::new(None, "resp_1").unwrap();
    assert_eq!(
        unsolicited.observe_part(0, &part).unwrap_err(),
        CodeExecutionError::Unsolicited
    );

    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    assert!(matches!(
        projector.observe_part(
            0,
            &serde_json::json!({
                "executableCode": { "language": "JAVASCRIPT", "code": "1" }
            })
        ),
        Err(CodeExecutionError::UnsupportedLanguage)
    ));
    assert!(matches!(
        projector.observe_part(
            0,
            &serde_json::json!({
                "codeExecutionResult": { "outcome": "OUTCOME_OK" }
            })
        ),
        Err(CodeExecutionError::OrphanResult)
    ));

    assert_eq!(
        projector
            .observe_part(
                0,
                &serde_json::json!({
                    "executableCode": {
                        "id": "unsafe\ncontrol",
                        "language": "PYTHON",
                        "code": "print(1)"
                    }
                })
            )
            .unwrap_err(),
        CodeExecutionError::InvalidProviderId
    );
}

#[test]
fn duplicate_ids_unknown_results_and_unsettled_calls_fail_loud() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let code = serde_json::json!({
        "executableCode": { "id": "same", "language": "PYTHON", "code": "print(1)" }
    });
    projector.observe_part(0, &code).unwrap();
    assert_eq!(
        projector.observe_part(1, &code).unwrap_err(),
        CodeExecutionError::DuplicateId
    );
    assert_eq!(
        projector
            .observe_part(
                2,
                &serde_json::json!({
                    "codeExecutionResult": { "id": "other", "outcome": "OUTCOME_OK" }
                })
            )
            .unwrap_err(),
        CodeExecutionError::OrphanResult
    );
    assert_eq!(
        projector.finish().unwrap_err(),
        CodeExecutionError::UnsettledCall
    );
}

#[test]
fn a_provider_id_stays_used_after_its_result_settles() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let code = serde_json::json!({
        "executableCode": { "id": "same", "language": "PYTHON", "code": "print(1)" }
    });
    projector.observe_part(0, &code).unwrap();
    projector
        .observe_part(
            1,
            &serde_json::json!({
                "codeExecutionResult": { "id": "same", "outcome": "OUTCOME_OK" }
            }),
        )
        .unwrap();
    assert_eq!(
        projector.observe_part(2, &code).unwrap_err(),
        CodeExecutionError::DuplicateId
    );
}

#[test]
fn provider_ids_cannot_collide_with_generated_call_ordinals() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let unnamed = projector
        .observe_part(
            0,
            &serde_json::json!({
                "executableCode": { "language": "PYTHON", "code": "print(0)" }
            }),
        )
        .unwrap();
    let named = projector
        .observe_part(
            1,
            &serde_json::json!({
                "executableCode": { "id": "0", "language": "PYTHON", "code": "print(1)" }
            }),
        )
        .unwrap();
    let (
        InferenceEvent::ServerToolCall { call: unnamed, .. },
        InferenceEvent::ServerToolCall { call: named, .. },
    ) = (&unnamed[0], &named[0])
    else {
        panic!("both parts must be calls");
    };
    assert_eq!(unnamed.id().as_str(), "resp_1/code/0");
    assert_eq!(named.id().as_str(), "resp_1/code/1");
    assert_ne!(unnamed.id(), named.id());
    assert_eq!(named.input()["provider_id"], "0");
}

#[test]
fn opaque_provider_ids_are_correlated_without_becoming_call_ids() {
    // The current discovery schema publishes `id: string` with no character
    // pattern. It is correlation data, not a URL or a heycode CallId, so safely
    // bounded punctuation must not be rejected by an invented charset rule.
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    let calls = projector
        .observe_part(
            0,
            &serde_json::json!({
                "executableCode": {
                    "id": "provider/opaque:id",
                    "language": "PYTHON",
                    "code": "print(1)"
                }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolCall { call, .. } = &calls[0] else {
        panic!("expected call");
    };
    assert_eq!(call.id().as_str(), "resp_1/code/0");
    assert_eq!(call.input()["provider_id"], "provider/opaque:id");

    let results = projector
        .observe_part(
            1,
            &serde_json::json!({
                "codeExecutionResult": {
                    "id": "provider/opaque:id",
                    "outcome": "OUTCOME_OK"
                }
            }),
        )
        .unwrap();
    let InferenceEvent::ServerToolResult { result, .. } = &results[0] else {
        panic!("expected result");
    };
    assert_eq!(result.call_id(), call.id());
    projector.finish().unwrap();
}

#[test]
fn an_unknown_outcome_does_not_settle_the_pending_call() {
    let mut projector =
        CodeExecutionProjector::new(Some(CodeExecutionRequest::new()), "resp_1").unwrap();
    projector
        .observe_part(
            0,
            &serde_json::json!({
                "executableCode": { "language": "PYTHON", "code": "print(1)" }
            }),
        )
        .unwrap();
    assert_eq!(
        projector
            .observe_part(
                1,
                &serde_json::json!({
                    "codeExecutionResult": { "outcome": "OUTCOME_FUTURE" }
                })
            )
            .unwrap_err(),
        CodeExecutionError::UnsupportedOutcome
    );
    assert_eq!(
        projector.finish().unwrap_err(),
        CodeExecutionError::UnsettledCall
    );
}

#[test]
fn the_code_execution_native_candidate_is_google_route_specific() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_native_tools::native_tools_plugin(),
        google_code_execution_native_tools_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("google").unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].logical(), GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL);
    assert_eq!(
        routes[0].implementation(),
        GOOGLE_CODE_EXECUTION_IMPLEMENTATION
    );
    assert_eq!(routes[0].provider(), Some("google"));
    assert!(registry.resolve("anthropic").unwrap().is_empty());
}

#[test]
fn implicit_cache_mode_sends_no_wire_option_and_preserves_unreported_usage() {
    let request = GeminiCacheRequest::implicit();
    assert_eq!(request.mode(), GeminiCacheMode::Implicit);
    assert_eq!(request.provider_option().unwrap(), None);
    let usage = GeminiCacheUsage::parse(&request, &serde_json::json!({ "promptTokenCount": 1000 }))
        .unwrap();
    assert_eq!(usage.mode(), GeminiCacheMode::Implicit);
    assert_eq!(usage.prompt_tokens(), Some(1000));
    assert_eq!(usage.cached_tokens(), None);
    assert_eq!(usage.cache_hit(), None);
    assert_eq!(usage.uncached_prompt_tokens(), None);
    assert_eq!(usage.cache_tokens_details(), None);
}

#[test]
fn explicit_cache_mode_records_the_resource_and_normalizes_usage_by_modality() {
    let request = GeminiCacheRequest::explicit("cachedContents/cache-42").unwrap();
    assert_eq!(request.mode(), GeminiCacheMode::Explicit);
    assert!(!format!("{request:?}").contains("cache-42"));
    let option = request.provider_option().unwrap().unwrap();
    assert_eq!(option.kind(), GOOGLE_CONTEXT_CACHE_OPTION_KIND);
    assert_eq!(
        option.data(),
        &serde_json::json!({ "cachedContent": "cachedContents/cache-42" })
    );

    let usage = GeminiCacheUsage::parse(
        &request,
        &serde_json::json!({
            "promptTokenCount": 1000,
            "cachedContentTokenCount": 400,
            "cacheTokensDetails": [
                { "modality": "TEXT", "tokenCount": 300 },
                { "modality": "IMAGE", "tokenCount": 99 }
            ]
        }),
    )
    .unwrap();
    assert_eq!(usage.mode(), GeminiCacheMode::Explicit);
    assert_eq!(usage.prompt_tokens(), Some(1000));
    assert_eq!(usage.cached_tokens(), Some(400));
    assert_eq!(usage.cache_hit(), Some(true));
    assert_eq!(usage.uncached_prompt_tokens(), Some(600));
    let details = usage.cache_tokens_details().unwrap();
    assert_eq!(details.len(), 2);
    assert_eq!(details[0].modality(), GeminiModality::Text);
    assert_eq!(details[0].tokens(), 300);
    assert_eq!(details[1].modality(), GeminiModality::Image);
    // The discovery schema states no arithmetic identity between the details
    // and total, so the deliberately different 399/400 fixture is accepted.
    assert_eq!(details[1].tokens(), 99);
}

#[test]
fn vertex_explicit_cache_requires_and_preserves_the_full_resource_name() {
    let request = GeminiCacheRequest::vertex_explicit(
        "projects/vertex-fixture/locations/global/cachedContents/123456",
    )
    .unwrap();
    assert_eq!(request.mode(), GeminiCacheMode::Explicit);
    assert!(!format!("{request:?}").contains("vertex-fixture"));
    let option = request.provider_option().unwrap().unwrap();
    assert_eq!(option.provider(), GOOGLE_VERTEX_PROVIDER);
    assert_eq!(
        option.data(),
        &serde_json::json!({
            "cachedContent": "projects/vertex-fixture/locations/global/cachedContents/123456"
        })
    );

    for invalid in [
        "cachedContents/123456",
        "projects/vertex-fixture/locations/global/cachedContents/",
        "projects/BadProject/locations/global/cachedContents/123456",
        "projects/vertex-fixture/locations/us-central1-a/cachedContents/123456",
        "projects/vertex-fixture/locations/global/cachedContents/a/b",
    ] {
        assert!(
            GeminiCacheRequest::vertex_explicit(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn an_explicit_zero_is_a_miss_while_absence_is_unknown() {
    let request = GeminiCacheRequest::explicit("cachedContents/cache-42").unwrap();
    let miss = GeminiCacheUsage::parse(
        &request,
        &serde_json::json!({
            "promptTokenCount": 100,
            "cachedContentTokenCount": 0,
            "cacheTokensDetails": []
        }),
    )
    .unwrap();
    assert_eq!(miss.cache_hit(), Some(false));
    assert_eq!(miss.cached_tokens(), Some(0));
    assert_eq!(miss.cache_tokens_details(), Some(&[][..]));

    let unknown =
        GeminiCacheUsage::parse(&request, &serde_json::json!({ "promptTokenCount": 100 })).unwrap();
    assert_eq!(unknown.cache_hit(), None);
    assert_eq!(unknown.cache_tokens_details(), None);
}

#[test]
fn invalid_cache_resources_and_usage_are_refused_without_defaulting() {
    for invalid in [
        "",
        "cache-42",
        "cachedContents/",
        "cachedContents/a/b",
        "cachedContents/a?x=1",
    ] {
        assert!(GeminiCacheRequest::explicit(invalid).is_err(), "{invalid}");
    }
    let request = GeminiCacheRequest::implicit();
    for usage in [
        serde_json::json!([]),
        serde_json::json!({ "cachedContentTokenCount": -1 }),
        serde_json::json!({ "promptTokenCount": 5, "cachedContentTokenCount": 6 }),
        serde_json::json!({
            "cacheTokensDetails": [{ "modality": "TEXT" }]
        }),
        serde_json::json!({
            "cacheTokensDetails": [{ "modality": "FUTURE_MODALITY", "tokenCount": 1 }]
        }),
    ] {
        assert!(
            GeminiCacheUsage::parse(&request, &usage).is_err(),
            "{usage}"
        );
    }
    assert_eq!(
        GeminiCacheUsage::parse(
            &request,
            &serde_json::json!({ "promptTokenCount": 5, "cachedContentTokenCount": 6 })
        )
        .unwrap_err(),
        CacheMetadataError::CachedExceedsPrompt
    );
}
