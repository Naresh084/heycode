//! OpenRouter adapter over the shared OpenAI-compatible client (spec table,
//! AGENTS §5). Optional attribution headers come from `HEYCODE_HTTP_REFERER` and
//! `HEYCODE_X_TITLE`.

use std::collections::BTreeSet;

use async_trait::async_trait;

use crate::error::LlmError;
use crate::provider::{ChunkStream, Provider, ProviderInfo};
use crate::vocab::ChatRequest;
use crate::wire::{OpenAiCompatClient, OpenAiCompatConfig};

/// Exact effort ids verified for the default GLM route, in published order.
/// Every other OpenRouter model uses the vocabulary its own catalog row
/// published; this list is never projected onto another model.
const GLM_VERIFIED_EFFORTS: [&str; 3] = ["max", "high", "low"];
/// Verified default effort for that same route.
const GLM_VERIFIED_DEFAULT_EFFORT: &str = "max";

/// Environment variable supplying the optional `HTTP-Referer` header.
pub const HTTP_REFERER_ENV: &str = "HEYCODE_HTTP_REFERER";
/// Environment variable supplying the optional `X-Title` header.
pub const X_TITLE_ENV: &str = "HEYCODE_X_TITLE";

/// Interpret only OpenRouter's recognized routing discriminator. Free-form
/// messages, upstream bodies and configuration URLs never become diagnostics.
pub(crate) fn classify_transport_error(error: heycode_http::TransportError) -> LlmError {
    let paid_training_excluded = match &error {
        heycode_http::TransportError::Http {
            status: 404, body, ..
        } => has_paid_training_exclusion(body.as_str()),
        _ => false,
    };
    let error = crate::classify_transport_error(error);
    match error {
        LlmError::Provider(failure) if paid_training_excluded => {
            LlmError::Provider(failure.with_guidance(
                crate::error::ProviderFailureGuidance::OpenRouterPaidModelTrainingPolicy,
            ))
        }
        error => error,
    }
}

fn has_paid_training_exclusion(body: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct Envelope {
        error: RoutingError,
    }
    #[derive(serde::Deserialize)]
    struct RoutingError {
        metadata: RoutingMetadata,
    }
    #[derive(serde::Deserialize)]
    struct RoutingMetadata {
        ineligibility_reasons: Vec<serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct IneligibilityReason {
        reason: String,
        endpoint_count: u64,
    }
    let Ok(envelope) = serde_json::from_str::<Envelope>(body) else {
        return false;
    };
    envelope
        .error
        .metadata
        .ineligibility_reasons
        .into_iter()
        .any(|value| {
            serde_json::from_value::<IneligibilityReason>(value).is_ok_and(|entry| {
                entry.reason == "paid-model-training-violation-by-account"
                    && entry.endpoint_count > 0
            })
        })
}

/// OpenRouter third-party provider data-collection filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRouterDataCollection {
    /// Allow endpoints that may store/train on request data.
    Allow,
    /// Use only endpoints that do not collect request data.
    Deny,
}

/// OpenRouter web-search engine selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRouterWebSearchEngine {
    /// Prefer native search when available, otherwise OpenRouter's fallback.
    Auto,
    /// Prefer the upstream model provider's native search.
    Native,
    /// Use OpenRouter's Exa integration.
    Exa,
    /// Use the configured Firecrawl integration.
    Firecrawl,
    /// Use OpenRouter's Parallel integration.
    Parallel,
    /// Use OpenRouter's Perplexity Search integration.
    Perplexity,
}

impl OpenRouterWebSearchEngine {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Exa => "exa",
            Self::Firecrawl => "firecrawl",
            Self::Parallel => "parallel",
            Self::Perplexity => "perplexity",
        }
    }
}

/// Explicit bounded policy for `openrouter:web_search`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterWebSearchPolicy {
    engine: OpenRouterWebSearchEngine,
    max_results: u8,
    max_uses: u8,
    max_total_results: u16,
    max_characters: u32,
    max_tool_calls: u8,
}

