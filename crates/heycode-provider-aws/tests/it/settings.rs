//! PAWS06 restart-applied provider policy and inference-config construction.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use heycode_authorization_aws::{AWS_BEDROCK_API_KEY_REFERENCE, AwsRegion};
use heycode_core::{Plugin, compose};
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_llm::{
    CapabilitySupport, LlmSelection, ModelDescriptor, ProviderRegistry, SERVICE_PROVIDERS,
    llm_plugin, model_catalog_plugin,
};
use heycode_provider_aws::{
    AWS_BEDROCK_SETTINGS_NAMESPACE, AwsBedrockSettingsError, BedrockCachePlacement,
    BedrockConverseModelEvidence, BedrockConverseModelFact, BedrockPromptCacheCapabilities,
    aws_bedrock_settings_plugin, aws_converse_config_from_settings,
    aws_converse_live_config_from_settings, aws_converse_live_settings_plugin,
    aws_inference_plugin, resolve_aws_bedrock_settings,
};

use super::support::{CredentialsPlugin, Secret, http, http_plugin};
use heycode_settings::{
    SERVICE_SETTINGS, SettingsApplies, SettingsDocuments, SettingsNamespace, SettingsService,
    settings_plugin,
};

const MODEL: &str = "us.anthropic.claude-sonnet-4-6-v1:0";

fn api_key_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(AWS_BEDROCK_API_KEY_REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn descriptor(prompt_cache: CapabilitySupport) -> ModelDescriptor {
    let mut descriptor = ModelDescriptor::unknown(MODEL);
    descriptor.display_name = "Bedrock fixture".to_owned();
    descriptor.capabilities.tools = CapabilitySupport::Supported;
    descriptor.capabilities.prompt_cache = prompt_cache;
    descriptor
}

fn cache_evidence(one_hour_ttl: CapabilitySupport) -> BedrockPromptCacheCapabilities {
    BedrockPromptCacheCapabilities::new(
        MODEL,
        vec![BedrockCachePlacement::Tools, BedrockCachePlacement::System],
        2,
        one_hour_ttl,
    )
    .unwrap()
}

fn settings_documents(value: serde_json::Value) -> SettingsDocuments {
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            SettingsNamespace::new(AWS_BEDROCK_SETTINGS_NAMESPACE).unwrap(),
            value,
        )
        .unwrap();
    documents
}

