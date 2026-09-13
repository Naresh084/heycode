//! Neutral detailed provider response facts retain evidence without raw bodies.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    CachePrefixImpact, ContextEditKind, ProviderCacheUsage, ProviderContextEdit,
    ProviderResponseMetadata,
};

#[test]
fn cache_and_context_edit_facts_round_trip_without_losing_provider_accounting() {
    let cache = ProviderCacheUsage::new(100, 20, 30, 10)
        .unwrap()
        .with_uncached_input_tokens(60)
        .unwrap()
        .with_cache_write_ttl_tokens(4, 6)
        .unwrap()
        .with_reasoning_tokens(5)
        .unwrap();
    let metadata = ProviderResponseMetadata::new(
        Some(cache),
        vec![
            ProviderContextEdit::new(ContextEditKind::ClearThinking, 2, 40).unwrap(),
            ProviderContextEdit::new(ContextEditKind::ClearToolUses, 3, 25).unwrap(),
        ],
        Some(CachePrefixImpact::InvalidatedAtEdit),
    )
    .unwrap();
    let wire = serde_json::to_value(&metadata).unwrap();
    let restored: ProviderResponseMetadata = serde_json::from_value(wire).unwrap();
    restored.validate().unwrap();

    let cache = restored.cache_usage().unwrap();
    assert_eq!(cache.input_tokens(), 100);
    assert_eq!(cache.output_tokens(), 20);
    assert_eq!(cache.cache_read_tokens(), 30);
    assert_eq!(cache.cache_write_tokens(), 10);
    assert_eq!(cache.uncached_input_tokens(), Some(60));
    assert_eq!(cache.cache_write_5m_tokens(), Some(4));
    assert_eq!(cache.cache_write_1h_tokens(), Some(6));
    assert_eq!(cache.reasoning_tokens(), Some(5));
    assert_eq!(restored.context_edits().len(), 2);
    assert_eq!(restored.total_cleared_input_tokens(), Some(65));
    assert!(restored.invalidated_cache_prefix());
}

#[test]
fn impossible_or_ambiguous_detailed_facts_fail_closed() {
    assert!(ProviderCacheUsage::new(10, 1, 11, 0).is_err());
    assert!(
        ProviderCacheUsage::new(10, 1, 2, 3)
            .unwrap()
            .with_uncached_input_tokens(4)
            .is_err()
    );
    assert!(
        ProviderCacheUsage::new(10, 1, 2, 3)
            .unwrap()
            .with_cache_write_ttl_tokens(1, 1)
            .is_err()
    );
    assert!(
        ProviderCacheUsage::new(10, 1, 2, 3)
            .unwrap()
            .with_reasoning_tokens(2)
            .is_err()
    );
    assert!(ProviderResponseMetadata::new(None, Vec::new(), None).is_err());

    let edit = ProviderContextEdit::new(ContextEditKind::ClearThinking, 1, 1).unwrap();
    assert!(
        ProviderResponseMetadata::new(
            None,
            vec![edit.clone(), edit],
            Some(CachePrefixImpact::InvalidatedAtEdit)
        )
        .is_err()
    );

    let tampered = serde_json::json!({
        "schema_version":1,
        "cache_usage":{
            "schema_version":1,
            "input_tokens":5,
            "output_tokens":1,
            "cache_read_tokens":9,
            "cache_write_tokens":0
        },
        "context_edits":[]
    });
    let restored: ProviderResponseMetadata = serde_json::from_value(tampered).unwrap();
    assert!(restored.validate().is_err());
}
