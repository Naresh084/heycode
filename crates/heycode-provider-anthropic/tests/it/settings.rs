//! PAN05/PAN06 restart-applied opt-in Settings boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_authorization_api_key::{
    ApiKeyValidationFailure, ApiKeyValidator, SecretPrompt, SecretPromptRequest,
};
use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
    SERVICE_CREDENTIALS,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SERVICE_HTTP, SseEventStream,
};
use heycode_llm::{Provider, TokenCountRequest, TokenCounterRegistry};
use heycode_provider_anthropic::{
    ANTHROPIC_SETTINGS_NAMESPACE, AnthropicPluginConfig, AnthropicPromptCacheTtl,
    AnthropicProvider, AnthropicSettingsError, AnthropicSettingsPolicies,
    AnthropicTokenCounterConfig, anthropic_plugin, anthropic_settings_definition,
    anthropic_settings_namespace, anthropic_token_counter_plugin,
};
use heycode_settings::{SettingsApplies, SettingsDocuments, SettingsService, settings_plugin};
use tokio_util::sync::CancellationToken;

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct StaticCredentialProvider {
    id: CredentialProviderId,
}

impl CredentialProvider for StaticCredentialProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(CredentialProviderState::configured(
            CredentialSource::Environment,
            false,
        ))
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(Some(CredentialSecret::new("test-key")))
    }
}

struct ConfiguredCredentialsPlugin;

impl Plugin for ConfiguredCredentialsPlugin {
    fn name(&self) -> &'static str {
        "configured-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let credentials = CredentialsService::new();
        credentials
            .register(
                context,
                Arc::new(StaticCredentialProvider {
                    id: CredentialProviderId::new("anthropic-settings-test").unwrap(),
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
        context.provide(SERVICE_CREDENTIALS, self.name(), credentials)
    }
}

struct StaticPrompt;

#[async_trait]
impl SecretPrompt for StaticPrompt {
    async fn prompt(
        &self,
        _request: SecretPromptRequest,
        _cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        Ok(CredentialSecret::new("test-only"))
    }
}

struct AcceptingValidator;

#[async_trait]
impl ApiKeyValidator for AcceptingValidator {
    async fn validate(
        &self,
        _secret: &CredentialSecret,
        _cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        Ok(())
    }
}

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct CountTransport {
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl HttpTransport for CountTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().expect("count request body")).unwrap());
        Box::pin(async {
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                headers: BTreeMap::new(),
                body: serde_json::json!({
                    "input_tokens":25_000,
                    "context_management":{"original_input_tokens":70_000}
                })
                .to_string()
                .into_bytes(),
            })
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct HttpPlugin(Arc<CountTransport>);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), HttpService::new(self.0.clone()))
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("TEST_ANTHROPIC_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

fn config() -> AnthropicPluginConfig {
    AnthropicPluginConfig::new(
        query(),
        Arc::new(StaticPrompt),
        Arc::new(AcceptingValidator),
    )
}

fn world(documents: SettingsDocuments) -> Context {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        Box::new(CredentialsPlugin),
        heycode_authorization::authorization_plugin(),
        anthropic_plugin(config()),
    ];
    compose(&plugins).unwrap()
}

fn complete_explicit_value() -> serde_json::Value {
    serde_json::json!({
        "prompt_cache":{"mode":"automatic-1h"},
        "context_editing":{
            "thinking":{"mode":"keep-turns","keep_turns":2},
            "tools":{
                "mode":"enabled",
                "trigger":{"mode":"tool-uses","value":12},
                "keep":{"mode":"tool-uses","value":5},
                "clear_at_least":{"mode":"input-tokens","value":5000},
                "clear_tool_inputs":true,
                "exclude_tools":["memory","web_search"]
            }
        }
    })
}

#[test]
fn defaults_disable_both_features_and_use_no_numeric_sentinel() {
    let definition = anthropic_settings_definition().unwrap();
    let context = compose(&[settings_plugin(SettingsDocuments::new())]).unwrap();
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let snapshot = settings.register(&context, definition).unwrap();
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    let defaults = snapshot.resolved();
    assert_eq!(defaults["prompt_cache"]["mode"], "disabled");
    assert_eq!(defaults["context_editing"]["thinking"]["mode"], "disabled");
    assert_eq!(defaults["context_editing"]["thinking"]["keep_turns"], 1);
    assert_eq!(defaults["context_editing"]["tools"]["mode"], "disabled");
    assert_eq!(
        defaults["context_editing"]["tools"]["trigger"]["mode"],
        "provider-default"
    );
    assert_eq!(
        defaults["context_editing"]["tools"]["keep"]["mode"],
        "provider-default"
    );
    assert_eq!(
        defaults["context_editing"]["tools"]["clear_at_least"]["mode"],
        "disabled"
    );
    for field in ["trigger", "keep", "clear_at_least"] {
        assert!(
            defaults["context_editing"]["tools"][field]["value"]
                .as_u64()
                .is_some_and(|value| value > 0)
        );
    }
    let policies = AnthropicSettingsPolicies::from_snapshot(&snapshot).unwrap();
    assert!(policies.prompt_cache().is_none());
    assert!(policies.context_editing().is_none());
    assert!(snapshot.wire_projection().is_some());
}

