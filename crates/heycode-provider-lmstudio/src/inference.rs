//! LM Studio Chat route with an explicit loaded-model preparation gate.

use crate::{
    LM_STUDIO_DISPLAY_NAME, LM_STUDIO_PROVIDER, LmStudioAuth, LmStudioCatalog, LmStudioConfig,
    LmStudioDetector, LmStudioModelControl,
};
use async_trait::async_trait;
use heycode_core::ProviderProtocol;
use heycode_credentials::CredentialsService;
use heycode_http::HttpService;
use heycode_llm::{
    ChatRequest, ChunkStream, InferenceAdapter, LlmError, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig, Provider, ProviderDescriptor, ProviderInfo, ProviderOptionContext,
    RouteCredential,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Native-loop Chat adapter; preparation proves a loaded, tool-capable model.
pub struct LmStudioInference {
    model: String,
    reference: Option<String>,
    adapter: OpenAiChatCompletionsAdapter,
    catalog: LmStudioCatalog,
    detector: LmStudioDetector,
    control: LmStudioModelControl,
}

impl LmStudioInference {
    /// Construct an explicit local route without loading a model or contacting the server.
    ///
    /// # Errors
    /// Invalid model/endpoint or missing configured credential service.
    pub fn new(
        http: HttpService,
        credentials: Option<Arc<CredentialsService>>,
        config: LmStudioConfig,
        model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let model = model.into();
        if model.is_empty()
            || model.len() > 256
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(LlmError::Transport("LM Studio model id is invalid".into()));
        }
        let (reference, credential) = match config.auth() {
            LmStudioAuth::None => (None, None),
            LmStudioAuth::BearerToken(query) => {
                let credentials = credentials.as_ref().ok_or_else(|| {
                    LlmError::Transport("LM Studio credential service is unavailable".into())
                })?;
                (
                    Some(query.reference.as_str().to_owned()),
                    Some(RouteCredential::registry(
                        credentials.as_ref().clone(),
                        query.clone(),
                    )),
                )
            }
        };
        let base_url = format!("{}/v1", config.endpoint().base_url());
        let route = match credential {
            Some(credential) => {
                OpenAiChatCompletionsConfig::with_credential(descriptor(), base_url, credential)
            }
            None => OpenAiChatCompletionsConfig::without_authentication(descriptor(), base_url),
        };
        let adapter = OpenAiChatCompletionsAdapter::new(route, http.clone())?;
        Ok(Self {
            model,
            reference,
            adapter,
            catalog: LmStudioCatalog::new(http.clone(), credentials.clone(), config.clone()),
            detector: LmStudioDetector::new(http.clone(), credentials.clone(), config.clone()),
            control: LmStudioModelControl::new(http, credentials, config),
        })
    }
}

fn descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: LM_STUDIO_PROVIDER.into(),
        display_name: LM_STUDIO_DISPLAY_NAME.into(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

#[async_trait]
impl Provider for LmStudioInference {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: LM_STUDIO_PROVIDER.into(),
            default_model: self.model.clone(),
        }
    }
    fn credential_reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }
    fn descriptor(&self) -> ProviderDescriptor {
        descriptor()
    }
    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(&self.adapter)
    }
    async fn prepare_inference(
        &self,
        context: ProviderOptionContext<'_>,
        cancellation: CancellationToken,
    ) -> Result<Option<Arc<dyn Provider>>, LlmError> {
        let rows = self
            .catalog
            .list_models(cancellation.clone())
            .await
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        let model = context.model().id.as_str();
        let record = rows
            .iter()
            .find(|record| {
                record.key == model
                    || record
                        .loaded_instances
                        .iter()
                        .any(|instance| instance.id == model)
            })
            .ok_or_else(|| {
                LlmError::Transport("LM Studio no longer has the selected model".into())
            })?;
        if let Some(refusal) = record.agent_eligibility().refusal() {
            return Err(LlmError::Transport(refusal.to_string()));
        }
        let loaded = self
            .control
            .require_loaded(record)
            .map_err(|error| LlmError::Transport(error.to_string()))?;
        if !loaded.iter().any(|instance| instance.id == model) {
            return Err(LlmError::Transport(
                "Select the loaded LM Studio instance identifier to avoid loading another copy"
                    .into(),
            ));
        }
        let report = self.detector.detect(cancellation.clone()).await;
        if cancellation.is_cancelled() {
            return Err(LlmError::Provider(heycode_llm::ProviderFailure::new(
                heycode_llm::ProviderErrorClass::Cancelled,
                heycode_llm::ProviderFailureOrigin::Local,
            )));
        }
        if report.protocol(ProviderProtocol::OpenAiChatCompletions)
            != heycode_llm::CapabilitySupport::Supported
        {
            return Err(LlmError::Transport(
                "LM Studio's Chat Completions endpoint is unavailable".into(),
            ));
        }
        Ok(None)
    }
    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::Transport(
                "LM Studio requires the prepared inference adapter".into(),
            ))
        }))
    }
}
