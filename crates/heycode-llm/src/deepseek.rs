//! DeepSeek adapter over the shared OpenAI-compatible client (spec table,
//! AGENTS §5). Reasoning models surface `reasoning_content` deltas, which the
//! client already maps to [`crate::StreamChunk::ReasoningDelta`].

use async_trait::async_trait;

use crate::error::LlmError;
use crate::provider::{ChunkStream, Provider, ProviderInfo};
use crate::vocab::ChatRequest;
use crate::wire::{OpenAiCompatClient, OpenAiCompatConfig};

/// DeepSeek provider bound to one resolved default model.
#[derive(Clone)]
pub struct DeepSeekProvider {
    client: OpenAiCompatClient,
    inference: crate::OpenAiChatCompletionsAdapter,
    default_model: String,
    credential_reference: String,
}

impl DeepSeekProvider {
    /// Registry name of this provider.
    pub const NAME: &'static str = "deepseek";
    /// Model used when no explicit model is resolved.
    pub const DEFAULT_MODEL: &'static str = "deepseek-v4-flash";
    /// Base URL of the DeepSeek API.
    pub const BASE_URL: &'static str = "https://api.deepseek.com";
    /// Environment variable holding the API key.
    pub const API_KEY_ENV: &'static str = "DEEPSEEK_API_KEY";

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
    /// [`DeepSeekProvider::DEFAULT_MODEL`] when present.
    ///
    /// # Errors
    /// [`LlmError::MissingApiKey`] when `DEEPSEEK_API_KEY` is unset or empty;
    /// A classified [`LlmError::Provider`] when HTTP construction fails.
    pub fn from_env(model: Option<String>) -> Result<Self, LlmError> {
        let key = std::env::var(Self::API_KEY_ENV)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or(LlmError::MissingApiKey {
                env: Self::API_KEY_ENV,
            })?;
        Self::from_key(key, model)
    }

    /// Build from an explicit key (credentials-file ladder); same overrides
    /// as [`Self::from_env`].
    ///
    /// # Errors
    /// A classified [`LlmError::Provider`] when HTTP construction fails.
    pub fn from_key(key: impl Into<String>, model: Option<String>) -> Result<Self, LlmError> {
        let transport =
            heycode_http::ReqwestHttpTransport::new().map_err(crate::classify_transport_error)?;
        Self::build(
            crate::RouteCredential::fixed(key),
            model,
            heycode_http::HttpService::new(std::sync::Arc::new(transport)),
            Self::BASE_URL,
        )
    }

    /// Build from an explicit key and the composed shared HTTP service.
    ///
    /// # Errors
    /// Protocol-client construction failure.
    pub fn from_key_with_transport(
        key: impl Into<String>,
        model: Option<String>,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        Self::build(
            crate::RouteCredential::fixed(key),
            model,
            http,
            Self::BASE_URL,
        )
    }

    /// Build from a credential resolved once per operation and the composed
    /// shared HTTP service. Both the legacy and native routes then send the
    /// key that is in the store at request time.
    ///
    /// # Errors
    /// Protocol-client construction failure.
    pub fn from_credential_with_transport(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        Self::build(credential, model, http, Self::BASE_URL)
    }

    /// [`Self::from_credential_with_transport`] against a configured base URL
    /// (`llm.base_url`: a proxy or gateway) instead of the official host.
    ///
    /// # Errors
    /// Protocol-client construction failure.
    pub fn from_credential_with_transport_at(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        base_url: &str,
    ) -> Result<Self, LlmError> {
        Self::build(credential, model, http, base_url.trim_end_matches('/'))
    }

    fn build(
        credential: crate::RouteCredential,
        model: Option<String>,
        http: heycode_http::HttpService,
        base_url: &str,
    ) -> Result<Self, LlmError> {
        let credential_reference = credential
            .route()
            .map_or(Self::API_KEY_ENV, crate::CredentialHandle::as_str)
            .to_owned();
        let client = OpenAiCompatClient::with_credential_and_transport(
            OpenAiCompatConfig {
                base_url: base_url.to_owned(),
                api_key_env: Self::API_KEY_ENV,
                extra_headers: Vec::new(),
            },
            credential.clone(),
            http.clone(),
        )?;
        let none = crate::ReasoningEffortId::from_built_in("none");
        let high = crate::ReasoningEffortId::from_built_in("high");
        let max = crate::ReasoningEffortId::from_built_in("max");
        let thinking = crate::ChatThinkingConfig::object_type(
            none.clone(),
            vec![(high.clone(), high.clone()), (max.clone(), max.clone())],
        )
        .omit_temperature_when_enabled()
        .omit_automatic_tool_controls_when_enabled()
        .require_reasoning_content_for_tool_calls();
        let inference = crate::OpenAiChatCompletionsAdapter::new(
            crate::OpenAiChatCompletionsConfig::with_credential(
                provider_descriptor(),
                base_url,
                credential,
            )
            .with_reasoning(
                vec![none, high.clone(), max],
                Some(high),
                crate::ChatReasoningWire::ScalarEffort,
            )
            .with_thinking(thinking),
            http,
        )?;
        Ok(Self {
            client,
            inference,
            default_model: model.unwrap_or_else(|| Self::DEFAULT_MODEL.to_owned()),
            credential_reference,
        })
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
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

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.client.stream(&request.model, &request)
    }
}

impl crate::InferenceAdapter for DeepSeekProvider {
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
        id: DeepSeekProvider::NAME.to_owned(),
        display_name: "DeepSeek".to_owned(),
        protocols: vec![crate::ProviderProtocol::OpenAiChatCompletions],
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn spec_table_constants_match_agents() {
        assert_eq!(DeepSeekProvider::NAME, "deepseek");
        assert_eq!(DeepSeekProvider::DEFAULT_MODEL, "deepseek-v4-flash");
        assert_eq!(DeepSeekProvider::BASE_URL, "https://api.deepseek.com");
        assert_eq!(DeepSeekProvider::API_KEY_ENV, "DEEPSEEK_API_KEY");
        let profile = DeepSeekProvider::setup_profile();
        assert_eq!(profile.registry_name, DeepSeekProvider::NAME);
        assert_eq!(profile.descriptor.id, DeepSeekProvider::NAME);
        assert_eq!(profile.default_model, DeepSeekProvider::DEFAULT_MODEL);
        assert_eq!(
            profile.credential_reference.as_deref(),
            Some(DeepSeekProvider::API_KEY_ENV)
        );
    }

    #[test]
    fn info_projects_resolved_default_model() {
        let provider =
            DeepSeekProvider::from_key("test-key", Some("deepseek-reasoner".into())).unwrap();
        let info = provider.info();
        assert_eq!(info.name, "deepseek");
        assert_eq!(info.default_model, "deepseek-reasoner");
        assert!(Provider::inference_adapter(&provider).is_some());
    }
}
