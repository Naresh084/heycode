//! Amazon Bedrock Mantle Responses and Anthropic Messages profiles.
//!
//! PAWS05 binds the reusable P03/P05 adapters to the regional
//! `bedrock-mantle` endpoint. The two routes share one catalog/provider id and
//! one Bedrock API-key reference, but their wire and endpoint capabilities are
//! not interchangeable:
//!
//! - Responses uses `Authorization: Bearer` at `/v1/responses`, supports
//!   background inference, provider-managed response state, Projects and
//!   server-side tools.
//! - Messages uses `x-api-key` plus `anthropic-version: 2023-06-01` at
//!   `/anthropic/v1/messages`, supports Workspaces, and rejects
//!   `output_config.format` structured output on this endpoint.
//!
//! These are endpoint/protocol facts. [`MantleProfileCapabilities`] is kept
//! separate from [`heycode_llm::ModelCapabilities`], and the id-only PAWS02
//! catalog continues to publish Unknown for every model capability.
//! The current AWS model/API matrix is also enforced at its safe family
//! boundary: Responses admits only OpenAI/xAI candidates and Messages only
//! Anthropic candidates. A matching family remains Unknown until an exact
//! live/maintained model row proves protocol compatibility.
//!
//! Sources:
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/bedrock-mantle.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-messages-api.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/structured-output.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/models-api-compatibility.html>

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_authorization_aws::{AWS_BEDROCK_API_KEY_REFERENCE, AwsRegion};
use heycode_credentials::{CredentialQuery, CredentialSecret, CredentialsService};
use heycode_http::HttpService;
use heycode_llm::{
    AnthropicAuthWire, AnthropicMessagesAdapter, AnthropicMessagesConfig, AuthenticationBinding,
    CapabilitySupport, CatalogRefreshMode, CatalogRegistry, ChatRequest, ChunkStream,
    InferenceAdapter, InferenceStream, LlmError, ModelDescriptor, OpenAiResponsesAdapter,
    OpenAiResponsesConfig, Provider, ProviderErrorClass, ProviderFailure, ProviderFailureOrigin,
    ProviderInfo, ProviderOptionContext, ProviderProfile, RequestDraft, ResolveError, ResolvedCall,
    RouteCredential,
};

use crate::catalog::{catalog_failure_preparation_error, catalog_preparation_error};
use crate::mantle::{
    MANTLE_PROVIDER, MantleCatalogEvidence, MantleCatalogReadiness, mantle_origin,
    mantle_provider_descriptor,
};

/// One Mantle protocol profile with a distinct endpoint contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MantleProtocol {
    /// OpenAI-compatible Responses at `/v1/responses`.
    Responses,
    /// Anthropic Messages at `/anthropic/v1/messages`.
    Messages,
}

/// Exact current model ids whose AWS model cards/API matrix affirm the Mantle
/// Messages API. A listed Mantle model outside this set remains Unknown; an
/// Anthropic family prefix alone is never promoted.
const MAINTAINED_MANTLE_MESSAGES_MODELS: &[&str] = &[
    "anthropic.claude-fable-5",
    "anthropic.claude-haiku-4-5-20251001-v1:0",
    "anthropic.claude-mythos-5",
    "anthropic.claude-mythos-preview",
    "anthropic.claude-opus-4-7",
    "anthropic.claude-opus-4-8",
    "anthropic.claude-sonnet-5",
];

impl MantleProtocol {
    /// Provider-endpoint capabilities documented for this protocol profile.
    #[must_use]
    pub const fn capabilities(self) -> MantleProfileCapabilities {
        match self {
            Self::Responses => MantleProfileCapabilities {
                background: CapabilitySupport::Supported,
                client_side_tools: CapabilitySupport::Supported,
                server_side_tools: CapabilitySupport::Supported,
                projects: CapabilitySupport::Supported,
                workspaces: CapabilitySupport::Unsupported,
                // AWS does not publish a blanket Responses structured-output
                // guarantee across every Mantle model. Model evidence remains
                // independently required before heycode may request a schema.
                structured_output: CapabilitySupport::Unknown,
            },
            Self::Messages => MantleProfileCapabilities {
                background: CapabilitySupport::Unsupported,
                client_side_tools: CapabilitySupport::Supported,
                // The Messages page documents client tool use but no
                // provider-side tool definition. Absence is not Unsupported.
                server_side_tools: CapabilitySupport::Unknown,
                projects: CapabilitySupport::Unsupported,
                workspaces: CapabilitySupport::Supported,
                structured_output: CapabilitySupport::Unsupported,
            },
        }
    }