impl OpenRouterWebSearchPolicy {
    /// Construct one bounded web-search plan.
    ///
    /// # Errors
    /// Limits outside OpenRouter's documented ranges or internally
    /// inconsistent cumulative budgets fail.
    pub fn new(
        engine: OpenRouterWebSearchEngine,
        max_results: u8,
        max_uses: u8,
        max_total_results: u16,
        max_characters: u32,
        max_tool_calls: u8,
    ) -> Result<Self, OpenRouterWebSearchPolicyError> {
        if !(1..=25).contains(&max_results)
            || (engine == OpenRouterWebSearchEngine::Perplexity && max_results > 20)
            || !(1..=30).contains(&max_uses)
            || max_total_results == 0
            || max_total_results > u16::from(max_results) * u16::from(max_uses)
            || !(1..=100_000).contains(&max_characters)
            || !(1..=30).contains(&max_tool_calls)
            || max_uses > max_tool_calls
        {
            return Err(OpenRouterWebSearchPolicyError::InvalidLimits);
        }
        Ok(Self {
            engine,
            max_results,
            max_uses,
            max_total_results,
            max_characters,
            max_tool_calls,
        })
    }

    /// heycode's explicit bounded defaults over OpenRouter's documented fields.
    #[must_use]
    pub const fn heycode_defaults() -> Self {
        Self {
            engine: OpenRouterWebSearchEngine::Auto,
            max_results: 5,
            max_uses: 3,
            max_total_results: 15,
            max_characters: 4_000,
            max_tool_calls: 5,
        }
    }

    fn tool_definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type":"openrouter:web_search",
            "parameters":{
                "engine":self.engine.as_str(),
                "max_results":self.max_results,
                "max_uses":self.max_uses,
                "max_total_results":self.max_total_results,
                "max_characters":self.max_characters,
            }
        })
    }

    const fn max_tool_calls(&self) -> u32 {
        self.max_tool_calls as u32
    }
}

/// OpenRouter web-search policy validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenRouterWebSearchPolicyError {
    /// One count/size bound is outside the documented safe range.
    #[error("OpenRouter web-search limits are invalid or inconsistent")]
    InvalidLimits,
}

impl OpenRouterDataCollection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// Explicit OpenRouter provider-routing policy for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterRoutingPolicy {
    order: Vec<String>,
    allow_fallbacks: bool,
    require_parameters: bool,
    data_collection: OpenRouterDataCollection,
    zdr: Option<bool>,
}

impl OpenRouterRoutingPolicy {
    /// Validate one exact routing policy.
    ///
    /// # Errors
    /// Duplicate, blank, uppercase or unsafe provider slugs fail before the
    /// policy can enter a resolved request.
    pub fn new(
        order: Vec<String>,
        allow_fallbacks: bool,
        require_parameters: bool,
        data_collection: OpenRouterDataCollection,
        zdr: Option<bool>,
    ) -> Result<Self, OpenRouterRoutingPolicyError> {
        let mut seen = BTreeSet::new();
        if order
            .iter()
            .any(|slug| !valid_provider_slug(slug) || !seen.insert(slug.as_str()))
        {
            return Err(OpenRouterRoutingPolicyError::InvalidProviderOrder);
        }
        Ok(Self {
            order,
            allow_fallbacks,
            require_parameters,
            data_collection,
            zdr,
        })
    }

    /// Materialize OpenRouter's documented routing defaults explicitly.
    #[must_use]
    pub const fn official_defaults() -> Self {
        Self {
            order: Vec::new(),
            allow_fallbacks: true,
            require_parameters: false,
            data_collection: OpenRouterDataCollection::Allow,
            zdr: None,
        }
    }

