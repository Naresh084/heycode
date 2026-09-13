//! Microsoft Azure OpenAI v1 inference boundary.
//!
//! The GA v1 endpoint is `https://<resource>.openai.azure.com/openai/v1/`,
//! uses the deployment name in the request `model` field, and has implicit
//! versioning rather than an `api-version` query parameter:
//! <https://learn.microsoft.com/azure/ai-studio/ai-services/concepts/endpoints>.

use heycode_core::ProviderProtocol;
use heycode_http::HttpService;
use heycode_llm::{
    ChatRequest, ChunkStream, InferenceAdapter, InferenceStream, LlmError, ModelDescriptor,
    OpenAiResponsesAdapter, OpenAiResponsesConfig, Provider, ProviderDescriptor, ProviderInfo,
    RequestDraft, ResolveError, ResolvedCall, RouteCredential,
};

mod catalog;
mod plugin;

pub use catalog::{AzureOpenAiCatalog, AzureOpenAiCatalogConfig, azure_openai_catalog_plugin};
pub use plugin::{
    AzureInferencePluginConfig, AzureInferencePluginError, azure_openai_inference_plugin,
};

/// Stable Azure OpenAI inference identity.
pub const AZURE_OPENAI_PROVIDER: &str = "azure-openai";
/// Provider-owned non-secret API-key reference.
pub const AZURE_OPENAI_API_KEY_REFERENCE: &str = "AZURE_OPENAI_API_KEY";

/// Host-safe Azure Cognitive Services resource name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureResourceName(String);

impl AzureResourceName {
    /// Validate the resource label used in the Azure OpenAI endpoint host.
    ///
    /// # Errors
    /// Values outside 2..=64 ASCII alphanumeric/hyphen bytes, or with a
    /// non-alphanumeric edge, are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, AzureRouteError> {
        let value = value.into();
        let valid = (2..=64).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric);
        valid
            .then_some(Self(value))
            .ok_or(AzureRouteError::InvalidResource)
    }

    /// Borrow the exact resource name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact Azure deployment name sent in the Responses `model` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureDeploymentName(String);

impl AzureDeploymentName {
    /// Validate one bounded deployment name that is also safe as a model path segment.
    ///
    /// # Errors
    /// Blank, overlong, whitespace/control, slash and query-delimiter values
    /// are refused before URL construction.
    pub fn new(value: impl Into<String>) -> Result<Self, AzureRouteError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        valid
            .then_some(Self(value))
            .ok_or(AzureRouteError::InvalidDeployment)
    }

    /// Borrow the exact deployment name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Safe Azure route-construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AzureRouteError {
    /// Resource cannot be used as the Azure endpoint host label.
    #[error("Azure OpenAI resource name is invalid")]
    InvalidResource,
    /// Deployment cannot be used as the request model and lookup path.
    #[error("Azure OpenAI deployment name is invalid")]
    InvalidDeployment,
}

/// Provider descriptor shared by catalog and inference.
#[must_use]
pub fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: AZURE_OPENAI_PROVIDER.to_owned(),
        display_name: "Microsoft Azure OpenAI".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiResponses],
    }
}

/// Exact Azure OpenAI v1 base URL for one validated resource.
#[must_use]
pub fn responses_base_url(resource: &AzureResourceName) -> String {
    format!("https://{}.openai.azure.com/openai/v1", resource.as_str())
}

/// Provider-owned Azure OpenAI connection form.
#[must_use]
pub fn azure_openai_connection_profile() -> heycode_llm::ConnectionProfile {
    heycode_llm::ConnectionProfile {
        registry_name: AZURE_OPENAI_PROVIDER.to_owned(),
        descriptor: provider_descriptor(),
        default_model: None,
        credential_reference: Some(AZURE_OPENAI_API_KEY_REFERENCE.to_owned()),
        family: heycode_llm::ConnectionFamily::Cloud,
        default_endpoint: None,
        help: Some(
            "Create an Azure OpenAI resource and a deployment that supports the Responses API. heycode uses the GA /openai/v1 route with the deployment name as model, authenticates with an Azure API key and stores only its reference."
                .to_owned(),
        ),
        selectable_models: None,
        parameters: vec![
            heycode_llm::ConnectionParameter {
                id: "resource".to_owned(),
                label: "Azure OpenAI resource".to_owned(),
                description: "Resource name before .openai.azure.com".to_owned(),
            },
            heycode_llm::ConnectionParameter {
                id: "deployment".to_owned(),
                label: "Azure OpenAI deployment".to_owned(),
                description: "Deployment name sent in the Responses model field".to_owned(),
            },
        ],
        model_selection: heycode_llm::ConnectionModelSelection::Catalog,
    }
}

/// One Azure OpenAI deployment over the shared strict Responses protocol.
pub struct AzureOpenAiProvider {
    resource: AzureResourceName,
    deployment: AzureDeploymentName,
    credential_reference: String,
    adapter: OpenAiResponsesAdapter,
}

impl AzureOpenAiProvider {
    /// Bind one resource/deployment and operation-time API key.
    ///
    /// # Errors
    /// Invalid endpoint or credential configuration fails before publication.
    pub fn new(
        http: HttpService,
        resource: AzureResourceName,
        deployment: AzureDeploymentName,
        credential_reference: impl Into<String>,
        credential: RouteCredential,
    ) -> Result<Self, LlmError> {
        let adapter = OpenAiResponsesAdapter::new(
            OpenAiResponsesConfig::with_credential(
                provider_descriptor(),
                responses_base_url(&resource),
                credential,
            )
            .with_api_key_header()
            .with_unknown_tool_attempts(),
            http,
        )?;
        Ok(Self {
            resource,
            deployment,
            credential_reference: credential_reference.into(),
            adapter,
        })
    }

    /// Bound Azure resource.
    #[must_use]
    pub const fn resource(&self) -> &AzureResourceName {
        &self.resource
    }
}

impl Provider for AzureOpenAiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: AZURE_OPENAI_PROVIDER.to_owned(),
            default_model: self.deployment.as_str().to_owned(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
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
                "Azure OpenAI dispatches through its Responses adapter".to_owned(),
            ))
        }))
    }
}

impl InferenceAdapter for AzureOpenAiProvider {
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
        if draft.provider != AZURE_OPENAI_PROVIDER
            || draft.model != self.deployment.as_str()
            || model.id != self.deployment.as_str()
        {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "Azure OpenAI request must use the configured deployment".to_owned(),
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
