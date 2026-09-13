//! N02 normalized provider server-tool vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    CallId, ServerToolCall, ServerToolOutcome, ServerToolResult, ServerToolSource, ServerToolUsage,
    ServerToolUsageCost, ServerToolUsageEvidence, ServerToolWebMetadata, UrlCitation,
};

#[test]
fn normalized_server_tool_values_round_trip_without_debugging_provider_content() {
    let call = ServerToolCall::new(
        CallId::from_raw("srvtoolu_1"),
        "web_search",
        "web_search",
        serde_json::json!({"query":"private query"}),
    )
    .unwrap();
    assert_eq!(call.logical(), "web_search");
    assert!(!format!("{call:?}").contains("private query"));

    let source = ServerToolSource::new("https://example.test/source?q=rust", Some("Rust"))
        .unwrap()
        .with_web_metadata(
            ServerToolWebMetadata::new(
                "Example",
                "https://example.test/icon.png",
                "ref_1",
                "2026-08-29",
            )
            .unwrap(),
        )
        .unwrap();
    let result = ServerToolResult::success(call.id().clone(), Some(1), vec![source]).unwrap();
    assert_eq!(result.outcome(), ServerToolOutcome::Success);
    assert_eq!(result.sources().len(), 1);

    let citation = UrlCitation::new(
        "https://example.test/source?q=rust",
        Some("Rust"),
        Some("bounded cited text"),
        Some(7),
        Some(25),
    )
    .unwrap();
    let wire = serde_json::to_string(&(call, result, citation)).unwrap();
    let decoded: (ServerToolCall, ServerToolResult, UrlCitation) =
        serde_json::from_str(&wire).unwrap();
    decoded.0.validate().unwrap();
    decoded.1.validate().unwrap();
    decoded.2.validate().unwrap();
    assert_eq!(
        decoded.1.sources()[0]
            .web_metadata()
            .unwrap()
            .provider_reference(),
        "ref_1"
    );

    let legacy: ServerToolSource = serde_json::from_value(serde_json::json!({
        "url":"https://example.test/legacy",
        "title":"Legacy"
    }))
    .unwrap();
    legacy.validate().unwrap();
    assert!(legacy.web_metadata().is_none());
}

#[test]
fn aggregate_server_tool_usage_keeps_count_evidence_and_unknown_cost_distinct() {
    let usage = ServerToolUsage::new(
        "web_search",
        2,
        ServerToolUsageEvidence::ProviderAggregate,
        ServerToolUsageCost::Unknown,
    )
    .unwrap();
    assert_eq!(usage.logical(), "web_search");
    assert_eq!(usage.requests(), 2);
    assert_eq!(usage.evidence(), ServerToolUsageEvidence::ProviderAggregate);
    assert_eq!(usage.cost(), &ServerToolUsageCost::Unknown);
    assert!(!format!("{usage:?}").contains("query"));

    assert!(
        ServerToolUsage::new(
            "web_search",
            0,
            ServerToolUsageEvidence::ProviderAggregate,
            ServerToolUsageCost::Unknown,
        )
        .is_err()
    );
    assert!(ServerToolUsageCost::published("usd", 0).is_err());
    assert_ne!(
        ServerToolUsageCost::Unknown,
        ServerToolUsageCost::published("usd", 5_000_000_000).unwrap()
    );
}

#[test]
fn unsafe_or_inconsistent_server_tool_values_fail_closed() {
    assert!(
        ServerToolCall::new(
            CallId::from_raw("bad\nid"),
            "web_search",
            "web_search",
            serde_json::json!({}),
        )
        .is_err()
    );
    assert!(
        ServerToolCall::new(
            CallId::from_raw("srvtoolu_1"),
            "web_search",
            "web_search",
            serde_json::json!([]),
        )
        .is_err()
    );
    assert!(ServerToolSource::new("https://user:pass@example.test/", Some("source")).is_err());
    assert!(
        ServerToolWebMetadata::new("site", "file:///private/icon.png", "ref", "2026-08-29")
            .is_err()
    );
    assert!(UrlCitation::new("javascript:alert(1)", Some("source"), None, None, None,).is_err());
    assert!(
        UrlCitation::new(
            "https://example.test/",
            Some("source"),
            None,
            Some(9),
            Some(3),
        )
        .is_err()
    );
    assert!(ServerToolResult::error(CallId::from_raw("srvtoolu_1"), "Bad Code").is_err());
    assert!(ServerToolResult::success(CallId::from_raw("bad\nid"), None, Vec::new(),).is_err());
}