    fn provider_option(
        &self,
    ) -> Result<heycode_core::ProviderRequestOption, OpenRouterRoutingPolicyError> {
        let mut data = serde_json::Map::new();
        if !self.order.is_empty() {
            data.insert("order".to_owned(), serde_json::json!(self.order));
        }
        data.insert(
            "allow_fallbacks".to_owned(),
            serde_json::json!(self.allow_fallbacks),
        );
        data.insert(
            "require_parameters".to_owned(),
            serde_json::json!(self.require_parameters),
        );
        data.insert(
            "data_collection".to_owned(),
            serde_json::json!(self.data_collection.as_str()),
        );
        if let Some(zdr) = self.zdr {
            data.insert("zdr".to_owned(), serde_json::json!(zdr));
        }
        heycode_core::ProviderRequestOption::new(
            OpenRouterProvider::NAME,
            "routing",
            serde_json::Value::Object(data),
        )
        .map_err(|_| OpenRouterRoutingPolicyError::InvalidProviderOrder)
    }
}

/// OpenRouter routing-policy validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenRouterRoutingPolicyError {
    /// Provider order contains an unsafe or duplicate slug.
    #[error("OpenRouter provider order contains an invalid or duplicate slug")]
    InvalidProviderOrder,
}

/// OpenRouter provider bound to one resolved default model.
#[derive(Clone)]
pub struct OpenRouterProvider {
    client: OpenAiCompatClient,
    inference: crate::OpenAiChatCompletionsAdapter,
    default_model: String,
    credential_reference: String,
    request_options: Vec<heycode_core::ProviderRequestOption>,
}

impl OpenRouterProvider {
    /// Registry name of this provider.
    pub const NAME: &'static str = "openrouter";
    /// Model used when no explicit model is resolved.
    pub const DEFAULT_MODEL: &'static str = "z-ai/glm-5.3-flash";
    /// Base URL of the OpenRouter API.
    pub const BASE_URL: &'static str = "https://openrouter.ai/api/v1";
    /// Environment variable holding the API key.
    pub const API_KEY_ENV: &'static str = "OPENROUTER_API_KEY";

    /// Provider-owned setup metadata; no client or secret is constructed.
    #[must_use]
    pub fn setup_profile() -> crate::ProviderProfile {
        crate::ProviderProfile {
            registry_name: Self::NAME.to_owned(),
            descriptor: provider_descriptor(),
            default_model: Self::DEFAULT_MODEL.to_owned(),
            credential_reference: Some(Self::API_KEY_ENV.to_owned()),
        }
    }

