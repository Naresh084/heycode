//! Anthropic product profile, authorization contribution and model catalog.
//!
//! PAN01 owns everything around the reusable `AnthropicMessagesAdapter` that
//! P05 already implements: the provider profile, the provider-owned API-key
//! flow and the authenticated live catalog. Native `x-api-key` plus the
//! required `anthropic-version` header are used everywhere; no bearer dialect
//! is offered here.
//!
//! PAN02 adds the dispatching route itself and the thinking-state requirement
//! it enforces on tool continuation; see [`inference`].
//! PAN04–PAN06 add provider-native compaction, context-editing and prompt-cache
//! request/response bridges without moving durable session authority into this
//! crate.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization::{
    AuthorizationFlowFailure, AuthorizationService, SERVICE_AUTHORIZATION,
};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, ApiKeyValidationFailure, ApiKeyValidator,
    SecretPrompt,
};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialQuery, CredentialSecret};
use heycode_http::{HttpRequest, HttpResponse, HttpService, ReqwestHttpTransport, TransportError};
use tokio_util::sync::CancellationToken;

mod catalog;
mod compaction;
mod configured_tools;
mod context_editing;
mod inference;
mod product_policy;
mod prompt_cache;
mod response_metadata;
mod server_tools;
mod settings;
mod token_count;
mod wire;

pub use token_count::{
    ANTHROPIC_TOKEN_COUNTER_ID, AnthropicTokenCountReport, AnthropicTokenCounter,
    AnthropicTokenCounterConfig, anthropic_token_counter_plugin,
};

pub use catalog::{
    ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_VERSION, AnthropicCatalog, AnthropicCatalogConfig,
    anthropic_catalog_plugin,
};
pub use compaction::{
    ANTHROPIC_COMPACTION_BETA, AnthropicCompactionCheckpoint, AnthropicCompactionDefinition,
    AnthropicCompactionFault, anthropic_compaction_support,
};
pub use configured_tools::{
    ANTHROPIC_EXCLUDED_DEFAULT_SERVER_TOOL_KINDS, ANTHROPIC_SERVER_TOOLS_SETTINGS_NAMESPACE,
    AnthropicConfiguredServerToolPolicy, AnthropicServerToolSettingsFault,
    anthropic_configured_native_tools_plugin, anthropic_server_tools_settings_definition,
    anthropic_server_tools_settings_namespace,
};
pub use context_editing::{
    ANTHROPIC_CONTEXT_EDITING_BETA, ANTHROPIC_CONTEXT_EDITING_OPTION_KIND,
    AnthropicAppliedContextEdit, AnthropicCacheImpact, AnthropicContextEditKind,
    AnthropicContextEditReport, AnthropicContextEditingFault, AnthropicContextEditingPolicy,
    AnthropicThinkingClear, AnthropicThinkingKeep, AnthropicToolClear, AnthropicToolClearTrigger,
    anthropic_context_editing_support,
};

pub use inference::{
    ANTHROPIC_DEFAULT_REASONING_EFFORT, ANTHROPIC_REASONING_EFFORTS, AnthropicProvider,
    AnthropicThinkingContinuation, INTERLEAVED_THINKING_BETA, anthropic_provider,
};
pub use product_policy::{
    ANTHROPIC_DEFAULT_ATOMIC_SERVER_TOOL_KINDS, AnthropicMaintainedDefaultServerToolPolicy,
    anthropic_default_native_tools_plugin, configure_anthropic_default_server_tools,
};
pub use prompt_cache::{
    ANTHROPIC_PROMPT_CACHE_OPTION_KIND, AnthropicCacheActivity, AnthropicCacheUsage,
    AnthropicPromptCacheFault, AnthropicPromptCachePolicy, AnthropicPromptCacheTtl,
};
pub use response_metadata::AnthropicResponseMetadata;
pub use server_tools::{
    ANTHROPIC_ADVISOR_BETA, ANTHROPIC_MCP_CONNECTOR_BETA, ANTHROPIC_SERVER_TOOLS_OPTION_KIND,
    ANTHROPIC_WEB_FETCH_MAX_USES, ANTHROPIC_WEB_SEARCH_MAX_USES, AnthropicPendingPauseState,
    AnthropicServerToolCall, AnthropicServerToolClassification, AnthropicServerToolContinuation,
    AnthropicServerToolDefinition, AnthropicServerToolFault, AnthropicServerToolKind,
    AnthropicServerToolOutcome, AnthropicServerToolPlan, advisor_pair_support,
    historical_server_tool_beta_headers, server_tool_support,
};
pub use settings::{
    ANTHROPIC_SETTINGS_NAMESPACE, AnthropicSettingsError, AnthropicSettingsPolicies,
    anthropic_settings_definition, anthropic_settings_namespace,
};

