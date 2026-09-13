//! POA05 prompt-cache controls and lossless usage accounting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::Context;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEventStream};
use heycode_llm::{CapabilitySupport, Provider};
use heycode_provider_openai::{
    OPENAI_GPT_5_6_SOL, OpenAiCacheActivity, OpenAiCacheUsage, OpenAiPromptCacheControl,
    OpenAiPromptCacheFault, OpenAiPromptCacheMode, OpenAiPromptCachePolicy,
    OpenAiPromptCachePolicyFault, OpenAiProvider, openai_prompt_cache_settings_definition,
    openai_prompt_cache_settings_namespace, openai_prompt_cache_support,
    resolve_openai_prompt_cache_policy,
};
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsDocuments, SettingsError, SettingsSchema,
    SettingsService, WireExposureFault,
};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

#[test]
fn current_cache_options_are_exact_capability_gated_and_key_redacted() {
    assert_eq!(
        openai_prompt_cache_support(OPENAI_GPT_5_6_SOL),
        CapabilitySupport::Supported
    );
    assert_eq!(
        openai_prompt_cache_support("unlisted-model"),
        CapabilitySupport::Unknown
    );

    for (mode, wire_mode) in [
        (OpenAiPromptCacheMode::Implicit, "implicit"),
        (OpenAiPromptCacheMode::Explicit, "explicit"),
    ] {
        let control = OpenAiPromptCacheControl::new("session-cache-01", mode).unwrap();
        assert_eq!(
            control.wire_for(OPENAI_GPT_5_6_SOL).unwrap(),
            &serde_json::json!({
                "prompt_cache_key":"session-cache-01",
                "prompt_cache_options":{"mode":wire_mode,"ttl":"30m"}
            })
        );
        assert_eq!(control.mode(), mode);
        assert!(!format!("{control:?}").contains("session-cache-01"));
        let option = control.provider_option(OPENAI_GPT_5_6_SOL).unwrap();
        assert_eq!(option.provider(), "openai");
        assert_eq!(option.kind(), "prompt-cache");
        assert_eq!(option.data(), control.wire_for(OPENAI_GPT_5_6_SOL).unwrap());
        assert!(!format!("{option:?}").contains("session-cache-01"));
        assert_eq!(
            control.wire_for("unlisted-model").unwrap_err(),
            OpenAiPromptCacheFault::UnprovenCapability
        );
        assert_eq!(
            control.provider_option("unlisted-model").unwrap_err(),
            OpenAiPromptCacheFault::UnprovenCapability
        );
    }
}

#[test]
fn cache_reads_and_writes_remain_distinct_from_ordinary_token_totals() {
    let usage = OpenAiCacheUsage::from_usage(&serde_json::json!({
        "input_tokens":139,
        "input_tokens_details":{"cached_tokens":11,"cache_write_tokens":17},
        "output_tokens":438,
        "output_tokens_details":{"reasoning_tokens":64},
        "total_tokens":577
    }))
    .unwrap();

    assert_eq!(usage.input_tokens(), 139);
    assert_eq!(usage.cache_read_tokens(), 11);
    assert_eq!(usage.cache_write_tokens(), 17);
    assert_eq!(usage.output_tokens(), 438);
    assert_eq!(usage.reasoning_tokens(), 64);
    assert_eq!(usage.total_tokens(), 577);
    assert_eq!(usage.activity(), OpenAiCacheActivity::ReadAndWrite);
    assert!(format!("{usage:?}").contains("cache_read_tokens: 11"));
    assert!(format!("{usage:?}").contains("cache_write_tokens: 17"));
}

#[test]
fn invalid_controls_and_usage_fail_closed_without_echoing_input() {
    let canary = "sk-proj CACHE SECRET CANARY";
    let fault = OpenAiPromptCacheControl::new(canary, OpenAiPromptCacheMode::Implicit).unwrap_err();
    assert_eq!(fault, OpenAiPromptCacheFault::InvalidConfiguration);
    assert!(!format!("{fault:?} {fault}").contains(canary));

    for usage in [
        serde_json::json!({
            "input_tokens":10,"input_tokens_details":{"cached_tokens":1},
            "output_tokens":2,"output_tokens_details":{"reasoning_tokens":0},
            "total_tokens":12
        }),
        serde_json::json!({
            "input_tokens":10,
            "input_tokens_details":{"cached_tokens":1,"cache_write_tokens":2},
            "output_tokens":2,"output_tokens_details":{"reasoning_tokens":0},
            "total_tokens":999
        }),
    ] {
        assert_eq!(
            OpenAiCacheUsage::from_usage(&usage).unwrap_err(),
            OpenAiPromptCacheFault::InvalidUsage
        );
    }
}