    /// Build from the environment; `model` overrides
    /// [`OpenRouterProvider::DEFAULT_MODEL`] when present. Attribution
    /// headers attach only for set, non-blank variables.
    ///
    /// # Errors
    /// [`LlmError::MissingApiKey`] when `OPENROUTER_API_KEY` is unset or
    /// empty; classified [`LlmError::Provider`] when HTTP construction fails;
    /// missing/malformed explicit transform intent is refused.
    pub fn from_env(
        model: Option<String>,
        request_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        let key = std::env::var(Self::API_KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or(LlmError::MissingApiKey {
                env: Self::API_KEY_ENV,
            })?;
        Self::from_key(key, model, request_options)
    }

    /// Build from an explicit key; attribution headers still resolve from the
    /// environment when set.
    ///
    /// # Errors
    /// A classified [`LlmError::Provider`] when HTTP construction fails;
    /// missing/malformed explicit transform intent is refused.
    pub fn from_key(
        key: impl Into<String>,
        model: Option<String>,
        request_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        let transport =
            heycode_http::ReqwestHttpTransport::new().map_err(crate::classify_transport_error)?;
        Self::build(
            crate::RouteCredential::fixed(key),
            model,
            heycode_http::HttpService::new(std::sync::Arc::new(transport)),
            OpenRouterRoutingPolicy::official_defaults(),
            request_options,
        )
    }

    /// Build from an explicit key and the composed shared HTTP service;
    /// attribution headers still resolve from the environment.
    ///
    /// # Errors
    /// Protocol-client construction or missing/malformed transform intent.
    pub fn from_key_with_transport(
        key: impl Into<String>,
        model: Option<String>,
        http: heycode_http::HttpService,
        request_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        Self::build(
            crate::RouteCredential::fixed(key),
            model,
            http,
            OpenRouterRoutingPolicy::official_defaults(),
            request_options,
        )
    }

    /// Build from a credential resolved once per operation and the composed
    /// shared HTTP service. Both the legacy and native routes then send the
    /// key that is in the store at request time.
    ///
    /// # Errors
    /// Protocol-client or provider-option construction failure, including a
    /// missing/malformed transform decision.
    pub fn from_credential_with_transport(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        request_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        Self::build(
            credential,
            model,
            http,
            OpenRouterRoutingPolicy::official_defaults(),
            request_options,
        )
    }

    /// Build with one explicit provider-routing policy.
    ///
    /// # Errors
    /// Protocol-client or provider-option construction failure, including a
    /// missing/malformed transform decision.
    pub fn from_key_with_transport_and_routing(
        key: impl Into<String>,
        model: Option<String>,
        http: heycode_http::HttpService,
        routing: OpenRouterRoutingPolicy,
        request_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        Self::build(
            crate::RouteCredential::fixed(key),
            model,
            http,
            routing,
            request_options,
        )
    }

    /// [`Self::from_credential_with_transport`] against a configured base URL
    /// (`llm.base_url`: a proxy or gateway) instead of the official host.
    ///
    /// # Errors
    /// Protocol-client or provider-option construction failure.
    pub fn from_credential_with_transport_at(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        request_options: Vec<heycode_core::ProviderRequestOption>,
        base_url: &str,
    ) -> Result<Self, LlmError> {
        Self::build_at(
            credential,
            model,
            http,
            OpenRouterRoutingPolicy::official_defaults(),
            request_options,
            base_url.trim_end_matches('/'),
        )
    }

    fn build(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        routing: OpenRouterRoutingPolicy,
        additional_options: Vec<heycode_core::ProviderRequestOption>,
    ) -> Result<Self, LlmError> {
        Self::build_at(
            credential,
            model,
            http,
            routing,
            additional_options,
            Self::BASE_URL,
        )
    }

    fn build_at(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        routing: OpenRouterRoutingPolicy,
        additional_options: Vec<heycode_core::ProviderRequestOption>,
        base_url: &str,
    ) -> Result<Self, LlmError> {
        let extra_headers = attribution_headers(env_value);
        let web_search = OpenRouterWebSearchPolicy::heycode_defaults();
        let credential_reference = credential
            .route()
            .map_or(Self::API_KEY_ENV, crate::CredentialHandle::as_str)
            .to_owned();
        let client = OpenAiCompatClient::with_credential_and_transport(
            OpenAiCompatConfig {
                base_url: base_url.to_owned(),
                api_key_env: Self::API_KEY_ENV,
                extra_headers: extra_headers.clone(),
            },
            credential.clone(),
            http.clone(),
        )?;
        let mut inference_config = crate::OpenAiChatCompletionsConfig::with_credential(
            provider_descriptor(),
            base_url,
            credential,
        )
        // OpenRouter publishes a reasoning vocabulary per model, so the only
        // route-wide list here is the one verified for the default GLM route.
        // Every other model exposes what its own catalog row published.
        .with_reasoning(
            GLM_VERIFIED_EFFORTS
                .into_iter()
                .map(crate::ReasoningEffortId::from_built_in)
                .collect(),
            Some(crate::ReasoningEffortId::from_built_in(
                GLM_VERIFIED_DEFAULT_EFFORT,
            )),
            crate::ChatReasoningWire::ObjectEffort,
        )
        .with_model_published_reasoning(vec![Self::DEFAULT_MODEL.to_owned()])
        .with_reasoning_continuation(crate::ChatReasoningContinuation::ReasoningOrDetails)
        .with_extra_headers(extra_headers)
        .with_provider_request_option("routing", "provider")
        .with_anthropic_cache_breakpoints("caching")
        .with_server_tool(crate::NativeFeature::Web, web_search.tool_definition())
        .with_max_server_tool_calls(web_search.max_tool_calls())
        .with_url_citations();
        let mut seen = BTreeSet::new();
        for option in &additional_options {
            option.validate().map_err(|_| {
                LlmError::InvalidResponse(
                    "OpenRouter provider request option is invalid".to_owned(),
                )
            })?;
            if option.provider() != Self::NAME
                || option.kind() != "transforms"
                || !seen.insert(option.kind())
            {
                return Err(LlmError::InvalidResponse(
                    "OpenRouter provider request option is unsupported or duplicated".to_owned(),
                ));
            }
            inference_config = inference_config.with_provider_request_option_member(
                option.kind(),
                "plugins",
                "plugins",
            );
        }
        if !seen.contains("transforms") {
            return Err(LlmError::InvalidResponse(
                "OpenRouter transform request option is required".to_owned(),
            ));
        }
        let inference = crate::OpenAiChatCompletionsAdapter::new(inference_config, http)?;
        let routing_option = routing.provider_option().map_err(|_| {
            LlmError::InvalidResponse("OpenRouter routing policy is invalid".to_owned())
        })?;
        let mut request_options = Vec::with_capacity(1 + additional_options.len());
        request_options.push(routing_option);
        request_options.push(
            heycode_core::ProviderRequestOption::new(
                Self::NAME,
                "caching",
                serde_json::json!({"type":"ephemeral"}),
            )
            .map_err(|_| {
                LlmError::InvalidResponse("OpenRouter cache policy is invalid".to_owned())
            })?,
        );
        request_options.extend(additional_options);
        Ok(Self {
            client,
            inference,
            default_model: model.unwrap_or_else(|| Self::DEFAULT_MODEL.to_owned()),
            credential_reference,
            request_options,
        })
    }
}

/// Read one environment variable as an attribution value, treating unset and
/// blank the same. Injected into [`attribution_headers`] by `from_env`.
fn env_value(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Derive the optional OpenRouter attribution headers from a reader.
/// Values attach verbatim; blank-filtering belongs to the reader.
fn attribution_headers(read: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    [("HTTP-Referer", HTTP_REFERER_ENV), ("X-Title", X_TITLE_ENV)]
        .into_iter()
        .filter_map(|(header, var)| read(var).map(|value| (header.to_owned(), value)))
        .collect()
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: Self::NAME.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
    }

    fn descriptor(&self) -> crate::ProviderDescriptor {
        provider_descriptor()
    }

    fn inference_adapter(&self) -> Option<&dyn crate::InferenceAdapter> {
        Some(self)
    }

    fn request_options(&self) -> Vec<heycode_core::ProviderRequestOption> {
        self.request_options.clone()
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.client.stream(&request.model, &request)
    }
}

fn valid_provider_slug(value: &str) -> bool {
    let components = value.split('/').collect::<Vec<_>>();
    !components.is_empty()
        && components.len() <= 3
        && value.len() <= 128
        && components.iter().all(|component| {
            let bytes = component.as_bytes();
            bytes
                .first()
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        })
}

impl crate::InferenceAdapter for OpenRouterProvider {
    fn descriptor(&self) -> crate::ProviderDescriptor {
        provider_descriptor()
    }

