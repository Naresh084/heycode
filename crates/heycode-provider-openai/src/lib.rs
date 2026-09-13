//! OpenAI product profile, authorization/cache-policy contributions and model
//! catalog.
//!
//! POA01 owns everything around the reusable `OpenAiResponsesAdapter` that P03
//! already implements: the provider profile, the provider-owned API-key flow
//! and the authenticated live catalog. The documented auth form is a bearer
//! header, and the documented model-list endpoint takes no query parameters.

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
use heycode_settings::{SERVICE_SETTINGS, SettingsService};
use tokio_util::sync::CancellationToken;

mod catalog;
mod compaction;
mod configured_tools;
mod hosted_tools;
mod inference;
mod product_policy;
mod prompt_cache;

pub use catalog::{OPENAI_GPT_5_6_SOL, OpenAiCatalog, OpenAiCatalogConfig, openai_catalog_plugin};
pub use compaction::{
    OpenAiCompactionCheckpoint, OpenAiCompactionClient, OpenAiCompactionFault,
    openai_compaction_support,
};
pub use configured_tools::{
    OPENAI_HOSTED_TOOLS_SETTINGS_NAMESPACE, OPENAI_UNOWNED_UPPER_LOOP_KINDS,
    OpenAiConfiguredHostedToolPolicy, OpenAiHostedToolSettingsFault,
    openai_configured_native_tools_plugin, openai_hosted_tools_settings_definition,
    openai_hosted_tools_settings_namespace,
};
pub use hosted_tools::{
    OPENAI_HOSTED_TOOLS_OPTION_KIND, OpenAiHostedToolDefinition, OpenAiHostedToolEvent,
    OpenAiHostedToolFault, OpenAiHostedToolItemRole, OpenAiHostedToolKind, OpenAiHostedToolOutcome,
    OpenAiHostedTools, classify_hosted_tool_citations, classify_hosted_tool_item,
    hosted_tool_support,
};
pub use inference::{OpenAiProvider, openai_provider};
pub use product_policy::{
    OPENAI_BRIDGE_COMPLETE_HOSTED_TOOL_KINDS, OpenAiHostedToolProductPolicy,
    configure_openai_bridge_complete_hosted_tools, openai_bridge_complete_native_tools_plugin,
};
pub use prompt_cache::{
    OpenAiCacheActivity, OpenAiCacheUsage, OpenAiPromptCacheControl, OpenAiPromptCacheFault,
    OpenAiPromptCacheMode, OpenAiPromptCachePolicy, OpenAiPromptCachePolicyFault,
    openai_prompt_cache_settings_definition, openai_prompt_cache_settings_namespace,
    openai_prompt_cache_support, resolve_openai_prompt_cache_policy,
};

/// Stable OpenAI authorization-flow id.
pub const OPENAI_FLOW_ID: &str = "openai-api-key";
/// Provider-owned non-secret credential reference.
pub const OPENAI_API_KEY_REFERENCE: &str = "OPENAI_API_KEY";

const VALIDATION_RESPONSE_LIMIT: usize = 256 * 1024;

/// Safe OpenAI identity/default metadata shared by setup and routing.
#[must_use]
pub fn openai_profile() -> heycode_llm::ProviderProfile {
    heycode_llm::ProviderProfile {
        registry_name: catalog::OPENAI_PROVIDER.to_owned(),
        descriptor: catalog::provider_descriptor(),
        default_model: OPENAI_GPT_5_6_SOL.to_owned(),
        credential_reference: Some(OPENAI_API_KEY_REFERENCE.to_owned()),
    }
}

/// Outcome of one validation request, before it is attributed to an endpoint.
enum Probe {
    Accepted,
    Missing,
    Failed(ApiKeyValidationFailure),
}

/// Live OpenAI API-key validator.
///
/// Key acceptance and model entitlement are separate proofs: a `200` on the
/// model list only shows the key is accepted, so a configured model is checked
/// against its own exact lookup (GOTCHAS #38).
pub struct OpenAiApiKeyValidator {
    http: HttpService,
    list_url: String,
    model_url: Option<String>,
}

impl OpenAiApiKeyValidator {
    /// Build against the official OpenAI API origin.
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

    /// Build against an explicit OpenAI-compatible API base URL.
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
        let list_url = catalog::models_url(base_url);
        let model_url = match required_model {
            Some(model) if catalog::safe_model_id(&model) => Some(format!("{list_url}/{model}")),
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
            .and_then(|request| request.header("authorization", &catalog::bearer(secret.expose())))
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
impl ApiKeyValidator for OpenAiApiKeyValidator {
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

/// Provider-owned OpenAI authorization plugin configuration.
#[derive(Clone)]
pub struct OpenAiPluginConfig {
    query: CredentialQuery,
    prompt: Arc<dyn SecretPrompt>,
    validator: Arc<dyn ApiKeyValidator>,
}

impl OpenAiPluginConfig {
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

    /// Build the official OpenAI validation plan.
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
            OpenAiApiKeyValidator::new(HttpService::new(Arc::new(transport)), required_model)?;
        Ok(Self::new(query, prompt, Arc::new(validator)))
    }

    /// [`Self::official`] whose validation probe goes to `base_url`
    /// (`llm.base_url`: a proxy or gateway) instead of the official host, so a
    /// key issued for the gateway is never sent to OpenAI to be checked.
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
        let validator = OpenAiApiKeyValidator::with_base_url(
            HttpService::new(Arc::new(transport)),
            base_url,
            required_model,
        )?;
        Ok(Self::new(query, prompt, Arc::new(validator)))
    }

    fn flow(&self) -> Result<ApiKeyAuthorizationFlow, CoreError> {
        let id = heycode_authorization::AuthorizationFlowId::new(OPENAI_FLOW_ID)
            .map_err(|error| CoreError::other(error.to_string()))?;
        Ok(ApiKeyAuthorizationFlow::new(
            ApiKeyFlowConfig {
                id,
                label: "OpenAI API key".to_owned(),
                query: self.query.clone(),
                prompt: "Paste your OpenAI API key".to_owned(),
            },
            self.prompt.clone(),
            self.validator.clone(),
        ))
    }
}

/// Register provider-owned OpenAI authorization and prompt-cache Settings.
#[must_use]
pub fn openai_plugin(config: OpenAiPluginConfig) -> Box<dyn Plugin> {
    struct OpenAiPlugin(OpenAiPluginConfig);

    impl Plugin for OpenAiPlugin {
        fn name(&self) -> &'static str {
            "provider-openai"
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
                    OPENAI_FLOW_ID,
                ),
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::SettingsNamespace,
                    "openai-prompt-cache",
                ),
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION, SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let authorization = context
                .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
                .ok_or_else(|| CoreError::MissingService(SERVICE_AUTHORIZATION.to_string()))?;
            let settings = context
                .get::<SettingsService>(SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_SETTINGS.to_string()))?;
            settings
                .register(
                    context,
                    prompt_cache::openai_prompt_cache_settings_definition()
                        .map_err(|_| CoreError::other("OpenAI prompt-cache schema is invalid"))?,
                )
                .map_err(|_| CoreError::other("OpenAI prompt-cache settings are invalid"))?;
            authorization
                .register(context, Arc::new(self.0.flow()?))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(OpenAiPlugin(config))
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
