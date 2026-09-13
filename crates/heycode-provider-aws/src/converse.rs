//! Amazon Bedrock Converse provider profile.
//!
//! PAWS04 joins two finished pieces: P07's reusable `ConverseStream` protocol
//! adapter in `heycode-llm`, and PAWS03's regional discovery in this crate. It
//! adds no protocol code — the request/response shapes, the AWS event-stream
//! framing and their conformance tests belong to P07 — and instead owns the
//! *profile*: which endpoint, which credential, which identity, and which
//! models may legitimately be dispatched through it.
//!
//! # The endpoint is the runtime plane, not the control plane
//!
//! Converse and ConverseStream live on `bedrock-runtime`, a different host
//! from the `bedrock` control plane PAWS03 lists models against and from the
//! `bedrock-mantle` endpoint PAWS02 lists models against. All three read the
//! same region from PAWS01 so they cannot disagree about where the world
//! points.
//!
//! # Which models may be dispatched
//!
//! This adapter streams, and streaming support is per model. AWS documents the
//! test: "To find out if a model supports streaming, call `GetFoundationModel`
//! and check the `responseStreamingSupported` field"
//! (<https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>).
//! PAWS03 already retains exactly that as tri-state evidence, because
//! `ModelDescriptor` has no field for it. [`converse_stream_eligible`] is that
//! join, and it answers `true` only for explicit `Supported` — a model whose
//! streaming support was never published is not a model this adapter may be
//! pointed at. CAT02 retains only the descriptor half, so
//! [`crate::BedrockConverseModelEvidence`] retains the provider-owned facts for
//! the effect-owned inference plugin. Root composition must still select that
//! plugin explicitly; catalog presence alone never activates inference.
//!
//! # Prompt caching is explicit policy
//!
//! Amazon Bedrock caching is model-specific. Amazon Nova may cache text
//! prompts automatically, while the Converse request shape for explicit
//! caching uses a `cachePoint` block placed in `tools`, `system` or `messages`,
//! processed in that order, with model-specific checkpoint limits
//! (<https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html>).
//!
//! [`BedrockRuntimeRequestMetadata`] is the explicit policy. When configured,
//! the request-specific provider hook derives one selected-model option and
//! the shared adapter validates/places its checkpoints. Omitting that policy
//! sends no checkpoint and does not inherit an account or crate default.
//!
//! What does work is the accounting. `ConverseStreamMetadataEvent.usage`
//! reports `cacheReadInputTokens`, `cacheWriteInputTokens` and a per-TTL
//! `cacheDetails` breakdown of what was written, and AWS states that with
//! caching enabled `inputTokens` counts only the *non-cached* tokens, so
//! `total input tokens = inputTokens + cacheReadInputTokens +
//! cacheWriteInputTokens`. `cacheDetails` describes tokens the write counter
//! already includes and must never be summed on top.
//!
//! # No default model is invented here
//!
//! `default_model` is supplied by the caller from the user's own `[llm] model`
//! configuration. AWS publishes model and inference-profile ids on per-model
//! detail pages, but does not name one universal Bedrock default. Candidate
//! forms carry constraints a static constant cannot satisfy: a
//! geography-prefixed inference profile is source-region/data-residency bound,
//! and a bare foundation model id only resolves where that model offers
//! on-demand throughput. A provider default is a versioned cross-crate
//! contract; picking one without account/region intent would be a guess.

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_authorization_aws::{AWS_BEDROCK_API_KEY_REFERENCE, AwsRegion};
use heycode_core::ProviderRequestOption;
use heycode_credentials::{CredentialQuery, CredentialSecret, CredentialsService};
use heycode_http::HttpService;
use heycode_llm::{
    BedrockConverseAdapter, BedrockConverseConfig, CapabilitySupport, CatalogRefreshMode,
    CatalogRegistry, ChatRequest, ChunkStream, InferenceAdapter, LlmError, ModelDescriptor,
    Provider, ProviderDescriptor, ProviderErrorClass, ProviderFailure, ProviderFailureOrigin,
    ProviderInfo, ProviderOptionContext, ProviderProfile, RequestedCapability, ResolveError,
    RouteCredential,
};