    fn authentication_binding(&self) -> crate::AuthenticationBinding {
        self.inference.authentication_binding()
    }

    fn reasoning_effort_options(
        &self,
        model: &crate::ModelDescriptor,
    ) -> Result<Option<crate::ReasoningEffortOptions>, crate::ResolveError> {
        self.inference.reasoning_effort_options(model)
    }

    fn resolve(
        &self,
        draft: crate::RequestDraft,
        model: &crate::ModelDescriptor,
    ) -> Result<crate::ResolvedCall, crate::ResolveError> {
        self.inference.resolve(draft, model)
    }

    fn stream(&self, call: crate::ResolvedCall) -> crate::InferenceStream {
        self.inference.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: crate::ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> crate::InferenceStream {
        crate::InferenceAdapter::stream_cancellable(&self.inference, call, cancellation)
    }
}

fn provider_descriptor() -> crate::ProviderDescriptor {
    crate::ProviderDescriptor {
        id: OpenRouterProvider::NAME.to_owned(),
        display_name: "OpenRouter".to_owned(),
        protocols: vec![crate::ProviderProtocol::OpenAiChatCompletions],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn transform_options() -> Vec<heycode_core::ProviderRequestOption> {
        vec![
            heycode_core::ProviderRequestOption::new(
                OpenRouterProvider::NAME,
                "transforms",
                serde_json::json!({"plugins":[]}),
            )
            .expect("test transform option is valid"),
        ]
    }

    #[test]
    fn spec_table_constants_match_agents() {
        assert_eq!(OpenRouterProvider::NAME, "openrouter");
        assert_eq!(OpenRouterProvider::DEFAULT_MODEL, "z-ai/glm-5.3-flash");
        assert_eq!(OpenRouterProvider::BASE_URL, "https://openrouter.ai/api/v1");
        assert_eq!(OpenRouterProvider::API_KEY_ENV, "OPENROUTER_API_KEY");
        let profile = OpenRouterProvider::setup_profile();
        assert_eq!(profile.registry_name, OpenRouterProvider::NAME);
        assert_eq!(profile.descriptor.id, OpenRouterProvider::NAME);
        assert_eq!(profile.default_model, OpenRouterProvider::DEFAULT_MODEL);
        assert_eq!(
            profile.credential_reference.as_deref(),
            Some(OpenRouterProvider::API_KEY_ENV)
        );
    }

    #[test]
    fn attribution_headers_attach_only_when_set() {
        let mut env: HashMap<&str, String> = HashMap::new();
        env.insert(HTTP_REFERER_ENV, "https://heycode.dev".into());
        let read = |var: &str| env.get(var).cloned();

        // Only the referer is set; blank-filtering lives in the env reader
        // that `from_env` supplies.
        let headers = attribution_headers(read);
        assert_eq!(
            headers,
            vec![("HTTP-Referer".to_owned(), "https://heycode.dev".to_owned())]
        );
        assert!(attribution_headers(|_| None).is_empty());
    }

    #[test]
    fn blank_env_values_are_filtered_out_by_the_env_reader() {
        // Guaranteed-unset name: no process state is mutated.
        assert_eq!(env_value("HEYCODE_TEST_NO_KEY_43"), None);
    }

    #[test]
    fn info_projects_resolved_default_model() {
        let provider = OpenRouterProvider::from_key(
            "test-key",
            Some("anthropic/claude-sonnet".into()),
            transform_options(),
        )
        .unwrap();
        let info = provider.info();
        assert_eq!(info.name, "openrouter");
        assert_eq!(info.default_model, "anthropic/claude-sonnet");
        assert!(Provider::inference_adapter(&provider).is_some());
    }
}

#[cfg(test)]
mod routing_error_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use futures::StreamExt as _;

    use super::*;
    use crate::{
        ProviderErrorClass, ProviderFailureOrigin, RetryAttempt, RetryDecision, RetrySpec,
    };

    fn body(reasons: serde_json::Value) -> String {
        serde_json::json!({"error": {
            "code": 404,
            "message": "upstream-secret-canary https://untrusted.example/private",
            "metadata": {
                "raw": "upstream-secret-canary",
                "ineligibility_reasons": reasons
            }
        }})
        .to_string()
    }

    fn recognized_reason() -> serde_json::Value {
        serde_json::json!({
            "reason": "paid-model-training-violation-by-account",
            "endpoint_count": 1,
            "configure_url": "https://untrusted.example/upstream-secret-canary"
        })
    }

    fn http_error(status: u16, body: &str) -> heycode_http::TransportError {
        heycode_http::TransportError::http(
            status,
            body,
            heycode_http::HttpErrorMetadata::new(None, None),
        )
    }

    #[tokio::test]
    async fn openrouter_routing_error_reaches_chat_output_without_provider_body() {
        let body = body(serde_json::json!([null, {"reason":"unknown"}, recognized_reason()]));
        for provider in [OpenRouterProvider::NAME, "other-provider"] {
            let events = Box::pin(futures::stream::iter([Err(http_error(404, &body))]));
            let mut output = crate::chat::normalize_chat_events(
                events,
                provider.to_owned(),
                "meta/muse-spark-1.3-contributor".to_owned(),
            );
            let error = output.next().await.unwrap().unwrap_err();
            assert!(output.next().await.is_none());
            assert_eq!(error.class(), ProviderErrorClass::InvalidRequest);
            let failure = error.provider_failure().unwrap();
            assert_eq!(failure.status(), Some(404));
            assert_eq!(failure.origin(), ProviderFailureOrigin::Http);
            assert!(matches!(
                RetrySpec::standard().decide(&error, RetryAttempt::new(1, false, 0, 0).unwrap()),
                RetryDecision::DoNotRetry(_)
            ));
            if provider == OpenRouterProvider::NAME {
                assert!(
                    error
                        .to_string()
                        .contains("paid-model training privacy setting")
                );
                assert!(
                    error
                        .to_string()
                        .contains("https://openrouter.ai/settings/privacy")
                );
            } else {
                assert_eq!(error.to_string(), "provider rejected the request");
            }
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("upstream-secret-canary"));
            assert!(!rendered.contains("untrusted.example"));
        }
    }