#[test]
fn explicit_settings_map_every_control_and_align_provider_with_counter() {
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            anthropic_settings_namespace().unwrap(),
            complete_explicit_value(),
        )
        .unwrap();
    let context = world(documents);
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let policies = AnthropicSettingsPolicies::resolve(&settings).unwrap();
    assert_eq!(
        policies.prompt_cache().unwrap().ttl(),
        AnthropicPromptCacheTtl::OneHour
    );
    assert_eq!(
        policies.context_editing().unwrap().request_fields(),
        &serde_json::json!({"context_management":{"edits":[
            {"type":"clear_thinking_20251015",
                "keep":{"type":"thinking_turns","value":2}},
            {"type":"clear_tool_uses_20250919",
                "trigger":{"type":"tool_uses","value":12},
                "keep":{"type":"tool_uses","value":5},
                "clear_at_least":{"type":"input_tokens","value":5000},
                "exclude_tools":["memory","web_search"],
                "clear_tool_inputs":true}
        ]}})
    );

    let provider = policies
        .apply_provider(
            AnthropicProvider::new(HttpService::new(Arc::new(DeadTransport)), "test-key", None)
                .unwrap(),
        )
        .unwrap();
    let options = Provider::request_options(&provider);
    assert_eq!(
        options
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        ["context-editing", "prompt-cache"]
    );

    let counter =
        policies.apply_token_counter_config(AnthropicTokenCounterConfig::official(query()));
    assert_eq!(counter.context_editing_policy(), policies.context_editing());
    assert_eq!(counter.prompt_cache_policy(), policies.prompt_cache());
}

#[test]
fn five_minute_cache_and_input_token_trigger_are_distinct_explicit_modes() {
    let mut value = complete_explicit_value();
    value["prompt_cache"]["mode"] = serde_json::json!("automatic-5m");
    value["context_editing"]["tools"]["trigger"] =
        serde_json::json!({"mode":"input-tokens","value":50_000});
    let policies = AnthropicSettingsPolicies::from_value(&value).unwrap();
    assert_eq!(
        policies.prompt_cache().unwrap().ttl(),
        AnthropicPromptCacheTtl::FiveMinutes
    );
    assert_eq!(
        policies.context_editing().unwrap().request_fields()["context_management"]["edits"][1]["trigger"],
        serde_json::json!({"type":"input_tokens","value":50_000})
    );
}

#[test]
fn keep_all_thinking_can_enable_context_editing_without_tool_clearing() {
    let value = serde_json::json!({
        "prompt_cache":{"mode":"disabled"},
        "context_editing":{
            "thinking":{"mode":"keep-all","keep_turns":1},
            "tools":{
                "mode":"disabled",
                "trigger":{"mode":"provider-default","value":100000},
                "keep":{"mode":"provider-default","value":3},
                "clear_at_least":{"mode":"disabled","value":1},
                "clear_tool_inputs":false,
                "exclude_tools":[]
            }
        }
    });
    let policies = AnthropicSettingsPolicies::from_value(&value).unwrap();
    assert!(policies.prompt_cache().is_none());
    assert_eq!(
        policies.context_editing().unwrap().request_fields(),
        &serde_json::json!({"context_management":{"edits":[
            {"type":"clear_thinking_20251015","keep":"all"}
        ]}})
    );
}

