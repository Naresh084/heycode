//! PMM01: base URLs are documented facts, and undocumented pairs stay Unknown.

use heycode_core::ProviderProtocol;
use heycode_http::HttpRequest;
use heycode_llm::CapabilitySupport;
use heycode_provider_minimax::{MiniMaxApiFamily, MiniMaxRegion, base_url, documented_base_url};

#[test]
fn regional_hosts_are_the_documented_minimax_hosts() {
    assert_eq!(MiniMaxRegion::International.host(), "api.minimax.io");
    assert_eq!(MiniMaxRegion::MainlandChina.host(), "api.minimaxi.com");
}

#[test]
fn protocol_families_are_served_from_their_documented_base_paths() {
    assert_eq!(MiniMaxApiFamily::OpenAiCompatible.base_path(), "/v1");
    assert_eq!(
        MiniMaxApiFamily::AnthropicCompatible.base_path(),
        "/anthropic"
    );
}

#[test]
fn protocol_families_map_to_their_heycode_protocol() {
    assert_eq!(
        MiniMaxApiFamily::OpenAiCompatible.protocol(),
        ProviderProtocol::OpenAiChatCompletions
    );
    assert_eq!(
        MiniMaxApiFamily::AnthropicCompatible.protocol(),
        ProviderProtocol::AnthropicMessages
    );
}

#[test]
fn base_urls_compose_the_documented_host_and_base_path() {
    assert_eq!(
        base_url(
            MiniMaxRegion::International,
            MiniMaxApiFamily::OpenAiCompatible
        ),
        "https://api.minimax.io/v1"
    );
    assert_eq!(
        base_url(
            MiniMaxRegion::International,
            MiniMaxApiFamily::AnthropicCompatible
        ),
        "https://api.minimax.io/anthropic"
    );
    assert_eq!(
        base_url(
            MiniMaxRegion::MainlandChina,
            MiniMaxApiFamily::AnthropicCompatible
        ),
        "https://api.minimaxi.com/anthropic"
    );
}

#[test]
fn the_mainland_china_openai_compatible_route_is_unknown_not_supported() {
    assert_eq!(
        documented_base_url(
            MiniMaxRegion::MainlandChina,
            MiniMaxApiFamily::OpenAiCompatible
        ),
        CapabilitySupport::Unknown
    );
}

#[test]
fn every_other_region_and_family_pair_has_documentary_evidence() {
    for (region, family) in [
        (
            MiniMaxRegion::International,
            MiniMaxApiFamily::OpenAiCompatible,
        ),
        (
            MiniMaxRegion::International,
            MiniMaxApiFamily::AnthropicCompatible,
        ),
        (
            MiniMaxRegion::MainlandChina,
            MiniMaxApiFamily::AnthropicCompatible,
        ),
    ] {
        assert_eq!(
            documented_base_url(region, family),
            CapabilitySupport::Supported,
            "{region:?}/{family:?} lost its documented evidence"
        );
    }
}

#[test]
fn no_region_and_family_pair_is_marked_unsupported() {
    for region in MiniMaxRegion::ALL {
        for family in MiniMaxApiFamily::ALL {
            assert_ne!(
                documented_base_url(region, family),
                CapabilitySupport::Unsupported,
                "{region:?}/{family:?} claims MiniMax refuses a route we only failed to verify"
            );
        }
    }
}

#[test]
fn every_composed_base_url_forms_a_valid_http_request() {
    for region in MiniMaxRegion::ALL {
        for family in MiniMaxApiFamily::ALL {
            let url = base_url(region, family);
            HttpRequest::get(&url)
                .unwrap_or_else(|error| panic!("`{url}` is not a usable URL: {error}"));
        }
    }
}