#[test]
fn settings_default_is_disabled_restart_applied_and_wire_exposed() {
    let mut context = Context::new();
    let settings = SettingsService::new(SettingsDocuments::new());
    let namespace = openai_prompt_cache_settings_namespace().unwrap();
    let snapshot = settings
        .register(&context, openai_prompt_cache_settings_definition().unwrap())
        .unwrap();

    assert_eq!(snapshot.namespace(), &namespace);
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert_eq!(
        snapshot.resolved(),
        &serde_json::json!({
            "enabled":false,
            "prompt_cache_key":"",
            "mode":"implicit"
        })
    );
    assert!(snapshot.wire_exposed());
    assert_eq!(
        snapshot.wire_projection().unwrap().resolved(),
        snapshot.resolved()
    );
    assert_eq!(
        resolve_openai_prompt_cache_policy(&settings).unwrap(),
        OpenAiPromptCachePolicy::Disabled
    );
    let provider = resolve_openai_prompt_cache_policy(&settings)
        .unwrap()
        .apply_to(
            OpenAiProvider::new(
                HttpService::new(Arc::new(DeadTransport)),
                "test-key",
                Some(OPENAI_GPT_5_6_SOL.to_owned()),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(Provider::request_options(&provider).is_empty());

    context.shutdown();
    assert!(settings.get(&namespace).unwrap().is_none());
}

#[test]
fn enabled_settings_resolve_a_typed_policy_root_can_apply() {
    for (wire_mode, mode) in [
        ("implicit", OpenAiPromptCacheMode::Implicit),
        ("explicit", OpenAiPromptCacheMode::Explicit),
    ] {
        let namespace = openai_prompt_cache_settings_namespace().unwrap();
        let mut documents = SettingsDocuments::new();
        documents
            .set_user(
                namespace,
                serde_json::json!({
                    "enabled":true,
                    "prompt_cache_key":"team-session-01",
                    "mode":wire_mode
                }),
            )
            .unwrap();
        let mut context = Context::new();
        let settings = SettingsService::new(documents);
        settings
            .register(&context, openai_prompt_cache_settings_definition().unwrap())
            .unwrap();

        let policy = resolve_openai_prompt_cache_policy(&settings).unwrap();
        assert!(matches!(
            &policy,
            OpenAiPromptCachePolicy::Enabled(control) if control.mode() == mode
        ));
        let provider = policy
            .apply_to(
                OpenAiProvider::new(
                    HttpService::new(Arc::new(DeadTransport)),
                    "test-key",
                    Some(OPENAI_GPT_5_6_SOL.to_owned()),
                )
                .unwrap(),
            )
            .unwrap();
        let options = Provider::request_options(&provider);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].kind(), "prompt-cache");
        assert_eq!(options[0].data()["prompt_cache_key"], "team-session-01");
        assert_eq!(options[0].data()["prompt_cache_options"]["mode"], wire_mode);
        context.shutdown();
    }
}

#[test]
fn invalid_or_credential_shaped_settings_never_publish() {
    let namespace = openai_prompt_cache_settings_namespace().unwrap();
    for section in [
        serde_json::json!({
            "enabled":true,
            "prompt_cache_key":"",
            "mode":"implicit"
        }),
        serde_json::json!({
            "enabled":true,
            "prompt_cache_key":"has spaces",
            "mode":"implicit"
        }),
        serde_json::json!({
            "enabled":true,
            "prompt_cache_key":"team-session-01",
            "mode":"automatic"
        }),
        serde_json::json!({
            "enabled":false,
            "prompt_cache_key":"",
            "mode":"implicit",
            "unknown":true
        }),
    ] {
        let mut documents = SettingsDocuments::new();
        documents.set_user(namespace.clone(), section).unwrap();
        let context = Context::new();
        let settings = SettingsService::new(documents);
        assert!(matches!(
            settings
                .register(&context, openai_prompt_cache_settings_definition().unwrap())
                .unwrap_err(),
            SettingsError::InvalidResolved { .. }
        ));
    }

    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace,
            serde_json::json!({
                "enabled":true,
                "prompt_cache_key":"sk-cache-policy-canary-000001",
                "mode":"implicit"
            }),
        )
        .unwrap();
    let context = Context::new();
    let settings = SettingsService::new(documents);
    let error = settings
        .register(&context, openai_prompt_cache_settings_definition().unwrap())
        .unwrap_err();
    assert!(matches!(
        error,
        SettingsError::UnprovableWireExposure {
            fault: WireExposureFault::CredentialMaterial,
            ..
        }
    ));
    assert!(!format!("{error:?} {error}").contains("sk-cache-policy-canary"));

    let mut disabled_documents = SettingsDocuments::new();
    disabled_documents
        .set_user(
            openai_prompt_cache_settings_namespace().unwrap(),
            serde_json::json!({
                "enabled":false,
                "prompt_cache_key":"saved-session-key",
                "mode":"explicit"
            }),
        )
        .unwrap();
    let mut disabled_context = Context::new();
    let disabled_settings = SettingsService::new(disabled_documents);
    disabled_settings
        .register(
            &disabled_context,
            openai_prompt_cache_settings_definition().unwrap(),
        )
        .unwrap();
    assert_eq!(
        resolve_openai_prompt_cache_policy(&disabled_settings).unwrap(),
        OpenAiPromptCachePolicy::Disabled,
        "disabled must never apply retained key/mode fields"
    );
    disabled_context.shutdown();
}

#[test]
fn typed_policy_refuses_a_wire_dark_snapshot_with_the_same_shape() {
    let mut context = Context::new();
    let settings = SettingsService::new(SettingsDocuments::new());
    let schema = SettingsSchema::new(
        serde_json::json!({"type":"object"}),
        serde_json::json!({
            "enabled":false,
            "prompt_cache_key":"",
            "mode":"implicit"
        }),
        |_| Ok(()),
    )
    .unwrap();
    let snapshot = settings
        .register(
            &context,
            SettingsDefinition::new(openai_prompt_cache_settings_namespace().unwrap(), schema)
                .with_applies(SettingsApplies::Restart),
        )
        .unwrap();
    assert_eq!(
        OpenAiPromptCachePolicy::from_snapshot(&snapshot).unwrap_err(),
        OpenAiPromptCachePolicyFault::InvalidSnapshot
    );
    context.shutdown();
}