#[test]
fn inactive_or_explicit_modes_never_hide_zero_or_stale_values() {
    let invalid = [
        serde_json::json!({
            "prompt_cache":{"mode":"disabled"},
            "context_editing":{
                "thinking":{"mode":"keep-turns","keep_turns":0},
                "tools":{"mode":"disabled",
                    "trigger":{"mode":"provider-default","value":100000},
                    "keep":{"mode":"provider-default","value":3},
                    "clear_at_least":{"mode":"disabled","value":1},
                    "clear_tool_inputs":false,"exclude_tools":[]}
            }
        }),
        serde_json::json!({
            "prompt_cache":{"mode":"disabled"},
            "context_editing":{
                "thinking":{"mode":"disabled","keep_turns":1},
                "tools":{"mode":"enabled",
                    "trigger":{"mode":"input-tokens","value":0},
                    "keep":{"mode":"provider-default","value":3},
                    "clear_at_least":{"mode":"disabled","value":1},
                    "clear_tool_inputs":false,"exclude_tools":[]}
            }
        }),
        serde_json::json!({
            "prompt_cache":{"mode":"automatic"},
            "context_editing":{
                "thinking":{"mode":"disabled","keep_turns":1},
                "tools":{"mode":"disabled",
                    "trigger":{"mode":"provider-default","value":100000},
                    "keep":{"mode":"provider-default","value":3},
                    "clear_at_least":{"mode":"disabled","value":1},
                    "clear_tool_inputs":false,"exclude_tools":[]}
            }
        }),
        serde_json::json!({
            "prompt_cache":{"mode":"automatic-1h"},
            "context_editing":{
                "thinking":{"mode":"disabled","keep_turns":1},
                "tools":{"mode":"enabled",
                    "trigger":{"mode":"tool-uses","value":12},
                    "keep":{"mode":"provider-default","value":3},
                    "clear_at_least":{"mode":"input-tokens","value":0},
                    "clear_tool_inputs":false,"exclude_tools":[]}
            }
        }),
    ];
    for value in invalid {
        assert_eq!(
            AnthropicSettingsPolicies::from_value(&value),
            Err(AnthropicSettingsError::InvalidSettings)
        );
    }
}

#[test]
fn provider_plugin_owns_effect_registration_and_verified_wire_exposure() {
    let mut context = world(SettingsDocuments::new());
    let settings = context
        .get::<SettingsService>(heycode_settings::SERVICE_SETTINGS)
        .unwrap();
    let namespace = anthropic_settings_namespace().unwrap();
    assert_eq!(namespace.as_str(), ANTHROPIC_SETTINGS_NAMESPACE);
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(snapshot.applies(), SettingsApplies::Restart);
    assert!(snapshot.wire_projection().is_some());
    let inventory = context.plugin_inventory().snapshot().unwrap();
    let rows = inventory
        .contributions
        .iter()
        .filter(|row| {
            row.plugin == "provider-anthropic"
                && row.kind == heycode_core::ContributionKind::SettingsNamespace
                && row.name == ANTHROPIC_SETTINGS_NAMESPACE
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);

    context.shutdown();
    assert!(settings.get(&namespace).unwrap().is_none());
}

#[test]
fn wire_exposure_refuses_credential_shaped_tool_metadata_without_echoing_it() {
    let canary = format!("sk-ant-api03-{}", "A".repeat(96));
    let mut value = complete_explicit_value();
    value["context_editing"]["tools"]["exclude_tools"] = serde_json::json!([canary.clone()]);
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(anthropic_settings_namespace().unwrap(), value)
        .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        Box::new(CredentialsPlugin),
        heycode_authorization::authorization_plugin(),
        anthropic_plugin(config()),
    ];
    let error = compose(&plugins).err().expect("wire exposure must fail");
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(&canary));
    assert!(rendered.contains("wire exposure"));
}

#[tokio::test]
async fn token_counter_plugin_resolves_the_same_registered_policy_generation() {
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            anthropic_settings_namespace().unwrap(),
            complete_explicit_value(),
        )
        .unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let transport = Arc::new(CountTransport {
        bodies: bodies.clone(),
    });
    let plugins: Vec<Box<dyn Plugin>> = vec![
        settings_plugin(documents),
        Box::new(ConfiguredCredentialsPlugin),
        heycode_authorization::authorization_plugin(),
        anthropic_plugin(config()),
        Box::new(HttpPlugin(transport)),
        heycode_llm::token_counters_plugin(),
        anthropic_token_counter_plugin(AnthropicTokenCounterConfig::official(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let registry = context
        .get::<TokenCounterRegistry>(heycode_llm::SERVICE_TOKEN_COUNTERS)
        .unwrap();
    let message = heycode_llm::ChatMessage::user("continue");
    let request = TokenCountRequest::new("anthropic", "claude-opus-5")
        .unwrap()
        .with_message(&message);
    let outcome = registry
        .count(&request, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        outcome.count().evidence(),
        heycode_llm::TokenEvidence::Estimated(heycode_llm::EstimationMethod::ProviderTokenizer)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["cache_control"]["ttl"], "1h");
    assert_eq!(
        bodies[0]["context_management"]["edits"][1]["trigger"],
        serde_json::json!({"type":"tool_uses","value":12})
    );
    drop(bodies);
    context.shutdown();
}