    /// Conservative provider-family compatibility from AWS's current API
    /// matrix.
    ///
    /// A matching family stays `Unknown`: the table is model-specific and a
    /// live catalog/maintained exact row must still prove the selected id.
    /// A family the matrix excludes is explicitly `Unsupported` and is
    /// refused before transport.
    #[must_use]
    pub fn model_family_support(self, canonical_model: &str) -> CapabilitySupport {
        let possible = match self {
            Self::Responses => {
                canonical_model.starts_with("openai.") || canonical_model.starts_with("xai.")
            }
            Self::Messages => canonical_model.starts_with("anthropic."),
        };
        if possible {
            CapabilitySupport::Unknown
        } else {
            CapabilitySupport::Unsupported
        }
    }

    /// Provider-maintained exact model/protocol evidence used by lazy
    /// activation after account-visible catalog membership is proven.
    ///
    /// Responses membership is supplied by the live `/v1/models` catalog and
    /// therefore remains Unknown here. Messages has no equivalent discovery
    /// API, so only exact current AWS-documented ids are Supported.
    #[must_use]
    pub fn maintained_model_support(self, canonical_model: &str) -> CapabilitySupport {
        if self.model_family_support(canonical_model) == CapabilitySupport::Unsupported {
            return CapabilitySupport::Unsupported;
        }
        match self {
            Self::Responses => CapabilitySupport::Unknown,
            Self::Messages if MAINTAINED_MANTLE_MESSAGES_MODELS.contains(&canonical_model) => {
                CapabilitySupport::Supported
            }
            Self::Messages => CapabilitySupport::Unknown,
        }
    }
}

struct MantleAdapter<A> {
    protocol: MantleProtocol,
    inner: A,
}

impl<A> MantleAdapter<A> {
    const fn new(protocol: MantleProtocol, inner: A) -> Self {
        Self { protocol, inner }
    }
}

impl<A: InferenceAdapter> InferenceAdapter for MantleAdapter<A> {
    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        let mut descriptor = self.inner.descriptor();
        descriptor.protocols = vec![match self.protocol {
            MantleProtocol::Responses => heycode_core::ProviderProtocol::OpenAiResponses,
            MantleProtocol::Messages => heycode_core::ProviderProtocol::AnthropicMessages,
        }];
        descriptor
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.inner.authentication_binding()
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        let call = self.inner.resolve(draft, model)?;
        if self.protocol.model_family_support(call.model()) == CapabilitySupport::Unsupported {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "the selected model family is incompatible with this Mantle protocol"
                    .to_owned(),
            });
        }
        Ok(call)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.inner.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        self.inner.stream_cancellable(call, cancellation)
    }

    fn native_compaction(&self) -> Option<&dyn heycode_llm::NativeCompactionAdapter> {
        self.inner.native_compaction()
    }
}

/// Endpoint/protocol facts that must not be projected onto every listed model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MantleProfileCapabilities {
    /// Long-running inference through `background=true`.
    pub background: CapabilitySupport,
    /// Client-defined function/tool calls.
    pub client_side_tools: CapabilitySupport,
    /// Provider-executed or preconfigured tools.
    pub server_side_tools: CapabilitySupport,
    /// OpenAI-compatible Project scoping.
    pub projects: CapabilitySupport,
    /// Anthropic-compatible Workspace scoping.
    pub workspaces: CapabilitySupport,
    /// Schema-constrained response output on this protocol route.
    pub structured_output: CapabilitySupport,
}

/// Safe setup/routing metadata for one selected Mantle protocol route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MantleInferenceProfile {
    /// Exact adapter protocol this profile selects.
    pub protocol: MantleProtocol,
    /// Shared provider/catalog identity and caller-supplied model default.
    pub provider: ProviderProfile,
    /// Endpoint facts for this exact protocol, never model facts.
    pub capabilities: MantleProfileCapabilities,
}

/// OpenAI-compatible Responses base URL for one validated region.
///
/// The shared Responses adapter appends `/responses`.
#[must_use]
pub fn mantle_responses_base_url(region: &AwsRegion) -> String {
    format!("{}/v1", mantle_origin(region))
}

/// Anthropic Messages base URL for one validated region.
///
/// The shared Messages adapter appends `/messages`.
#[must_use]
pub fn mantle_messages_base_url(region: &AwsRegion) -> String {
    format!("{}/anthropic/v1", mantle_origin(region))
}