/// Stable Anthropic authorization-flow id.
pub const ANTHROPIC_FLOW_ID: &str = "anthropic-api-key";
/// Provider-owned non-secret credential reference.
pub const ANTHROPIC_API_KEY_REFERENCE: &str = "ANTHROPIC_API_KEY";

const VALIDATION_RESPONSE_LIMIT: usize = 256 * 1024;

/// Safe Anthropic identity/default metadata shared by setup and routing.
#[must_use]
pub fn anthropic_profile() -> heycode_llm::ProviderProfile {
    heycode_llm::ProviderProfile {
        registry_name: catalog::ANTHROPIC_PROVIDER.to_owned(),
        descriptor: catalog::provider_descriptor(),
        default_model: ANTHROPIC_CLAUDE_OPUS_5.to_owned(),
        credential_reference: Some(ANTHROPIC_API_KEY_REFERENCE.to_owned()),
    }
}

/// Outcome of one validation request, before it is attributed to an endpoint.
enum Probe {
    Accepted,
    Missing,
    Failed(ApiKeyValidationFailure),
}

/// Live Anthropic API-key validator.
///
/// Key acceptance and model entitlement are separate proofs: a `200` on the
/// model list only shows the key is accepted, so a configured model is checked
/// against its own exact lookup (GOTCHAS #38).
pub struct AnthropicApiKeyValidator {
    http: HttpService,
    list_url: String,
    model_url: Option<String>,
}

impl AnthropicApiKeyValidator {
    /// Build against the official Anthropic API origin.
    ///
    /// # Errors
    /// An unusable endpoint or an unsafe configured model id fails with the
    /// safe host class.
    pub fn new(
        http: HttpService,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        Self::with_base_url(http, catalog::DEFAULT_BASE_URL, required_model)
    }

    /// Build against an explicit Anthropic-compatible API base URL.
    ///
    /// # Errors
    /// An unusable endpoint or an unsafe configured model id fails with the
    /// safe host class.
    pub fn with_base_url(
        http: HttpService,
        base_url: impl AsRef<str>,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let base_url = base_url.as_ref().trim_end_matches('/');
        let list_url = format!("{base_url}/v1/models?limit=1");
        let model_url = match required_model {
            Some(model) if catalog::safe_model_id(&model) => {
                Some(format!("{base_url}/v1/models/{model}"))
            }
            Some(_) => return Err(host_failure()),
            None => None,
        };
        for url in std::iter::once(&list_url).chain(model_url.as_ref()) {
            HttpRequest::get(url).map_err(|_| host_failure())?;
        }
        Ok(Self {
            http,
            list_url,
            model_url,
        })
    }

    async fn probe(
        &self,
        url: &str,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Probe {
        let request = HttpRequest::get(url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("x-api-key", secret.expose()))
            .and_then(|request| request.header("anthropic-version", ANTHROPIC_VERSION))
            .map(|request| request.with_max_response_bytes(VALIDATION_RESPONSE_LIMIT));
        let Ok(request) = request else {
            return Probe::Failed(ApiKeyValidationFailure::Host);
        };
        match self.http.send(request, cancellation).await {
            Ok(response) => classify_response(&response),
            Err(error) => Probe::Failed(map_transport_error(error)),
        }
    }
}

#[async_trait]
impl ApiKeyValidator for AnthropicApiKeyValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        if cancellation.is_cancelled() {
            return Err(ApiKeyValidationFailure::Cancelled);
        }
        match self
            .probe(&self.list_url, secret, cancellation.clone())
            .await
        {
            Probe::Accepted => {}
            // The model list always exists; a 404 here means the endpoint is
            // wrong, never that a model is missing.
            Probe::Missing => return Err(ApiKeyValidationFailure::Host),
            Probe::Failed(failure) => return Err(failure),
        }
        let Some(model_url) = self.model_url.as_ref() else {
            return Ok(());
        };
        if cancellation.is_cancelled() {
            return Err(ApiKeyValidationFailure::Cancelled);
        }
        match self.probe(model_url, secret, cancellation).await {
            Probe::Accepted => Ok(()),
            Probe::Missing => Err(ApiKeyValidationFailure::Model),
            Probe::Failed(failure) => Err(failure),
        }
    }
}

/// Provider-owned Anthropic authorization plugin configuration.
#[derive(Clone)]
pub struct AnthropicPluginConfig {
    query: CredentialQuery,
    prompt: Arc<dyn SecretPrompt>,
    validator: Arc<dyn ApiKeyValidator>,
}

