use heycode_http::HttpService;
use heycode_llm::{
    ChatRequest, ChunkStream, InferenceAdapter, LlmError, ModelDescriptor,
    OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig, OpenAiCompatClient,
    OpenAiCompatConfig, Provider, ProviderDescriptor, ProviderInfo, RouteCredential,
};

use crate::CompatibleSpec;

/// Exact Chat route with a credential acquired at each operation.
pub struct CompatibleProvider {
    spec: CompatibleSpec,
    model: String,
    reference: String,
    adapter: OpenAiChatCompletionsAdapter,
    legacy: OpenAiCompatClient,
}

impl CompatibleProvider {
    /// Bind one selected model and endpoint to the shared strict Chat adapter.
    ///
    /// # Errors
    /// Invalid endpoint, descriptor or default model fails before publication.
    pub fn new(
        spec: CompatibleSpec,
        http: HttpService,
        base_url: &str,
        model: impl Into<String>,
        credential: RouteCredential,
    ) -> Result<Self, LlmError> {
        let model = model.into();
        if model.is_empty()
            || model.len() > 256
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(LlmError::Transport(
                "compatible provider default model is invalid".into(),
            ));
        }
        let reference = match credential.binding() {
            heycode_llm::AuthenticationBinding::Credential(reference) => {
                reference.as_str().to_owned()
            }
            heycode_llm::AuthenticationBinding::AdapterOwned(_) => spec.credential_reference.into(),
            heycode_llm::AuthenticationBinding::None
            | heycode_llm::AuthenticationBinding::Ambient => {
                return Err(LlmError::Transport(
                    "compatible provider requires an explicit credential binding".into(),
                ));
            }
        };
        let adapter = OpenAiChatCompletionsAdapter::new(
            OpenAiChatCompletionsConfig::with_credential(
                spec.descriptor(),
                base_url,
                credential.clone(),
            )
            .with_unknown_tool_attempts(),
            http.clone(),
        )?;
        let legacy = OpenAiCompatClient::with_credential_and_transport(
            OpenAiCompatConfig {
                base_url: base_url.into(),
                api_key_env: spec.credential_reference,
                extra_headers: Vec::new(),
            },
            credential,
            http,
        )?;
        Ok(Self {
            spec,
            model,
            reference,
            adapter,
            legacy,
        })
    }
}

impl Provider for CompatibleProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.spec.id.into(),
            default_model: self.model.clone(),
        }
    }
    fn descriptor(&self) -> ProviderDescriptor {
        self.spec.descriptor()
    }
    fn credential_reference(&self) -> Option<&str> {
        Some(&self.reference)
    }
    fn describe_model(&self, model: &str) -> ModelDescriptor {
        crate::catalog::documented_model(self.spec, model)
    }
    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(&self.adapter)
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.legacy.stream(&request.model, &request)
    }
}
