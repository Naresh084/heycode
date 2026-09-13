//! PDS05 endpoint and wire contracts for DeepSeek's four optional surfaces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use heycode_core::ToolSpec;
use heycode_llm::{CapabilitySupport, Role};
use heycode_provider_deepseek::{
    DEEPSEEK_BETA_BASE_URL, DEEPSEEK_BETA_CHAT_COMPLETIONS_URL, DEEPSEEK_BETA_FIM_COMPLETIONS_URL,
    DEEPSEEK_CHAT_PREFIX_OPTION_KIND, DEEPSEEK_FIM_MAX_OUTPUT_TOKENS,
    DEEPSEEK_JSON_OUTPUT_OPTION_KIND, DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL,
    DEEPSEEK_STRICT_TOOLS_OPTION_KIND, DEEPSEEK_V4_FLASH, DEEPSEEK_V4_FLASH_VISION_EXP,
    DEEPSEEK_V4_PRO, DeepSeekChatPrefix, DeepSeekFimRequest, DeepSeekJsonOutput,
    DeepSeekOptionalCapability, DeepSeekOptionalError, DeepSeekOptionalProtocol,
    DeepSeekStrictTools,
};

fn tool(name: &str, schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: format!("Run {name}"),
        parameters: schema,
    }
}

#[test]
fn every_optional_capability_has_its_own_explicit_endpoint_and_primary_evidence() {
    let rows = [
        (
            DeepSeekOptionalCapability::StrictTools,
            DEEPSEEK_BETA_BASE_URL,
            "/chat/completions",
            DEEPSEEK_BETA_CHAT_COMPLETIONS_URL,
            DeepSeekOptionalProtocol::ChatCompletions,
            true,
            "https://api-docs.deepseek.com/guides/tool_calls/",
        ),
        (
            DeepSeekOptionalCapability::JsonOutput,
            "https://api.deepseek.com",
            "/chat/completions",
            DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL,
            DeepSeekOptionalProtocol::ChatCompletions,
            false,
            "https://api-docs.deepseek.com/guides/json_mode/",
        ),
        (
            DeepSeekOptionalCapability::FimCompletion,
            DEEPSEEK_BETA_BASE_URL,
            "/completions",
            DEEPSEEK_BETA_FIM_COMPLETIONS_URL,
            DeepSeekOptionalProtocol::FimCompletions,
            true,
            "https://api-docs.deepseek.com/api/create-completion/",
        ),
        (
            DeepSeekOptionalCapability::ChatPrefixCompletion,
            DEEPSEEK_BETA_BASE_URL,
            "/chat/completions",
            DEEPSEEK_BETA_CHAT_COMPLETIONS_URL,
            DeepSeekOptionalProtocol::ChatCompletions,
            true,
            "https://api-docs.deepseek.com/guides/chat_prefix_completion/",
        ),
    ];

    assert_eq!(
        DeepSeekOptionalCapability::ALL,
        [
            DeepSeekOptionalCapability::StrictTools,
            DeepSeekOptionalCapability::JsonOutput,
            DeepSeekOptionalCapability::FimCompletion,
            DeepSeekOptionalCapability::ChatPrefixCompletion,
        ]
    );
    assert_eq!(DeepSeekOptionalCapability::ALL.len(), rows.len());
    for (capability, base, path, url, protocol, beta, evidence) in rows {
        let route = capability.route();
        assert_eq!(route.base_url(), base, "{capability:?}");
        assert_eq!(route.path(), path, "{capability:?}");
        assert_eq!(route.url(), url, "{capability:?}");
        assert_eq!(route.protocol(), protocol, "{capability:?}");
        assert_eq!(route.is_beta(), beta, "{capability:?}");
        assert_eq!(capability.evidence_url(), evidence, "{capability:?}");
    }
}

#[test]
fn strict_tools_use_the_beta_route_and_mark_every_function_strict() {
    let tools = vec![
        tool(
            "lookup",
            serde_json::json!({
                "type":"object",
                "properties":{"id":{"type":"string"}},
                "required":["id"],
                "additionalProperties":false
            }),
        ),
        tool(
            "count",
            serde_json::json!({
                "type":"object",
                "properties":{"value":{"type":"integer"}},
                "required":["value"],
                "additionalProperties":false
            }),
        ),
    ];
    let option = DeepSeekStrictTools::request_option(&tools).unwrap();
    let wire = DeepSeekStrictTools::wire_tools(&tools).unwrap();

    assert_eq!(
        DeepSeekStrictTools::route().url(),
        DEEPSEEK_BETA_CHAT_COMPLETIONS_URL
    );
    assert_eq!(option.provider(), "deepseek");
    assert_eq!(option.kind(), DEEPSEEK_STRICT_TOOLS_OPTION_KIND);
    assert_eq!(option.data(), &serde_json::json!({"enabled":true}));
    assert_eq!(
        DeepSeekStrictTools::thinking_support(),
        CapabilitySupport::Supported
    );
    assert_eq!(wire.len(), 2);
    for (actual, original) in wire.iter().zip(&tools) {
        assert_eq!(actual["type"], "function");
        assert_eq!(actual["function"]["name"], original.name);
        assert_eq!(actual["function"]["description"], original.description);
        assert_eq!(actual["function"]["parameters"], original.parameters);
        assert_eq!(actual["function"]["strict"], true);
    }
}

