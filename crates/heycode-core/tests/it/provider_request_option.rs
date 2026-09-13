//! Provider-owned request options are safe, schema-tagged durable vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::ProviderRequestOption;

#[test]
fn provider_request_option_round_trips_without_debugging_its_data() {
    let option = ProviderRequestOption::new(
        "openrouter",
        "routing",
        serde_json::json!({
            "order": ["z-ai", "novita"],
            "allow_fallbacks": false,
            "require_parameters": true,
            "data_collection": "deny",
            "zdr": true,
            "debug_canary": "must-not-enter-debug"
        }),
    )
    .unwrap();
    assert_eq!(option.provider(), "openrouter");
    assert_eq!(option.kind(), "routing");
    assert_eq!(option.schema_version(), 1);
    assert!(option.data().is_object());
    assert!(!format!("{option:?}").contains("must-not-enter-debug"));

    let encoded = serde_json::to_vec(&option).unwrap();
    let decoded: ProviderRequestOption = serde_json::from_slice(&encoded).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, option);
}

#[test]
fn provider_request_option_rejects_unsafe_identity_shape_and_size() {
    assert!(ProviderRequestOption::new(" openrouter", "routing", serde_json::json!({})).is_err());
    assert!(ProviderRequestOption::new("openrouter", "Routing", serde_json::json!({})).is_err());
    assert!(ProviderRequestOption::new("openrouter", "routing", serde_json::json!([])).is_err());
    assert!(
        ProviderRequestOption::new(
            "openrouter",
            "routing",
            serde_json::json!({"value": "x".repeat(65 * 1024)})
        )
        .is_err()
    );
}