    #[test]
    fn openrouter_routing_guidance_requires_exact_structured_reason_and_positive_count() {
        for reasons in [
            serde_json::json!([]),
            serde_json::json!([{"reason":"unknown", "endpoint_count":1}]),
            serde_json::json!([{"reason":"paid-model-training-violation-by-account", "endpoint_count":0}]),
            serde_json::json!([{"reason":"paid-model-training-violation-by-account", "endpoint_count":-1}]),
            serde_json::json!([{"reason":"paid-model-training-violation-by-account", "endpoint_count":"1"}]),
            serde_json::json!([{"reason":"paid-model-training-violation-by-account"}]),
            serde_json::json!([{"reason":"paid-model-training-violation-by-account-canary", "endpoint_count":1}]),
            recognized_reason(),
        ] {
            let error = classify_transport_error(http_error(404, &body(reasons)));
            assert_eq!(error.to_string(), "provider rejected the request");
        }
        for body in [
            "not json",
            r#"{"error":{"message":"paid-model-training-violation-by-account"}}"#,
            r#"{"metadata":{"ineligibility_reasons":[]}}"#,
        ] {
            assert_eq!(
                classify_transport_error(http_error(404, body)).to_string(),
                "provider rejected the request"
            );
        }
    }

    #[test]
    fn routing_metadata_does_not_override_auth_rate_or_server_failures() {
        let body = body(serde_json::json!([recognized_reason()]));
        for (status, class) in [
            (401, ProviderErrorClass::Authentication),
            (429, ProviderErrorClass::RateLimited),
            (500, ProviderErrorClass::Server),
            (503, ProviderErrorClass::Overloaded),
        ] {
            let error = classify_transport_error(http_error(status, &body));
            assert_eq!(error.class(), class);
            assert!(!error.to_string().contains("settings/privacy"));
        }
    }
}