fn profile(protocol: MantleProtocol, default_model: impl Into<String>) -> MantleInferenceProfile {
    MantleInferenceProfile {
        protocol,
        provider: ProviderProfile {
            registry_name: MANTLE_PROVIDER.to_owned(),
            descriptor: mantle_provider_descriptor(),
            default_model: default_model.into(),
            credential_reference: Some(AWS_BEDROCK_API_KEY_REFERENCE.to_owned()),
        },
        capabilities: protocol.capabilities(),
    }
}

/// Safe identity/default metadata for a Mantle Responses route.
#[must_use]
pub fn mantle_responses_profile(default_model: impl Into<String>) -> MantleInferenceProfile {
    profile(MantleProtocol::Responses, default_model)
}

/// Safe identity/default metadata for a Mantle Messages route.
#[must_use]
pub fn mantle_messages_profile(default_model: impl Into<String>) -> MantleInferenceProfile {
    profile(MantleProtocol::Messages, default_model)
}

fn legacy_refusal() -> ChunkStream {
    Box::pin(futures::stream::once(async {
        Err(LlmError::Provider(ProviderFailure::new(
            ProviderErrorClass::InvalidRequest,
            ProviderFailureOrigin::Local,
        )))
    }))
}

fn validated_default_model(
    protocol: MantleProtocol,
    default_model: impl Into<String>,
) -> Result<String, LlmError> {
    let default_model = default_model.into();
    if protocol.model_family_support(&default_model) == CapabilitySupport::Unsupported {
        return Err(LlmError::Provider(ProviderFailure::new(
            ProviderErrorClass::InvalidRequest,
            ProviderFailureOrigin::Local,
        )));
    }
    Ok(default_model)
}

/// Amazon Bedrock Mantle route over the OpenAI Responses protocol.
///
/// Deliberately has no `Debug`: a fixed-key embedding constructor may leave a
/// credential inside the adapter; production-style construction keeps only an
/// operation-time route binding.
pub struct MantleResponsesProvider {
    adapter: MantleAdapter<OpenAiResponsesAdapter>,
    default_model: String,
    credential_reference: String,
    model_evidence: Option<BTreeMap<String, ModelDescriptor>>,
    catalog_evidence: Option<CatalogRegistry>,
    live_evidence: Option<MantleCatalogEvidence>,
}

impl MantleResponsesProvider {
    /// Bind a Bedrock API key to one regional Mantle Responses endpoint.
    ///
    /// # Errors
    /// [`LlmError`] when endpoint, credential or adapter configuration is
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

    /// Bind one operation-time credential route to Mantle Responses.
    ///
    /// # Errors
    /// [`LlmError`] when endpoint or adapter configuration is unusable.
    pub fn with_credential(
        http: HttpService,
        region: &AwsRegion,
        credential: RouteCredential,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let default_model = validated_default_model(MantleProtocol::Responses, default_model)?;
        let credential_reference = credential.route().map_or_else(
            || AWS_BEDROCK_API_KEY_REFERENCE.to_owned(),
            |route| route.as_str().to_owned(),
        );
        let config = OpenAiResponsesConfig::with_credential(
            mantle_provider_descriptor(),
            mantle_responses_base_url(region),
            credential,
        );
        Ok(Self {
            adapter: MantleAdapter::new(
                MantleProtocol::Responses,
                OpenAiResponsesAdapter::new(config, http)?,
            ),
            default_model,
            credential_reference,
            model_evidence: None,
            catalog_evidence: None,
            live_evidence: None,
        })
    }

    /// Bind the configured reference for resolution once per operation.
    ///
    /// # Errors
    /// The same construction failures as [`Self::with_credential`]. Missing,
    /// unreadable or rotated credentials fail when an operation starts.
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
        self.catalog_evidence = None;
        self.live_evidence = None;
        self
    }

    pub(crate) fn with_catalog_evidence(
        mut self,
        evidence: CatalogRegistry,
        live_evidence: MantleCatalogEvidence,
    ) -> Self {
        self.model_evidence = None;
        self.catalog_evidence = Some(evidence);
        self.live_evidence = Some(live_evidence);
        self
    }
}

#[async_trait::async_trait]
impl Provider for MantleResponsesProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: MANTLE_PROVIDER.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        mantle_provider_descriptor()
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
        prepare_mantle_catalog(
            self.catalog_evidence.as_ref(),
            self.live_evidence.as_ref(),
            MantleProtocol::Responses,
            context,
            cancellation,
        )
        .await?;
        Ok(None)
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        legacy_refusal()
    }
}