use crate::catalog::{
    BEDROCK_PROVIDER, BedrockCatalogEvidence, BedrockCatalogReadiness,
    catalog_failure_preparation_error, catalog_preparation_error, provider_descriptor,
};
use crate::model::BedrockFoundationModel;
use crate::runtime_metadata::{BedrockMetadataError, BedrockRuntimeRequestMetadata};

/// Regional Amazon Bedrock Runtime origin.
///
/// Converse, ConverseStream and InvokeModel are served here. No default region
/// is substituted: PAWS01 resolves the effective region and a world with none
/// never reaches this function.
///
/// <https://docs.aws.amazon.com/general/latest/gr/bedrock.html>
#[must_use]
pub fn runtime_url(region: &AwsRegion) -> String {
    format!("https://bedrock-runtime.{}.amazonaws.com", region.as_str())
}

/// Safe Amazon Bedrock identity plus the caller's configured default model.
///
/// The descriptor is PAWS03's, not a second copy: the catalog and the
/// inference route must not be able to disagree about what `bedrock` is.
#[must_use]
pub fn bedrock_converse_profile(default_model: impl Into<String>) -> ProviderProfile {
    ProviderProfile {
        registry_name: BEDROCK_PROVIDER.to_owned(),
        descriptor: provider_descriptor(),
        default_model: default_model.into(),
        credential_reference: Some(AWS_BEDROCK_API_KEY_REFERENCE.to_owned()),
    }
}

/// Whether one discovered model may be dispatched through `ConverseStream`.
///
/// Only explicit `Supported` qualifies. `Unknown` means AWS published no
/// streaming evidence for the model, which is not permission to stream it, and
/// promoting it would be the exact failure this vocabulary exists to prevent.
#[must_use]
pub fn converse_stream_eligible(model: &BedrockFoundationModel) -> bool {
    model.response_streaming() == CapabilitySupport::Supported
}

/// Amazon Bedrock inference route over the Converse protocol.
///
/// Deliberately has no `Debug` implementation: the fixed-key embedding
/// constructor may leave a credential inside the adapter, while
/// [`Self::from_credentials`] retains only an operation-time route binding.
#[derive(Clone)]
pub struct BedrockConverseProvider {
    adapter: BedrockConverseAdapter,
    default_model: String,
    credential_reference: String,
    region: AwsRegion,
    runtime_metadata: Option<BedrockRuntimeRequestMetadata>,
    model_evidence: Option<BTreeMap<String, ModelDescriptor>>,
    live_model_evidence: Option<BedrockCatalogEvidence>,
    catalogs: Option<CatalogRegistry>,
}

impl BedrockConverseProvider {
    /// Bind one already-resolved Amazon Bedrock API key to a region.
    ///
    /// # Errors
    /// [`LlmError`] when the endpoint, credential or adapter configuration is
    /// unusable.
    pub fn new(
        http: HttpService,
        region: &AwsRegion,
        api_key: &CredentialSecret,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        Self::with_credential(
            http,
            region,
            RouteCredential::fixed(api_key.expose()),
            default_model,
        )
    }

    /// Bind one operation-time credential route to a region.
    ///
    /// # Errors
    /// [`LlmError`] when the endpoint or adapter configuration is unusable.
    pub fn with_credential(
        http: HttpService,
        region: &AwsRegion,
        credential: RouteCredential,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let credential_reference = credential.route().map_or_else(
            || AWS_BEDROCK_API_KEY_REFERENCE.to_owned(),
            |route| route.as_str().to_owned(),
        );
        let config = BedrockConverseConfig::with_credential(
            provider_descriptor(),
            runtime_url(region),
            credential,
        );
        Ok(Self {
            adapter: BedrockConverseAdapter::new(config, http)?,
            default_model: default_model.into(),
            credential_reference,
            region: region.clone(),
            runtime_metadata: None,
            model_evidence: None,
            live_model_evidence: None,
            catalogs: None,
        })
    }

    /// Attach validated PAWS06 cache/guardrail intent to this route.
    ///
    /// [`Provider::request_options_for`] materializes it from the exact
    /// selected model and route set before durable request admission.
    #[must_use]
    pub fn with_runtime_metadata(mut self, metadata: BedrockRuntimeRequestMetadata) -> Self {
        self.runtime_metadata = Some(metadata);
        self
    }