#[test]
fn default_policy_is_restart_applied_route_metadata_only_and_effect_owned() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(SettingsDocuments::new()),
        aws_bedrock_settings_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let snapshot = settings
        .get(&SettingsNamespace::new(AWS_BEDROCK_SETTINGS_NAMESPACE).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert!(snapshot.wire_exposed());

    let policy = resolve_aws_bedrock_settings(&settings).unwrap();
    let metadata = policy.runtime_metadata(MODEL, None).unwrap();
    assert!(metadata.prompt_cache().is_none());
    assert!(metadata.guardrail().is_none());
    let option = metadata
        .to_provider_option(
            &AwsRegion::new("us-east-1").unwrap(),
            &descriptor(CapabilitySupport::Unknown),
        )
        .unwrap();
    assert_eq!(
        option.data(),
        &serde_json::json!({
            "route": {
                "source_region": "us-east-1",
                "target_kind": "cross_region_inference_profile",
                "cross_region_scope": "geographic"
            }
        })
    );
    let application =
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/app-profile";
    let application_option = metadata
        .to_provider_option(
            &AwsRegion::new("us-east-1").unwrap(),
            &ModelDescriptor::unknown(application),
        )
        .unwrap();
    assert_eq!(
        application_option.data()["route"],
        serde_json::json!({
            "source_region":"us-east-1",
            "target_kind":"application_inference_profile"
        })
    );

    let config = aws_converse_live_config_from_settings(
        &settings,
        AwsRegion::new("us-east-1").unwrap(),
        api_key_query(),
        MODEL,
        None,
    )
    .unwrap();
    assert_eq!(
        aws_inference_plugin(config).name(),
        "inference-bedrock-converse"
    );

    context.shutdown();
    assert!(
        settings
            .get(&SettingsNamespace::new(AWS_BEDROCK_SETTINGS_NAMESPACE).unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn settings_backed_converse_plugin_resolves_after_namespace_registration() {
    let (http, requests) = http(Vec::new());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(SettingsDocuments::new()),
        aws_bedrock_settings_plugin(),
        http_plugin(http),
        Box::new(CredentialsPlugin(Secret::Present)),
        model_catalog_plugin(Duration::from_secs(300)),
        llm_plugin(
            LlmSelection {
                provider_name: "bedrock".to_owned(),
                model: MODEL.to_owned(),
            },
            Vec::new(),
        ),
        aws_converse_live_settings_plugin(
            AwsRegion::new("us-east-1").unwrap(),
            api_key_query(),
            MODEL,
            None,
        )
        .unwrap(),
    ];
    let mut context = compose(&plugins).unwrap();
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        context
            .get::<ProviderRegistry>(SERVICE_PROVIDERS)
            .unwrap()
            .get("bedrock")
            .is_some()
    );
    context.shutdown();
}

#[test]
fn configured_cache_guardrail_and_route_build_one_exact_converse_config() {
    let guardrail_id = "arn:aws:bedrock:us-east-1:123456789012:guardrail/grabc123";
    let documents = settings_documents(serde_json::json!({
        "prompt_cache": {
            "mode": "enabled",
            "points": [
                {"placement":"tools","ttl":"1h"},
                {"placement":"system","ttl":"5m"}
            ]
        },
        "guardrail": {
            "mode": "enabled",
            "identifier": guardrail_id,
            "version": "7",
            "trace": "enabled-full",
            "stream_mode": "async"
        }
    }));
    let plugins: Vec<Box<dyn Plugin>> =
        vec![settings_plugin(documents), aws_bedrock_settings_plugin()];
    let mut context = compose(&plugins).unwrap();
    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let policy = resolve_aws_bedrock_settings(&settings).unwrap();
    let metadata = policy
        .runtime_metadata(MODEL, Some(cache_evidence(CapabilitySupport::Supported)))
        .unwrap();
    let option = metadata
        .to_provider_option(
            &AwsRegion::new("us-east-1").unwrap(),
            &descriptor(CapabilitySupport::Supported),
        )
        .unwrap();
    assert_eq!(
        option.data(),
        &serde_json::json!({
            "route": {
                "source_region": "us-east-1",
                "target_kind": "cross_region_inference_profile",
                "cross_region_scope": "geographic"
            },
            "cache_points": [
                {"placement":"tools","cachePoint":{"type":"default","ttl":"1h"}},
                {"placement":"system","cachePoint":{"type":"default"}}
            ],
            "guardrailConfig": {
                "guardrailIdentifier": guardrail_id,
                "guardrailVersion": "7",
                "trace": "enabled_full",
                "streamProcessingMode": "async"
            }
        })
    );
    assert!(!format!("{metadata:?}").contains(guardrail_id));

    let evidence = BedrockConverseModelEvidence::new(vec![
        BedrockConverseModelFact::new(
            descriptor(CapabilitySupport::Supported),
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
        )
        .unwrap(),
    ])
    .unwrap();
    let config = aws_converse_config_from_settings(
        &settings,
        AwsRegion::new("us-east-1").unwrap(),
        api_key_query(),
        MODEL,
        evidence,
        Some(cache_evidence(CapabilitySupport::Supported)),
    )
    .unwrap();
    assert_eq!(
        aws_inference_plugin(config).name(),
        "inference-bedrock-converse"
    );
    context.shutdown();
}

#[test]
fn one_hour_cache_never_promotes_missing_or_unknown_evidence() {
    let documents = settings_documents(serde_json::json!({
        "prompt_cache": {
            "mode": "enabled",
            "points": [{"placement":"tools","ttl":"1h"}]
        }
    }));
    let plugins: Vec<Box<dyn Plugin>> =
        vec![settings_plugin(documents), aws_bedrock_settings_plugin()];
    let mut context = compose(&plugins).unwrap();
    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let policy = resolve_aws_bedrock_settings(&settings).unwrap();
    assert_eq!(
        policy.runtime_metadata(MODEL, None).unwrap_err(),
        AwsBedrockSettingsError::CacheEvidenceUnproven
    );
    assert_eq!(
        policy
            .runtime_metadata(MODEL, Some(cache_evidence(CapabilitySupport::Unknown)))
            .unwrap_err(),
        AwsBedrockSettingsError::CacheEvidenceUnproven
    );
    context.shutdown();
}

#[test]
fn malformed_or_value_bearing_guardrail_settings_fail_without_echo() {
    let canary = "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let documents = settings_documents(serde_json::json!({
        "guardrail": {
            "mode": "enabled",
            "identifier": "grabc123",
            "version": "7",
            "trace": "provider-default",
            "stream_mode": "provider-default",
            "api_key": canary
        }
    }));
    let plugins: Vec<Box<dyn Plugin>> =
        vec![settings_plugin(documents), aws_bedrock_settings_plugin()];
    let error = compose(&plugins)
        .err()
        .expect("unknown value field must fail");
    assert!(!error.to_string().contains(canary));
    assert!(!format!("{error:?}").contains(canary));
}

#[test]
fn inactive_policy_values_are_still_bounded_and_validated() {
    for user in [
        serde_json::json!({
            "prompt_cache":{
                "mode":"disabled",
                "points":[
                    {"placement":"tools","ttl":"5m"},
                    {"placement":"system","ttl":"5m"},
                    {"placement":"latest-user-message","ttl":"5m"},
                    {"placement":"tools","ttl":"1h"}
                ]
            }
        }),
        serde_json::json!({
            "guardrail":{
                "mode":"disabled",
                "identifier":"not-a-valid-guardrail-id",
                "version":"7",
                "trace":"provider-default",
                "stream_mode":"provider-default"
            }
        }),
    ] {
        let plugins: Vec<Box<dyn Plugin>> = vec![
            settings_plugin(settings_documents(user)),
            aws_bedrock_settings_plugin(),
        ];
        assert!(compose(&plugins).is_err());
    }
}