impl InferenceAdapter for MantleResponsesProvider {
    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        self.adapter.descriptor()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        require_model_evidence(
            &self.model_evidence,
            self.catalog_evidence.as_ref(),
            MantleProtocol::Responses,
            draft.effective_at_ms,
            model,
        )?;
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

    fn native_compaction(&self) -> Option<&dyn heycode_llm::NativeCompactionAdapter> {
        self.adapter.native_compaction()
    }
}

/// Amazon Bedrock Mantle route over the Anthropic Messages protocol.
///
/// Deliberately has no `Debug`: a fixed-key embedding constructor may leave a
/// credential inside the adapter; production-style construction keeps only an
/// operation-time route binding.
pub struct MantleMessagesProvider {
    adapter: MantleAdapter<AnthropicMessagesAdapter>,
    default_model: String,
    credential_reference: String,
    model_evidence: Option<BTreeMap<String, ModelDescriptor>>,
    catalog_evidence: Option<CatalogRegistry>,
    live_evidence: Option<MantleCatalogEvidence>,
}

impl MantleMessagesProvider {
    /// Bind a Bedrock API key to one regional Mantle Messages endpoint.
    ///
    /// `default_max_output_tokens` is caller evidence, not a crate constant.
    /// `None` requires every request to carry an explicit output cap, because
    /// PAWS02's id-only catalog publishes no trustworthy model limit.
    ///
    /// # Errors
    /// [`LlmError`] when endpoint, credential or adapter configuration is
    /// unusable.
    pub fn new(
        http: HttpService,
        region: &AwsRegion,
        api_key: &CredentialSecret,
        default_model: impl Into<String>,
        default_max_output_tokens: Option<u64>,
    ) -> Result<Self, LlmError> {
        Self::with_credential(
            http,
            region,
            RouteCredential::fixed(api_key.expose()),
            default_model,
            default_max_output_tokens,
        )
    }

    /// Bind one operation-time credential route to Mantle Messages.
    ///
    /// # Errors
    /// [`LlmError`] when endpoint or adapter configuration is unusable.
    pub fn with_credential(
        http: HttpService,
        region: &AwsRegion,
        credential: RouteCredential,
        default_model: impl Into<String>,
        default_max_output_tokens: Option<u64>,
    ) -> Result<Self, LlmError> {
        let default_model = validated_default_model(MantleProtocol::Messages, default_model)?;
        let credential_reference = credential.route().map_or_else(
            || AWS_BEDROCK_API_KEY_REFERENCE.to_owned(),
            |route| route.as_str().to_owned(),
        );
        let config = AnthropicMessagesConfig::with_credential(
            mantle_provider_descriptor(),
            mantle_messages_base_url(region),
            credential,
        )
        .with_auth_wire(AnthropicAuthWire::XApiKey)
        .with_anthropic_version(Some("2023-06-01".to_owned()))
        .with_default_max_output_tokens(default_max_output_tokens);
        Ok(Self {
            adapter: MantleAdapter::new(
                MantleProtocol::Messages,
                AnthropicMessagesAdapter::new(config, http)?,
            ),
            default_model,
            credential_reference,
            model_evidence: None,
            catalog_evidence: None,
            live_evidence: None,
        })
    }

    /// Bind the configured reference for resolution once per operation.
    ///
    /// # Errors
    /// The same construction failures as [`Self::with_credential`]. Missing,
    /// unreadable or rotated credentials fail when an operation starts.
    pub fn from_credentials(
        http: HttpService,
        credentials: &CredentialsService,
        credential: &CredentialQuery,
        region: &AwsRegion,
        default_model: impl Into<String>,
        default_max_output_tokens: Option<u64>,
    ) -> Result<Self, LlmError> {
        Self::with_credential(
            http,
            region,
            RouteCredential::registry(credentials.clone(), credential.clone()),
            default_model,
            default_max_output_tokens,
        )
    }

    pub(crate) fn with_model_evidence(
        mut self,
        evidence: BTreeMap<String, ModelDescriptor>,
    ) -> Self {
        self.model_evidence = Some(evidence);
        self.catalog_evidence = None;
        self.live_evidence = None;
        self
    }

    pub(crate) fn with_catalog_evidence(
        mut self,
        evidence: CatalogRegistry,
        live_evidence: MantleCatalogEvidence,
    ) -> Self {
        self.model_evidence = None;
        self.catalog_evidence = Some(evidence);
        self.live_evidence = Some(live_evidence);
        self
    }
}