    /// Build request options for the concrete selected model descriptor.
    ///
    /// An unconfigured route returns no options. A configured route returns
    /// exactly one schema-v1 `bedrock/runtime-metadata` option.
    ///
    /// # Errors
    /// Invalid/unsafe selected target identity, Unsupported/Unknown prompt
    /// cache evidence, or an unexpected generic option-boundary refusal.
    pub fn runtime_request_options(
        &self,
        selected_model: &ModelDescriptor,
    ) -> Result<Vec<ProviderRequestOption>, BedrockMetadataError> {
        self.runtime_metadata
            .as_ref()
            .map(|metadata| {
                metadata
                    .to_provider_option(&self.region, selected_model)
                    .map(|option| vec![option])
            })
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    /// Bind the configured reference for resolution once per operation.
    ///
    /// # Errors
    /// The same construction failures as [`Self::with_credential`]. Missing,
    /// unreadable or rotated credentials fail when an operation starts,
    /// before transport.
    pub fn from_credentials(
        http: HttpService,
        credentials: &CredentialsService,
        credential: &CredentialQuery,
        region: &AwsRegion,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        Self::with_credential(
            http,
            region,
            RouteCredential::registry(credentials.clone(), credential.clone()),
            default_model,
        )
    }

    pub(crate) fn with_model_evidence(
        mut self,
        evidence: BTreeMap<String, ModelDescriptor>,
    ) -> Self {
        self.model_evidence = Some(evidence);
        self.live_model_evidence = None;
        self.catalogs = None;
        self
    }

    pub(crate) fn with_live_model_evidence(
        mut self,
        evidence: BedrockCatalogEvidence,
        catalogs: CatalogRegistry,
    ) -> Self {
        self.model_evidence = None;
        self.live_model_evidence = Some(evidence);
        self.catalogs = Some(catalogs);
        self
    }

    fn require_model_evidence(&self, model: &ModelDescriptor) -> Result<(), ResolveError> {
        if self
            .model_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.contains_key(&model.id))
        {
            return Err(model_evidence_error(
                "selected Bedrock model is outside exact Converse evidence",
            ));
        }
        let Some(evidence) = &self.live_model_evidence else {
            return Ok(());
        };
        let row = evidence
            .get(&model.id)
            .map_err(model_evidence_error)?
            .ok_or_else(|| {
                model_evidence_error(
                    "selected Bedrock model lacks live streaming/on-demand evidence; force a catalog refresh",
                )
            })?;
        match row.response_streaming() {
            CapabilitySupport::Supported => {}
            CapabilitySupport::Unsupported => {
                return Err(model_evidence_error(
                    "selected Bedrock model explicitly does not support streaming",
                ));
            }
            CapabilitySupport::Unknown => {
                return Err(model_evidence_error(
                    "selected Bedrock model streaming support is unproven",
                ));
            }
        }
        match row.on_demand() {
            CapabilitySupport::Supported => Ok(()),
            CapabilitySupport::Unsupported => Err(model_evidence_error(
                "selected Bedrock model explicitly lacks on-demand inference",
            )),
            CapabilitySupport::Unknown => Err(model_evidence_error(
                "selected Bedrock model on-demand inference is unproven",
            )),
        }
    }
}

fn model_evidence_error(message: impl Into<String>) -> ResolveError {
    ResolveError::InvalidRequest {
        field: "model_evidence",
        message: message.into(),
    }
}

