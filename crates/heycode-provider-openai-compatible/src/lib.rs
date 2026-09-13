//! Explicit custom-server boundary for the OpenAI Chat Completions protocol.
//!
//! A custom server is never protocol-detected. Its configured URL is the
//! version root used for exact `/models` and `/chat/completions` routes, and
//! its optional bearer credential remains absent unless the user chose one.

use heycode_core::ProviderProtocol;
use heycode_http::{HttpRequest, HttpService, HttpSseRequest};
use heycode_llm::{
    ChatRequest, ChunkStream, InferenceAdapter, InferenceStream, LlmError, ModelDescriptor,
    OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig, Provider, ProviderDescriptor,
    ProviderInfo, RequestDraft, ResolveError, ResolvedCall, RouteCredential,
};

mod catalog;
mod plugin;

pub use catalog::{CustomOpenAiCatalog, CustomOpenAiCatalogConfig, custom_openai_catalog_plugin};
pub use plugin::{
    CustomOpenAiInferencePluginConfig, CustomOpenAiInferencePluginError,
    custom_openai_inference_plugin,
};

/// Stable registry identity for a user-supplied OpenAI-compatible server.
pub const CUSTOM_OPENAI_PROVIDER: &str = "custom-openai";

/// Validated OpenAI-compatible version root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomOpenAiEndpoint(String);

impl CustomOpenAiEndpoint {
    /// Validate an absolute credential-free HTTP(S) version-root URL.
    ///
    /// A query or fragment is refused because heycode appends fixed protocol
    /// paths. One trailing slash is normalized away.
    ///
    /// # Errors
    /// Blank, untrimmed, oversized, non-HTTP(S), hostless, credential-bearing,
    /// query-bearing or fragment-bearing URLs are refused without echoing the
    /// input.
    pub fn new(value: impl AsRef<str>) -> Result<Self, CustomOpenAiRouteError> {
        let value = value.as_ref();
        if value.is_empty() || value.len() > 2048 || value.trim() != value {
            return Err(CustomOpenAiRouteError::InvalidEndpoint);
        }
        let parsed = url::Url::parse(value).map_err(|_| CustomOpenAiRouteError::InvalidEndpoint)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(CustomOpenAiRouteError::InvalidEndpoint);
        }
        let base = parsed.as_str().trim_end_matches('/').to_owned();
        if base.is_empty()
            || HttpRequest::get(format!("{base}/models")).is_err()
            || HttpSseRequest::post(format!("{base}/chat/completions"), Vec::new()).is_err()
        {
            return Err(CustomOpenAiRouteError::InvalidEndpoint);
        }
        Ok(Self(base))
    }

    /// Borrow the normalized version root.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn models_url(&self) -> String {
        format!("{}/models", self.0)
    }
}

/// Exact model id selected for a custom server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomOpenAiModel(String);

impl CustomOpenAiModel {
    /// Validate one bounded provider-native model id without rewriting it.
    ///
    /// # Errors
    /// Blank, untrimmed, control-bearing or oversized ids are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, CustomOpenAiRouteError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 256
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(CustomOpenAiRouteError::InvalidModel);
        }
        Ok(Self(value))
    }

    /// Borrow the exact provider-native id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Safe custom route construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CustomOpenAiRouteError {
    /// The configured version root cannot safely receive fixed protocol paths.
    #[error("custom OpenAI-compatible server URL is invalid")]
    InvalidEndpoint,
    /// The selected provider-native model id is unsafe or empty.
    #[error("custom OpenAI-compatible model ID is invalid")]
    InvalidModel,
}

/// Descriptor shared by setup, catalog and inference.
#[must_use]
pub fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: CUSTOM_OPENAI_PROVIDER.to_owned(),
        display_name: "Custom OpenAI-compatible server".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

/// Custom local-server connection metadata.
#[must_use]
pub fn custom_openai_connection_profile() -> heycode_llm::ConnectionProfile {
    heycode_llm::ConnectionProfile {
        registry_name: CUSTOM_OPENAI_PROVIDER.to_owned(),
        descriptor: provider_descriptor(),
        default_model: None,
        credential_reference: None,
        family: heycode_llm::ConnectionFamily::Local,
        default_endpoint: None,
        help: Some(
            "Enter the server's OpenAI-compatible version root, including /v1 when required. heycode uses only GET /models and POST /chat/completions, can save an optional masked bearer key, and does not start or manage the server."
                .to_owned(),
        ),
        selectable_models: None,
        parameters: Vec::new(),
        model_selection: heycode_llm::ConnectionModelSelection::CatalogOrExplicit,
    }
}

/// One selected model on one explicit custom Chat Completions route.
pub struct CustomOpenAiProvider {
    endpoint: CustomOpenAiEndpoint,
    model: CustomOpenAiModel,
    credential_reference: Option<String>,
    adapter: OpenAiChatCompletionsAdapter,
}

impl CustomOpenAiProvider {
    /// Bind the selected model to authenticated or unauthenticated Chat Completions.
    ///
    /// # Errors
    /// Invalid adapter configuration fails before registry publication.
    pub fn new(
        http: HttpService,
        endpoint: CustomOpenAiEndpoint,
        model: CustomOpenAiModel,
        credential: Option<RouteCredential>,
    ) -> Result<Self, LlmError> {
        let credential_reference = credential.as_ref().and_then(|credential| {
            credential
                .route()
                .map(|reference| reference.as_str().to_owned())
        });
        let config = match credential {
            Some(credential) => OpenAiChatCompletionsConfig::with_credential(
                provider_descriptor(),
                endpoint.as_str(),
                credential,
            ),
            None => OpenAiChatCompletionsConfig::without_authentication(
                provider_descriptor(),
                endpoint.as_str(),
            ),
        };
        let adapter = OpenAiChatCompletionsAdapter::new(config.with_unknown_tool_attempts(), http)?;
        Ok(Self {
            endpoint,
            model,
            credential_reference,
            adapter,
        })
    }

    /// Configured normalized version root.
    #[must_use]
    pub const fn endpoint(&self) -> &CustomOpenAiEndpoint {
        &self.endpoint
    }
}

impl Provider for CustomOpenAiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: CUSTOM_OPENAI_PROVIDER.to_owned(),
            default_model: self.model.as_str().to_owned(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn credential_reference(&self) -> Option<&str> {
        self.credential_reference.as_deref()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        ModelDescriptor::unknown(model)
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "custom OpenAI-compatible inference requires the strict adapter".to_owned(),
            ))
        }))
    }
}

impl InferenceAdapter for CustomOpenAiProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn authentication_binding(&self) -> heycode_llm::AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        if draft.provider != CUSTOM_OPENAI_PROVIDER
            || draft.model != self.model.as_str()
            || model.id != self.model.as_str()
        {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "custom OpenAI-compatible request must use the selected model".to_owned(),
            });
        }
        self.adapter.resolve(draft, model)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.adapter.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        self.adapter.stream_cancellable(call, cancellation)
    }
}