impl AnthropicPluginConfig {
    /// Build from one exact credential query and replaceable interaction/
    /// validation providers.
    #[must_use]
    pub fn new(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        validator: Arc<dyn ApiKeyValidator>,
    ) -> Self {
        Self {
            query,
            prompt,
            validator,
        }
    }

    /// Build the official Anthropic validation plan.
    ///
    /// # Errors
    /// Fixed endpoint/transport construction failures retain the safe host
    /// class.
    pub fn official(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let transport = ReqwestHttpTransport::new().map_err(|_| host_failure())?;
        let validator =
            AnthropicApiKeyValidator::new(HttpService::new(Arc::new(transport)), required_model)?;
        Ok(Self::new(query, prompt, Arc::new(validator)))
    }

    /// [`Self::official`] whose validation probe goes to `base_url`
    /// (`llm.base_url`: a proxy or gateway) instead of the official host, so a
    /// key issued for the gateway is never sent to Anthropic to be checked.
    ///
    /// # Errors
    /// Endpoint/transport construction failures retain the safe host class.
    pub fn at_base_url(
        query: CredentialQuery,
        prompt: Arc<dyn SecretPrompt>,
        base_url: &str,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let transport = ReqwestHttpTransport::new().map_err(|_| host_failure())?;
        let validator = AnthropicApiKeyValidator::with_base_url(
            HttpService::new(Arc::new(transport)),
            base_url,
            required_model,
        )?;
        Ok(Self::new(query, prompt, Arc::new(validator)))
    }

    fn flow(&self) -> Result<ApiKeyAuthorizationFlow, CoreError> {
        let id = heycode_authorization::AuthorizationFlowId::new(ANTHROPIC_FLOW_ID)
            .map_err(|error| CoreError::other(error.to_string()))?;
        Ok(ApiKeyAuthorizationFlow::new(
            ApiKeyFlowConfig {
                id,
                label: "Anthropic API key".to_owned(),
                query: self.query.clone(),
                prompt: "Paste your Anthropic API key".to_owned(),
            },
            self.prompt.clone(),
            self.validator.clone(),
        ))
    }
}

/// Register provider-owned Anthropic authorization and Settings contributions.
#[must_use]
pub fn anthropic_plugin(config: AnthropicPluginConfig) -> Box<dyn Plugin> {
    struct AnthropicPlugin(AnthropicPluginConfig);

    impl Plugin for AnthropicPlugin {
        fn name(&self) -> &'static str {
            "provider-anthropic"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    PluginContributionKind::Provider,
                    PluginContributionKind::Service,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::AuthorizationFlow,
                    ANTHROPIC_FLOW_ID,
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    ANTHROPIC_SETTINGS_NAMESPACE,
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION, heycode_settings::SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let authorization = context
                .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
                .ok_or_else(|| CoreError::MissingService(SERVICE_AUTHORIZATION.to_string()))?;
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| {
                    CoreError::MissingService(heycode_settings::SERVICE_SETTINGS.to_string())
                })?;
            settings
                .register(
                    context,
                    anthropic_settings_definition()
                        .map_err(|error| CoreError::other(error.to_string()))?,
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            authorization
                .register(context, Arc::new(self.0.flow()?))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(AnthropicPlugin(config))
}

/// Classify one validation response without reading its body.
fn classify_response(response: &HttpResponse) -> Probe {
    match response.status {
        200..=299 => {
            if response.content_type.as_deref().is_some_and(|value| {
                value == "application/json"
                    || value.starts_with("application/json;")
                    || value.ends_with("+json")
            }) {
                Probe::Accepted
            } else {
                Probe::Failed(ApiKeyValidationFailure::Host)
            }
        }
        401 | 403 => Probe::Failed(ApiKeyValidationFailure::Unauthorized),
        404 => Probe::Missing,
        429 | 500..=599 => Probe::Failed(ApiKeyValidationFailure::Network),
        _ => Probe::Failed(ApiKeyValidationFailure::Host),
    }
}

fn map_transport_error(error: TransportError) -> ApiKeyValidationFailure {
    match error {
        TransportError::Cancelled => ApiKeyValidationFailure::Cancelled,
        TransportError::Http {
            status: 401 | 403, ..
        } => ApiKeyValidationFailure::Unauthorized,
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            ApiKeyValidationFailure::Network
        }
        TransportError::Network { .. } | TransportError::Timeout => {
            ApiKeyValidationFailure::Network
        }
        _ => ApiKeyValidationFailure::Host,
    }
}

fn host_failure() -> AuthorizationFlowFailure {
    AuthorizationFlowFailure::new("host", "validation endpoint or response is invalid")
}