#[async_trait::async_trait]
impl Provider for BedrockConverseProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: BEDROCK_PROVIDER.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        self.model_evidence
            .as_ref()
            .and_then(|evidence| evidence.get(model))
            .cloned()
            .unwrap_or_else(|| ModelDescriptor::unknown(model))
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    async fn prepare_inference(
        &self,
        context: ProviderOptionContext<'_>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Option<Arc<dyn Provider>>, LlmError> {
        let Some(catalogs) = &self.catalogs else {
            if cancellation.is_cancelled() {
                return Err(preparation_failure(ProviderErrorClass::Cancelled));
            }
            return Ok(None);
        };
        if cancellation.is_cancelled() {
            return Err(preparation_failure(ProviderErrorClass::Cancelled));
        }
        let evidence = self
            .live_model_evidence
            .as_ref()
            .ok_or_else(|| preparation_failure(ProviderErrorClass::Server))?;
        let mut readiness = evidence
            .take_readiness(&context.model().id)
            .map_err(|_| preparation_failure(ProviderErrorClass::Server))?;
        if matches!(readiness, BedrockCatalogReadiness::RefreshRequired) {
            catalogs
                .refresh(BEDROCK_PROVIDER, CatalogRefreshMode::Force, cancellation)
                .await
                .map_err(catalog_preparation_error)?;
            readiness = evidence
                .take_readiness(&context.model().id)
                .map_err(|_| preparation_failure(ProviderErrorClass::Server))?;
        }
        let row = match readiness {
            BedrockCatalogReadiness::Ready(row) => row,
            BedrockCatalogReadiness::Failed(kind) => {
                return Err(catalog_failure_preparation_error(kind));
            }
            BedrockCatalogReadiness::RefreshRequired => {
                return Err(preparation_failure(ProviderErrorClass::InvalidRequest));
            }
        };
        let descriptor = row.descriptor().clone();
        if &descriptor != context.model()
            || row.response_streaming() != CapabilitySupport::Supported
            || row.on_demand() != CapabilitySupport::Supported
        {
            return Err(preparation_failure(ProviderErrorClass::InvalidRequest));
        }
        let mut prepared = self.clone();
        prepared.model_evidence = Some(BTreeMap::from([(descriptor.id.clone(), descriptor)]));
        prepared.live_model_evidence = None;
        prepared.catalogs = None;
        Ok(Some(Arc::new(prepared)))
    }

    fn request_options_for(
        &self,
        context: ProviderOptionContext<'_>,
    ) -> Result<Vec<ProviderRequestOption>, ResolveError> {
        self.require_model_evidence(context.model())?;
        if context.native_tool_routes().iter().any(|route| {
            route.kind() == heycode_core::NativeToolImplementationKind::Provider
                && route.provider() == Some(BEDROCK_PROVIDER)
        }) {
            return Err(ResolveError::InvalidRequest {
                field: "native_tool_routes",
                message: "Bedrock Converse has no provider-native tool implementation".to_owned(),
            });
        }
        self.runtime_request_options(context.model())
            .map_err(|error| match error.class() {
                crate::BedrockMetadataErrorClass::Unsupported => ResolveError::Unsupported {
                    provider: BEDROCK_PROVIDER.to_owned(),
                    model: context.model().id.clone(),
                    capability: RequestedCapability::PromptCache,
                },
                crate::BedrockMetadataErrorClass::Unproven => ResolveError::Unproven {
                    provider: BEDROCK_PROVIDER.to_owned(),
                    model: context.model().id.clone(),
                    capability: RequestedCapability::PromptCache,
                },
                crate::BedrockMetadataErrorClass::Invalid => ResolveError::InvalidRequest {
                    field: error.field(),
                    message: "Bedrock request metadata is invalid".to_owned(),
                },
            })
    }

    /// The legacy chat path is refused rather than degraded.
    ///
    /// Converse has no OpenAI-compatible client behind it, so there is nothing
    /// this could fall back to. P08's rule is that a Provider which advertises
    /// an `InferenceAdapter` must fail loud here instead of quietly taking a
    /// different route — a silent degradation on a path nobody watches is how
    /// a whole class of provider-state bugs stays hidden.
    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::Provider(ProviderFailure::new(
                ProviderErrorClass::InvalidRequest,
                ProviderFailureOrigin::Local,
            )))
        }))
    }
}

fn preparation_failure(class: ProviderErrorClass) -> LlmError {
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

impl InferenceAdapter for BedrockConverseProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor()
    }

    fn authentication_binding(&self) -> heycode_llm::AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        draft: heycode_llm::RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<heycode_llm::ResolvedCall, ResolveError> {
        self.require_model_evidence(model)?;
        self.adapter.resolve(draft, model)
    }

    fn stream(&self, call: heycode_llm::ResolvedCall) -> heycode_llm::InferenceStream {
        self.adapter.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: heycode_llm::ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> heycode_llm::InferenceStream {
        self.adapter.stream_cancellable(call, cancellation)
    }
}