#[async_trait::async_trait]
impl Provider for MantleMessagesProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: MANTLE_PROVIDER.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        mantle_provider_descriptor()
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
        prepare_mantle_catalog(
            self.catalog_evidence.as_ref(),
            self.live_evidence.as_ref(),
            MantleProtocol::Messages,
            context,
            cancellation,
        )
        .await?;
        Ok(None)
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        legacy_refusal()
    }
}

impl InferenceAdapter for MantleMessagesProvider {
    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        self.adapter.descriptor()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        require_model_evidence(
            &self.model_evidence,
            self.catalog_evidence.as_ref(),
            MantleProtocol::Messages,
            draft.effective_at_ms,
            model,
        )?;
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

fn require_model_evidence(
    evidence: &Option<BTreeMap<String, ModelDescriptor>>,
    catalog: Option<&CatalogRegistry>,
    protocol: MantleProtocol,
    effective_at_ms: u64,
    model: &ModelDescriptor,
) -> Result<(), ResolveError> {
    if evidence
        .as_ref()
        .is_some_and(|evidence| !evidence.contains_key(&model.id))
    {
        return Err(mantle_evidence_error(
            "selected Mantle model is outside exact protocol evidence",
        ));
    }
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let selected = catalog
        .resolve_model(MANTLE_PROVIDER, &model.id, effective_at_ms)
        .map_err(|_| {
            mantle_evidence_error(
                "selected Mantle model lacks live or trusted cached account evidence",
            )
        })?;
    if selected.descriptor != *model {
        return Err(mantle_evidence_error(
            "selected Mantle descriptor differs from catalog evidence",
        ));
    }
    if protocol == MantleProtocol::Messages {
        return match protocol.maintained_model_support(&model.id) {
            CapabilitySupport::Supported => Ok(()),
            CapabilitySupport::Unsupported => Err(mantle_evidence_error(
                "selected Mantle model is unsupported by Messages",
            )),
            CapabilitySupport::Unknown => Err(mantle_evidence_error(
                "selected Mantle model Messages compatibility is unproven",
            )),
        };
    }
    Ok(())
}

async fn prepare_mantle_catalog(
    catalog: Option<&CatalogRegistry>,
    evidence: Option<&MantleCatalogEvidence>,
    protocol: MantleProtocol,
    context: ProviderOptionContext<'_>,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), LlmError> {
    let Some(catalog) = catalog else {
        return if cancellation.is_cancelled() {
            Err(mantle_preparation_failure(ProviderErrorClass::Cancelled))
        } else {
            Ok(())
        };
    };
    if cancellation.is_cancelled() {
        return Err(mantle_preparation_failure(ProviderErrorClass::Cancelled));
    }
    let evidence =
        evidence.ok_or_else(|| mantle_preparation_failure(ProviderErrorClass::Server))?;
    let mut readiness = evidence
        .take_readiness(&context.model().id)
        .map_err(|_| mantle_preparation_failure(ProviderErrorClass::Server))?;
    if matches!(readiness, MantleCatalogReadiness::RefreshRequired) {
        catalog
            .refresh(MANTLE_PROVIDER, CatalogRefreshMode::Force, cancellation)
            .await
            .map_err(catalog_preparation_error)?;
        readiness = evidence
            .take_readiness(&context.model().id)
            .map_err(|_| mantle_preparation_failure(ProviderErrorClass::Server))?;
    }
    match readiness {
        MantleCatalogReadiness::Ready => {}
        MantleCatalogReadiness::Failed(kind) => {
            return Err(catalog_failure_preparation_error(kind));
        }
        MantleCatalogReadiness::RefreshRequired => {
            return Err(mantle_preparation_failure(
                ProviderErrorClass::InvalidRequest,
            ));
        }
    }
    let snapshot = catalog
        .cached(MANTLE_PROVIDER)
        .map_err(catalog_preparation_error)?;
    let selected = snapshot
        .models
        .iter()
        .find(|model| model.id == context.model().id)
        .filter(|model| *model == context.model())
        .ok_or_else(|| mantle_preparation_failure(ProviderErrorClass::InvalidRequest))?;
    if protocol == MantleProtocol::Messages
        && protocol.maintained_model_support(&selected.id) != CapabilitySupport::Supported
    {
        return Err(mantle_preparation_failure(
            ProviderErrorClass::InvalidRequest,
        ));
    }
    Ok(())
}

fn mantle_preparation_failure(class: ProviderErrorClass) -> LlmError {
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

fn mantle_evidence_error(message: impl Into<String>) -> ResolveError {
    ResolveError::InvalidRequest {
        field: "model_evidence",
        message: message.into(),
    }
}
