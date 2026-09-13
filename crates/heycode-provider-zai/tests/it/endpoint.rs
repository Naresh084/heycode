//! PZA01 endpoints: each plan publishes its own, and an undocumented pair is
//! reported rather than filled in.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_http::HttpRequest;
use heycode_provider_zai::{
    Coding, General, ZAI_CODING_ANTHROPIC_BASE_URL, ZAI_CODING_CHAT_BASE_URL,
    ZAI_CODING_RESPONSES_BASE_URL, ZAI_GENERAL_BASE_URL, ZaiEndpoint, ZaiPlan, ZaiProfileError,
    ZaiProtocol,
};

#[test]
fn general_endpoint_is_the_documented_pay_as_you_go_base_url() {
    let endpoint = ZaiEndpoint::<General>::documented(ZaiProtocol::OpenAiChatCompletions).unwrap();
    assert_eq!(endpoint.plan(), ZaiPlan::General);
    assert_eq!(endpoint.protocol(), ZaiProtocol::OpenAiChatCompletions);
    assert_eq!(endpoint.base_url(), "https://api.z.ai/api/paas/v4");
}

#[test]
fn coding_endpoints_match_the_documented_protocol_table() {
    for (protocol, expected) in [
        (
            ZaiProtocol::AnthropicMessages,
            "https://api.z.ai/api/anthropic",
        ),
        (
            ZaiProtocol::OpenAiChatCompletions,
            "https://api.z.ai/api/coding/paas/v4",
        ),
        (ZaiProtocol::OpenAiResponses, "https://api.z.ai/api/v1"),
    ] {
        let endpoint = ZaiEndpoint::<Coding>::documented(protocol).unwrap();
        assert_eq!(endpoint.plan(), ZaiPlan::Coding);
        assert_eq!(endpoint.protocol(), protocol);
        assert_eq!(endpoint.base_url(), expected);
    }
}

#[test]
fn the_general_plan_refuses_protocols_z_ai_documents_only_for_the_coding_plan() {
    for protocol in [ZaiProtocol::AnthropicMessages, ZaiProtocol::OpenAiResponses] {
        let error = ZaiEndpoint::<General>::documented(protocol).unwrap_err();
        assert_eq!(
            error,
            ZaiProfileError::UndocumentedEndpoint {
                plan: ZaiPlan::General,
                protocol,
            }
        );
    }
}

#[test]
fn no_two_documented_base_urls_are_shared_between_the_plans() {
    let general = ZaiEndpoint::<General>::documented(ZaiProtocol::OpenAiChatCompletions)
        .unwrap()
        .base_url();
    for protocol in [
        ZaiProtocol::AnthropicMessages,
        ZaiProtocol::OpenAiChatCompletions,
        ZaiProtocol::OpenAiResponses,
    ] {
        let coding = ZaiEndpoint::<Coding>::documented(protocol).unwrap();
        assert_ne!(coding.base_url(), general);
    }
}

#[test]
fn documented_base_urls_are_absolute_credential_free_http_urls() {
    for base_url in [
        ZAI_GENERAL_BASE_URL,
        ZAI_CODING_ANTHROPIC_BASE_URL,
        ZAI_CODING_CHAT_BASE_URL,
        ZAI_CODING_RESPONSES_BASE_URL,
    ] {
        assert!(
            HttpRequest::get(base_url).is_ok(),
            "{base_url} is not a usable request URL"
        );
        assert!(
            !base_url.ends_with('/'),
            "{base_url} would join a path with a doubled separator"
        );
    }
}