#[test]
fn strict_mode_refuses_an_empty_tool_set_instead_of_degrading_to_generic_chat() {
    assert!(matches!(
        DeepSeekStrictTools::request_option(&[]),
        Err(DeepSeekOptionalError::EmptyStrictTools)
    ));
    assert!(matches!(
        DeepSeekStrictTools::wire_tools(&[]),
        Err(DeepSeekOptionalError::EmptyStrictTools)
    ));
}

#[test]
fn json_output_uses_standard_chat_and_requires_the_documented_prompt_keyword() {
    for missing in ["return an object", "notjson text", "structured output"] {
        assert!(matches!(
            DeepSeekJsonOutput::request_option(missing),
            Err(DeepSeekOptionalError::JsonPromptMissingKeyword)
        ));
    }
    let option = DeepSeekJsonOutput::request_option("Return JSON-formatted output").unwrap();

    assert_eq!(
        DeepSeekJsonOutput::route().url(),
        DEEPSEEK_STANDARD_CHAT_COMPLETIONS_URL
    );
    assert!(!DeepSeekJsonOutput::route().is_beta());
    assert_eq!(option.provider(), "deepseek");
    assert_eq!(option.kind(), DEEPSEEK_JSON_OUTPUT_OPTION_KIND);
    assert_eq!(
        option.data(),
        &serde_json::json!({"response_format":{"type":"json_object"}})
    );
}

#[test]
fn chat_prefix_is_a_beta_assistant_message_option_not_a_generic_chat_capability() {
    let message = DeepSeekChatPrefix::assistant_message("```rust\n");
    let option = DeepSeekChatPrefix::request_option().unwrap();

    assert_eq!(
        DeepSeekChatPrefix::route().url(),
        DEEPSEEK_BETA_CHAT_COMPLETIONS_URL
    );
    assert_eq!(option.provider(), "deepseek");
    assert_eq!(option.kind(), DEEPSEEK_CHAT_PREFIX_OPTION_KIND);
    assert_eq!(option.data(), &serde_json::json!({"enabled":true}));
    assert_eq!(message.role, Role::Assistant);
    assert_eq!(message.content, "```rust\n");
    assert!(message.tool_calls.is_none());
}

#[test]
fn fim_has_its_own_beta_completions_body_and_never_smuggles_chat_or_thinking_fields() {
    let request = DeepSeekFimRequest::new(
        "fn fib(n: u64) -> u64 {",
        Some("\n}".to_owned()),
        Some(DEEPSEEK_FIM_MAX_OUTPUT_TOKENS),
    )
    .unwrap();

    assert_eq!(request.route().url(), DEEPSEEK_BETA_FIM_COMPLETIONS_URL);
    assert_eq!(request.model(), DEEPSEEK_V4_PRO);
    assert_eq!(
        request.body(),
        serde_json::json!({
            "model":"deepseek-v4-pro",
            "prompt":"fn fib(n: u64) -> u64 {",
            "suffix":"\n}",
            "max_tokens":4096
        })
    );
    for forbidden in ["messages", "thinking", "tools", "response_format", "prefix"] {
        assert!(request.body().get(forbidden).is_none(), "{forbidden}");
    }
}

#[test]
fn fim_enforces_only_the_published_maximum_and_keeps_conflicting_model_evidence_unknown() {
    assert!(DeepSeekFimRequest::new("", None, Some(0)).is_ok());
    assert!(matches!(
        DeepSeekFimRequest::new("prefix", None, Some(DEEPSEEK_FIM_MAX_OUTPUT_TOKENS + 1)),
        Err(DeepSeekOptionalError::FimMaxTokensExceeded {
            requested: 4_097,
            maximum: 4_096
        })
    ));

    assert_eq!(
        DeepSeekFimRequest::model_support(DEEPSEEK_V4_PRO),
        CapabilitySupport::Supported
    );
    assert_eq!(
        DeepSeekFimRequest::model_support(DEEPSEEK_V4_FLASH),
        CapabilitySupport::Unknown,
        "the pricing table says supported while the endpoint schema lists only V4 Pro"
    );
    assert_eq!(
        DeepSeekFimRequest::model_support(DEEPSEEK_V4_FLASH_VISION_EXP),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        DeepSeekFimRequest::model_support("future-model"),
        CapabilitySupport::Unknown
    );
    assert_eq!(
        DeepSeekFimRequest::thinking_support(),
        CapabilitySupport::Unsupported
    );
}

#[test]
fn optional_request_debug_output_never_contains_model_visible_payloads() {
    let prefix = DeepSeekChatPrefix::request_option().unwrap();
    let fim = DeepSeekFimRequest::new("do-not-print-this-prompt", None, Some(64)).unwrap();

    assert!(!format!("{prefix:?}").contains("enabled"));
    assert!(!format!("{fim:?}").contains("do-not-print-this-prompt"));
}
