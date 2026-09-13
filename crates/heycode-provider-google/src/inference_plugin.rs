//! Effect-owned construction of exact Google inference products.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization_gcp::{
    GcpAccountHealth, GcpAuthProfile, GcpAuthService, GcpLocation, GcpLocationHealth,
    GcpLocationKind, GcpProfileRequest, GcpProjectHealth, GcpProjectId, SERVICE_GCP_AUTH,
};
use heycode_core::{
    Context, ContributionKind, CoreError, NativeToolImplementationKind, Plugin,
    PluginContributionKind, PluginContributionSpec, PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{
    AuthenticationBinding, ChatRequest, ChunkStream, InferenceAdapter, InferenceStream, LlmError,
    ModelDescriptor, Provider, ProviderDescriptor, ProviderErrorClass, ProviderFailure,
    ProviderFailureOrigin, ProviderInfo, ProviderOptionContext, ProviderRegistry, RequestDraft,
    ResolveError, ResolvedCall, RouteCredential, SERVICE_MODELS, SERVICE_PROVIDERS,
};
use heycode_native_tools::{NativeToolImplementation, NativeToolRegistry, SERVICE_NATIVE_TOOLS};
use tokio_util::sync::CancellationToken;

use crate::{
    CLAUDE_VERTEX_PROVIDER, ClaudeVertexControls, ClaudeVertexError, ClaudeVertexProfile,
    ClaudeVertexProvider, CodeExecutionRequest, ExternalGroundingRequest,
    GOOGLE_CODE_EXECUTION_IMPLEMENTATION, GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
    GOOGLE_EXTERNAL_GROUNDING_LOGICAL, GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
    GOOGLE_SEARCH_IMPLEMENTATION, GOOGLE_VERTEX_PROVIDER, GOOGLE_WEB_SEARCH_LOGICAL,
    GeminiCacheRequest, GoogleGeminiProvider, GoogleSearchRequest,
};

/// Explicit selectable-model evidence and provider default for one Gemini
/// product route.
#[derive(Debug, Clone, PartialEq)]
pub struct GoogleGeminiModelEvidence {
    default_model: String,
    models: BTreeMap<String, ModelDescriptor>,
}

impl GoogleGeminiModelEvidence {
    /// Provider-maintained Vertex Gemini 3.7 Flash evidence.
    ///
    /// This is model metadata from the official model card, not evidence that
    /// the configured project can call the model.
    #[must_use]
    pub fn maintained_vertex() -> Self {
        let model = crate::maintained_catalog::vertex_gemini_model_descriptor();
        let default_model = model.id.clone();
        Self {
            default_model,
            models: BTreeMap::from([(model.id.clone(), model)]),
        }
    }

    /// Bind a default to one non-empty unique descriptor generation.
    ///
    /// # Errors
    /// Empty, duplicate, unsafe or default-missing evidence fails.
    pub fn new(
        default_model: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Result<Self, GoogleInferencePluginError> {
        if models.is_empty() {
            return Err(GoogleInferencePluginError::EmptyModelEvidence);
        }
        let default_model = default_model.into();
        if !safe_model_id(&default_model) {
            return Err(GoogleInferencePluginError::InvalidModelEvidence);
        }
        let mut normalized = BTreeMap::new();
        for model in models {
            if !safe_model_id(&model.id) {
                return Err(GoogleInferencePluginError::InvalidModelEvidence);
            }
            if normalized.insert(model.id.clone(), model).is_some() {
                return Err(GoogleInferencePluginError::DuplicateModelEvidence);
            }
        }
        if !normalized.contains_key(&default_model) {
            return Err(GoogleInferencePluginError::DefaultModelNotEvidenced);
        }
        Ok(Self {
            default_model,
            models: normalized,
        })
    }

    fn contains(&self, model: &str) -> bool {
        self.models.contains_key(model)
    }
}

#[derive(Clone)]
struct EvidencedFeature<T> {
    request: T,
    models: BTreeSet<String>,
}

/// Explicit Gemini/Vertex feature policy. `none()` is an affirmative empty
/// policy; configured features never activate outside their exact model set.
#[derive(Clone)]
pub struct GoogleGeminiPolicy {
    search: Option<EvidencedFeature<GoogleSearchRequest>>,
    external_grounding: Option<EvidencedFeature<ExternalGroundingRequest>>,
    code_execution: Option<EvidencedFeature<CodeExecutionRequest>>,
    cache: Option<GeminiCacheRequest>,
}

impl GoogleGeminiPolicy {
    /// Explicitly enable no provider tools and no cache policy.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            search: None,
            external_grounding: None,
            code_execution: None,
            cache: None,
        }
    }

    /// Enable Google Search only for exact evidenced model ids.
    ///
    /// # Errors
    /// Duplicate policy or invalid/empty model evidence fails.
    pub fn with_google_search(
        mut self,
        request: GoogleSearchRequest,
        models: Vec<String>,
    ) -> Result<Self, GoogleInferencePluginError> {
        if self.search.is_some() {
            return Err(GoogleInferencePluginError::DuplicatePolicy);
        }
        self.search = Some(EvidencedFeature {
            request,
            models: model_set(models)?,
        });
        Ok(self)
    }

    /// Enable Vertex external grounding only for exact evidenced model ids.
    ///
    /// # Errors
    /// Duplicate policy or invalid/empty model evidence fails.
    pub fn with_external_grounding(
        mut self,
        request: ExternalGroundingRequest,
        models: Vec<String>,
    ) -> Result<Self, GoogleInferencePluginError> {
        if self.external_grounding.is_some() {
            return Err(GoogleInferencePluginError::DuplicatePolicy);
        }
        self.external_grounding = Some(EvidencedFeature {
            request,
            models: model_set(models)?,
        });
        Ok(self)
    }

    /// Enable provider code execution only for exact evidenced model ids.
    ///
    /// # Errors
    /// Duplicate policy or invalid/empty model evidence fails.
    pub fn with_code_execution(
        mut self,
        request: CodeExecutionRequest,
        models: Vec<String>,
    ) -> Result<Self, GoogleInferencePluginError> {
        if self.code_execution.is_some() {
            return Err(GoogleInferencePluginError::DuplicatePolicy);
        }
        self.code_execution = Some(EvidencedFeature {
            request,
            models: model_set(models)?,
        });
        Ok(self)
    }

    /// Attach one explicit or implicit cache policy.
    ///
    /// # Errors
    /// A second cache policy fails instead of replacing the first.
    pub fn with_cache(
        mut self,
        request: GeminiCacheRequest,
    ) -> Result<Self, GoogleInferencePluginError> {
        if self.cache.is_some() {
            return Err(GoogleInferencePluginError::DuplicatePolicy);
        }
        self.cache = Some(request);
        Ok(self)
    }

    fn validate_models(
        &self,
        evidence: &GoogleGeminiModelEvidence,
    ) -> Result<(), GoogleInferencePluginError> {
        for models in [
            self.search.as_ref().map(|feature| &feature.models),
            self.external_grounding
                .as_ref()
                .map(|feature| &feature.models),
            self.code_execution.as_ref().map(|feature| &feature.models),
        ]
        .into_iter()
        .flatten()
        {
            if models.iter().any(|model| !evidence.contains(model)) {
                return Err(GoogleInferencePluginError::PolicyModelNotEvidenced);
            }
        }
        Ok(())
    }

    fn has_candidates(&self) -> bool {
        self.search.is_some() || self.external_grounding.is_some() || self.code_execution.is_some()
    }
}

#[derive(Clone)]
enum GoogleInferenceRoute {
    Developer {
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
    },
    Vertex {
        base_url: String,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
    },
    ClaudeVertex {
        profile: ClaudeVertexProfile,
        controls: ClaudeVertexControls,
    },
    LazyVertex {
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
    },
    LazyClaudeVertex {
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        controls: ClaudeVertexControls,
    },
}

/// Complete explicit construction input for one Google inference product.
#[derive(Clone)]
pub struct GoogleInferencePluginConfig {
    route: GoogleInferenceRoute,
}

impl GoogleInferencePluginConfig {
    /// Build the Developer API route for explicit late activation, preserving
    /// model evidence and Settings-derived cache policy. Hosted-tool policies
    /// require primary composition to own their native registry contributions.
    ///
    /// # Errors
    /// Other product routes, hosted tools needing context-owned registration,
    /// or invalid provider construction fail without publishing a provider.
    pub fn build_developer_provider(
        &self,
        http: HttpService,
        credentials: &CredentialsService,
    ) -> Result<GoogleGeminiProvider, CoreError> {
        let GoogleInferenceRoute::Developer {
            credential,
            evidence,
            policy,
        } = &self.route
        else {
            return Err(CoreError::other(
                "late Google activation requires a Developer API route",
            ));
        };
        if policy.has_candidates() {
            return Err(CoreError::other(
                "Google hosted tools require a separately composed primary connection; late activation cannot omit their registry ownership",
            ));
        }
        build_gemini_provider(
            GoogleGeminiProvider::developer(
                http,
                RouteCredential::registry(credentials.clone(), credential.clone()),
                credential.reference.as_str(),
                evidence.default_model.clone(),
            )
            .map_err(|_| CoreError::other("Gemini Developer provider is invalid"))?,
            evidence,
            policy,
        )
    }

    /// Construct the Gemini Developer API product.
    ///
    /// # Errors
    /// Wrong credential kind, unevidenced policy or Vertex-only feature policy.
    pub fn developer(
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
    ) -> Result<Self, GoogleInferencePluginError> {
        if credential.kind.as_str() != "api-key" {
            return Err(GoogleInferencePluginError::CredentialKindMismatch);
        }
        if policy.external_grounding.is_some() {
            return Err(GoogleInferencePluginError::ProductPolicyMismatch);
        }
        if policy
            .cache
            .as_ref()
            .is_some_and(|cache| !cache.supports_provider(crate::catalog::GOOGLE_PROVIDER))
        {
            return Err(GoogleInferencePluginError::ProductPolicyMismatch);
        }
        policy.validate_models(&evidence)?;
        Ok(Self {
            route: GoogleInferenceRoute::Developer {
                credential,
                evidence,
                policy,
            },
        })
    }

    /// Construct the Vertex Gemini product for one explicit endpoint base.
    ///
    /// # Errors
    /// Wrong credential kind, malformed endpoint, unevidenced policy or a
    /// Developer-only Search/code policy.
    pub fn vertex(
        base_url: impl Into<String>,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
    ) -> Result<Self, GoogleInferencePluginError> {
        if credential.kind.as_str() != "oauth-token" {
            return Err(GoogleInferencePluginError::CredentialKindMismatch);
        }
        if policy.search.is_some() || policy.code_execution.is_some() {
            return Err(GoogleInferencePluginError::ProductPolicyMismatch);
        }
        if policy
            .cache
            .as_ref()
            .is_some_and(|cache| !cache.supports_provider(GOOGLE_VERTEX_PROVIDER))
        {
            return Err(GoogleInferencePluginError::ProductPolicyMismatch);
        }
        policy.validate_models(&evidence)?;
        let base_url = base_url.into();
        if !base_url.starts_with("https://")
            || base_url.contains(['?', '#'])
            || heycode_http::HttpRequest::post(&base_url, Vec::new()).is_err()
        {
            return Err(GoogleInferencePluginError::InvalidEndpoint);
        }
        Ok(Self {
            route: GoogleInferenceRoute::Vertex {
                base_url,
                credential,
                evidence,
                policy,
            },
        })
    }

    /// Construct exact-model Claude on Vertex with explicit thinking policy.
    ///
    /// # Errors
    /// A profile not bound to the Claude Vertex provider is refused.
    pub fn claude_vertex(
        profile: ClaudeVertexProfile,
        controls: ClaudeVertexControls,
    ) -> Result<Self, GoogleInferencePluginError> {
        if profile.provider_profile().descriptor.id != CLAUDE_VERTEX_PROVIDER {
            return Err(GoogleInferencePluginError::ProductPolicyMismatch);
        }
        Ok(Self {
            route: GoogleInferenceRoute::ClaudeVertex { profile, controls },
        })
    }

    /// Construct a lazy Vertex Gemini route over the composed GCP profile
    /// service and the provider-maintained Gemini 3.7 Flash catalog row.
    ///
    /// Composition performs no profile, credential, or HTTP operation. The
    /// first inference preparation resolves this exact request under caller
    /// cancellation and builds the concrete endpoint-bound provider.
    ///
    /// # Errors
    /// Project/location must be explicit and valid, the credential must be an
    /// OAuth token reference, and policy must belong to the maintained Vertex
    /// product/model.
    pub fn lazy_vertex(
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        policy: GoogleGeminiPolicy,
    ) -> Result<Self, GoogleInferencePluginError> {
        validate_explicit_profile(&profile)?;
        let evidence = GoogleGeminiModelEvidence::maintained_vertex();
        validate_vertex_policy(&credential, &evidence, &policy)?;
        Ok(Self {
            route: GoogleInferenceRoute::LazyVertex {
                profile,
                credential,
                evidence,
                policy,
            },
        })
    }

    /// Construct a lazy Claude-on-Vertex route over the composed GCP profile
    /// service and the provider-maintained Sonnet 5 catalog row.
    ///
    /// # Errors
    /// Project/location must be explicit and valid and the credential must be
    /// an OAuth token reference.
    pub fn lazy_claude_vertex(
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        controls: ClaudeVertexControls,
    ) -> Result<Self, GoogleInferencePluginError> {
        let location = validate_explicit_profile(&profile)?;
        if location.kind() != GcpLocationKind::Global {
            return Err(GoogleInferencePluginError::ProfileRouteInvalid);
        }
        if credential.kind.as_str() != "oauth-token" {
            return Err(GoogleInferencePluginError::CredentialKindMismatch);
        }
        Ok(Self {
            route: GoogleInferenceRoute::LazyClaudeVertex {
                profile,
                credential,
                controls,
            },
        })
    }

    fn policy(&self) -> Option<&GoogleGeminiPolicy> {
        match &self.route {
            GoogleInferenceRoute::Developer { policy, .. }
            | GoogleInferenceRoute::Vertex { policy, .. }
            | GoogleInferenceRoute::LazyVertex { policy, .. } => Some(policy),
            GoogleInferenceRoute::ClaudeVertex { .. }
            | GoogleInferenceRoute::LazyClaudeVertex { .. } => None,
        }
    }

    fn owns_catalog(&self) -> bool {
        matches!(
            &self.route,
            GoogleInferenceRoute::LazyVertex { .. } | GoogleInferenceRoute::LazyClaudeVertex { .. }
        )
    }

    fn has_candidates(&self) -> bool {
        self.policy()
            .is_some_and(GoogleGeminiPolicy::has_candidates)
    }
}

fn validate_explicit_profile(
    profile: &GcpProfileRequest,
) -> Result<GcpLocation, GoogleInferencePluginError> {
    let project = profile
        .project
        .as_ref()
        .ok_or(GoogleInferencePluginError::ProfileRouteInvalid)?;
    let location = profile
        .location
        .as_ref()
        .ok_or(GoogleInferencePluginError::ProfileRouteInvalid)?;
    GcpProjectId::new(project.clone())
        .map_err(|_| GoogleInferencePluginError::ProfileRouteInvalid)?;
    GcpLocation::new(location.clone()).map_err(|_| GoogleInferencePluginError::ProfileRouteInvalid)
}

fn validate_vertex_policy(
    credential: &CredentialQuery,
    evidence: &GoogleGeminiModelEvidence,
    policy: &GoogleGeminiPolicy,
) -> Result<(), GoogleInferencePluginError> {
    if credential.kind.as_str() != "oauth-token" {
        return Err(GoogleInferencePluginError::CredentialKindMismatch);
    }
    if policy.search.is_some() || policy.code_execution.is_some() {
        return Err(GoogleInferencePluginError::ProductPolicyMismatch);
    }
    if policy
        .cache
        .as_ref()
        .is_some_and(|cache| !cache.supports_provider(GOOGLE_VERTEX_PROVIDER))
    {
        return Err(GoogleInferencePluginError::ProductPolicyMismatch);
    }
    policy.validate_models(evidence)
}

/// Stable construction failure that never echoes endpoint/model/credential
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleInferencePluginError {
    /// Credential semantic kind does not match the product auth wire.
    CredentialKindMismatch,
    /// No model evidence was supplied.
    EmptyModelEvidence,
    /// Model evidence repeats an id.
    DuplicateModelEvidence,
    /// Model identity is unsafe for the route.
    InvalidModelEvidence,
    /// The default is absent from the evidence set.
    DefaultModelNotEvidenced,
    /// Feature policy repeats a family.
    DuplicatePolicy,
    /// Feature policy names a model outside product evidence.
    PolicyModelNotEvidenced,
    /// Policy belongs to another Google product.
    ProductPolicyMismatch,
    /// Vertex endpoint is invalid.
    InvalidEndpoint,
    /// Lazy project/location route is missing or malformed.
    ProfileRouteInvalid,
}

impl std::fmt::Display for GoogleInferencePluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CredentialKindMismatch => "Google inference credential kind is incompatible",
            Self::EmptyModelEvidence => "Google inference model evidence is empty",
            Self::DuplicateModelEvidence => "Google inference model evidence contains a duplicate",
            Self::InvalidModelEvidence => "Google inference model evidence is invalid",
            Self::DefaultModelNotEvidenced => "Google inference default model is not evidenced",
            Self::DuplicatePolicy => "Google inference policy contains a duplicate",
            Self::PolicyModelNotEvidenced => "Google inference policy model is not evidenced",
            Self::ProductPolicyMismatch => "Google inference policy belongs to another product",
            Self::InvalidEndpoint => "Google inference endpoint is invalid",
            Self::ProfileRouteInvalid => "Google inference profile route is invalid",
        })
    }
}

impl std::error::Error for GoogleInferencePluginError {}

#[derive(Clone)]
enum LazyPreparedRoute {
    Vertex {
        evidence: GoogleGeminiModelEvidence,
        policy: Box<GoogleGeminiPolicy>,
    },
    Claude {
        controls: ClaudeVertexControls,
    },
}

/// Inert composition-time provider that prepares an exact Vertex operation
/// provider through the composed GCP profile service.
struct LazyVertexProvider {
    descriptor: ProviderDescriptor,
    model: ModelDescriptor,
    profile: GcpProfileRequest,
    credential: CredentialQuery,
    authentication: AuthenticationBinding,
    gcp: Arc<GcpAuthService>,
    http: HttpService,
    credentials: CredentialsService,
    route: LazyPreparedRoute,
}

impl LazyVertexProvider {
    fn vertex(
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        evidence: GoogleGeminiModelEvidence,
        policy: GoogleGeminiPolicy,
        gcp: Arc<GcpAuthService>,
        http: HttpService,
        credentials: CredentialsService,
    ) -> Self {
        let model = crate::maintained_catalog::vertex_gemini_model_descriptor();
        let authentication =
            RouteCredential::registry(credentials.clone(), credential.clone()).binding();
        Self {
            descriptor: crate::maintained_catalog::vertex_gemini_provider_descriptor(),
            model,
            profile,
            credential,
            authentication,
            gcp,
            http,
            credentials,
            route: LazyPreparedRoute::Vertex {
                evidence,
                policy: Box::new(policy),
            },
        }
    }

    fn claude(
        profile: GcpProfileRequest,
        credential: CredentialQuery,
        controls: ClaudeVertexControls,
        gcp: Arc<GcpAuthService>,
        http: HttpService,
        credentials: CredentialsService,
    ) -> Self {
        let authentication =
            RouteCredential::registry(credentials.clone(), credential.clone()).binding();
        Self {
            descriptor: crate::claude_vertex::provider_descriptor(),
            model: crate::claude_vertex::sonnet_five_descriptor(),
            profile,
            credential,
            authentication,
            gcp,
            http,
            credentials,
            route: LazyPreparedRoute::Claude { controls },
        }
    }

    fn validate_context(&self, context: ProviderOptionContext<'_>) -> Result<(), LlmError> {
        if context.model() != &self.model {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
        for route in context
            .native_tool_routes()
            .iter()
            .filter(|route| route.kind() == NativeToolImplementationKind::Provider)
        {
            if route.provider() != Some(self.descriptor.id.as_str()) {
                return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
            }
            let supported = match &self.route {
                LazyPreparedRoute::Vertex { policy, .. } => [
                    policy.search.as_ref().map(|feature| {
                        (
                            GOOGLE_WEB_SEARCH_LOGICAL,
                            GOOGLE_SEARCH_IMPLEMENTATION,
                            &feature.models,
                        )
                    }),
                    policy.external_grounding.as_ref().map(|feature| {
                        (
                            GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
                            GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
                            &feature.models,
                        )
                    }),
                    policy.code_execution.as_ref().map(|feature| {
                        (
                            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
                            GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
                            &feature.models,
                        )
                    }),
                ]
                .into_iter()
                .flatten()
                .any(|(logical, implementation, models)| {
                    route.logical() == logical
                        && route.implementation() == implementation
                        && models.contains(&self.model.id)
                }),
                LazyPreparedRoute::Claude { .. } => false,
            };
            if !supported {
                return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
            }
        }
        Ok(())
    }

    fn prepare_vertex(
        &self,
        profile: &GcpAuthProfile,
        context: ProviderOptionContext<'_>,
        evidence: &GoogleGeminiModelEvidence,
        policy: &GoogleGeminiPolicy,
    ) -> Result<Arc<dyn Provider>, LlmError> {
        let base_url = vertex_gemini_base_url(profile)?;
        let provider = GoogleGeminiProvider::vertex(
            self.http.clone(),
            base_url,
            RouteCredential::registry(self.credentials.clone(), self.credential.clone()),
            self.credential.reference.as_str(),
            evidence.default_model.clone(),
        )
        .map_err(|_| local_provider_failure(ProviderErrorClass::InvalidRequest))?;
        let provider = build_gemini_provider(provider, evidence, policy)
            .map_err(|_| local_provider_failure(ProviderErrorClass::InvalidRequest))?;
        provider
            .request_options_for(context)
            .map_err(|_| local_provider_failure(ProviderErrorClass::InvalidRequest))?;
        if Provider::descriptor(&provider) != self.descriptor
            || provider.describe_model(&self.model.id) != self.model
        {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
        Ok(Arc::new(provider))
    }

    fn prepare_claude(
        &self,
        profile: &GcpAuthProfile,
        controls: ClaudeVertexControls,
    ) -> Result<Arc<dyn Provider>, LlmError> {
        let profile = ClaudeVertexProfile::from_gcp(profile, self.credential.clone())
            .map_err(map_claude_profile_error)?;
        if profile.model() != &self.model {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
        let provider = ClaudeVertexProvider::new_with_controls(
            profile,
            self.http.clone(),
            &self.credentials,
            controls,
        )
        .map_err(|_| local_provider_failure(ProviderErrorClass::InvalidRequest))?;
        if Provider::descriptor(&provider) != self.descriptor
            || provider.describe_model(&self.model.id) != self.model
        {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
        Ok(Arc::new(provider))
    }
}

#[async_trait]
impl Provider for LazyVertexProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.descriptor.id.clone(),
            default_model: self.model.id.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(self.credential.reference.as_str())
    }

    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        if model == self.model.id {
            self.model.clone()
        } else {
            ModelDescriptor::unknown(model)
        }
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    async fn prepare_inference(
        &self,
        context: ProviderOptionContext<'_>,
        cancellation: CancellationToken,
    ) -> Result<Option<Arc<dyn Provider>>, LlmError> {
        if cancellation.is_cancelled() {
            return Err(local_provider_failure(ProviderErrorClass::Cancelled));
        }
        self.validate_context(context)?;
        let profile = self
            .gcp
            .resolve(self.profile.clone(), cancellation.clone())
            .await;
        if cancellation.is_cancelled() {
            return Err(local_provider_failure(ProviderErrorClass::Cancelled));
        }
        let provider = match &self.route {
            LazyPreparedRoute::Vertex { evidence, policy } => {
                self.prepare_vertex(&profile, context, evidence, policy)?
            }
            LazyPreparedRoute::Claude { controls } => self.prepare_claude(&profile, *controls)?,
        };
        Ok(Some(provider))
    }

    fn request_options_for(
        &self,
        _context: ProviderOptionContext<'_>,
    ) -> Result<Vec<heycode_core::ProviderRequestOption>, ResolveError> {
        Err(ResolveError::InvalidAdapter {
            field: "prepare_inference",
            message: "lazy Vertex provider must be prepared before request options".to_owned(),
        })
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(local_provider_failure(ProviderErrorClass::InvalidRequest))
        }))
    }
}

impl InferenceAdapter for LazyVertexProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.authentication.clone()
    }

    fn resolve(
        &self,
        _draft: RequestDraft,
        _model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        Err(ResolveError::InvalidAdapter {
            field: "prepare_inference",
            message: "lazy Vertex provider must be prepared before resolution".to_owned(),
        })
    }

    fn stream(&self, _call: ResolvedCall) -> InferenceStream {
        Box::pin(futures::stream::once(async {
            Err(local_provider_failure(ProviderErrorClass::InvalidRequest))
        }))
    }
}

fn vertex_gemini_base_url(profile: &GcpAuthProfile) -> Result<String, LlmError> {
    match profile.account() {
        GcpAccountHealth::Configured { .. } => {}
        GcpAccountHealth::Absent | GcpAccountHealth::Faulted { .. } => {
            return Err(local_provider_failure(ProviderErrorClass::Authentication));
        }
        GcpAccountHealth::Undetermined { .. } => {
            return Err(local_provider_failure(ProviderErrorClass::Authentication));
        }
    }
    let project = match profile.project() {
        GcpProjectHealth::Confirmed { project, .. }
        | GcpProjectHealth::Unconfirmed { project, .. } => project.as_str(),
        GcpProjectHealth::Unset
        | GcpProjectHealth::Malformed { .. }
        | GcpProjectHealth::Undetermined { .. } => {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
    };
    let location = match profile.location() {
        GcpLocationHealth::Selected { location, .. } => location,
        GcpLocationHealth::Unset
        | GcpLocationHealth::Malformed { .. }
        | GcpLocationHealth::Undetermined { .. } => {
            return Err(local_provider_failure(ProviderErrorClass::InvalidRequest));
        }
    };
    let host = if location.kind() == GcpLocationKind::Global {
        "aiplatform.googleapis.com".to_owned()
    } else {
        format!("{}-aiplatform.googleapis.com", location.as_str())
    };
    Ok(format!(
        "https://{host}/v1/projects/{project}/locations/{}/publishers/google",
        location.as_str()
    ))
}

fn map_claude_profile_error(error: ClaudeVertexError) -> LlmError {
    let class = match error {
        ClaudeVertexError::AccountUnavailable
        | ClaudeVertexError::AccountUndetermined
        | ClaudeVertexError::CredentialUnavailable => ProviderErrorClass::Authentication,
        ClaudeVertexError::Cancelled => ProviderErrorClass::Cancelled,
        _ => ProviderErrorClass::InvalidRequest,
    };
    local_provider_failure(class)
}

fn local_provider_failure(class: ProviderErrorClass) -> LlmError {
    LlmError::Provider(ProviderFailure::new(class, ProviderFailureOrigin::Local))
}

/// Register one exact Google inference provider and only its explicitly
/// configured native candidates as Context effects.
#[must_use]
pub fn google_inference_plugin(config: GoogleInferencePluginConfig) -> Box<dyn Plugin> {
    struct GoogleInferencePlugin(GoogleInferencePluginConfig);

    impl Plugin for GoogleInferencePlugin {
        fn name(&self) -> &'static str {
            match &self.0.route {
                GoogleInferenceRoute::Developer { .. } => "inference-google-gemini",
                GoogleInferenceRoute::Vertex { .. } | GoogleInferenceRoute::LazyVertex { .. } => {
                    "inference-google-vertex"
                }
                GoogleInferenceRoute::ClaudeVertex { .. }
                | GoogleInferenceRoute::LazyClaudeVertex { .. } => "inference-google-claude-vertex",
            }
        }

        fn descriptor(&self) -> PluginDescriptor {
            let kinds = if self.0.has_candidates() {
                &[
                    PluginContributionKind::Provider,
                    PluginContributionKind::Tool,
                ][..]
            } else {
                &[PluginContributionKind::Provider][..]
            };
            PluginDescriptor::built_in(self.name(), env!("CARGO_PKG_VERSION"), kinds)
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            let (provider, policy) = match &self.0.route {
                GoogleInferenceRoute::Developer { policy, .. } => {
                    (crate::catalog::GOOGLE_PROVIDER, Some(policy))
                }
                GoogleInferenceRoute::Vertex { policy, .. } => {
                    (GOOGLE_VERTEX_PROVIDER, Some(policy))
                }
                GoogleInferenceRoute::LazyVertex { policy, .. } => {
                    (GOOGLE_VERTEX_PROVIDER, Some(policy))
                }
                GoogleInferenceRoute::ClaudeVertex { .. }
                | GoogleInferenceRoute::LazyClaudeVertex { .. } => (CLAUDE_VERTEX_PROVIDER, None),
            };
            let mut rows = vec![PluginContributionSpec::new(
                ContributionKind::InferenceProvider,
                provider,
            )];
            if self.0.owns_catalog() {
                rows.push(PluginContributionSpec::new(
                    ContributionKind::ModelCatalog,
                    provider,
                ));
            }
            if let Some(policy) = policy {
                if policy.search.is_some() {
                    rows.push(PluginContributionSpec::new(
                        ContributionKind::NativeTool,
                        GOOGLE_SEARCH_IMPLEMENTATION,
                    ));
                }
                if policy.external_grounding.is_some() {
                    rows.push(PluginContributionSpec::new(
                        ContributionKind::NativeTool,
                        GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
                    ));
                }
                if policy.code_execution.is_some() {
                    rows.push(PluginContributionSpec::new(
                        ContributionKind::NativeTool,
                        GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
                    ));
                }
            }
            rows
        }

        fn inject(&self) -> &'static [ServiceKey] {
            match (self.0.owns_catalog(), self.0.has_candidates()) {
                (true, true) => &[
                    SERVICE_PROVIDERS,
                    SERVICE_MODELS,
                    SERVICE_HTTP,
                    SERVICE_CREDENTIALS,
                    SERVICE_GCP_AUTH,
                    SERVICE_NATIVE_TOOLS,
                ],
                (true, false) => &[
                    SERVICE_PROVIDERS,
                    SERVICE_MODELS,
                    SERVICE_HTTP,
                    SERVICE_CREDENTIALS,
                    SERVICE_GCP_AUTH,
                ],
                (false, true) => &[
                    SERVICE_PROVIDERS,
                    SERVICE_HTTP,
                    SERVICE_CREDENTIALS,
                    SERVICE_NATIVE_TOOLS,
                ],
                (false, false) => &[SERVICE_PROVIDERS, SERVICE_HTTP, SERVICE_CREDENTIALS],
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let providers = context
                .get::<ProviderRegistry>(SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_PROVIDERS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            if self.0.owns_catalog() {
                let models = context
                    .get::<heycode_llm::CatalogRegistry>(SERVICE_MODELS)
                    .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
                let source: Arc<dyn heycode_llm::ModelCatalog> =
                    match &self.0.route {
                        GoogleInferenceRoute::LazyVertex { credential, .. } => {
                            let gcp = context.get::<GcpAuthService>(SERVICE_GCP_AUTH).ok_or_else(
                                || CoreError::MissingService(SERVICE_GCP_AUTH.to_string()),
                            )?;
                            Arc::new(
                                crate::VertexGeminiCatalog::oauth_token(
                                    http.as_ref().clone(),
                                    credentials.clone(),
                                    gcp,
                                    credential.clone(),
                                )
                                .map_err(|error| CoreError::other(error.message()))?,
                            )
                        }
                        GoogleInferenceRoute::LazyClaudeVertex { .. } => {
                            Arc::new(crate::MaintainedVertexCatalog::claude_vertex())
                        }
                        _ => {
                            return Err(CoreError::other(
                                "Google inference catalog ownership is inconsistent",
                            ));
                        }
                    };
                models
                    .register(context, source)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            let provider: Arc<dyn Provider> = match &self.0.route {
                GoogleInferenceRoute::Developer {
                    credential,
                    evidence,
                    policy,
                } => Arc::new(build_gemini_provider(
                    GoogleGeminiProvider::developer(
                        http.as_ref().clone(),
                        RouteCredential::registry(credentials.as_ref().clone(), credential.clone()),
                        credential.reference.as_str(),
                        evidence.default_model.clone(),
                    )
                    .map_err(|_| CoreError::other("Gemini Developer provider is invalid"))?,
                    evidence,
                    policy,
                )?),
                GoogleInferenceRoute::Vertex {
                    base_url,
                    credential,
                    evidence,
                    policy,
                } => Arc::new(build_gemini_provider(
                    GoogleGeminiProvider::vertex(
                        http.as_ref().clone(),
                        base_url,
                        RouteCredential::registry(credentials.as_ref().clone(), credential.clone()),
                        credential.reference.as_str(),
                        evidence.default_model.clone(),
                    )
                    .map_err(|_| CoreError::other("Vertex Gemini provider is invalid"))?,
                    evidence,
                    policy,
                )?),
                GoogleInferenceRoute::ClaudeVertex { profile, controls } => Arc::new(
                    ClaudeVertexProvider::new_with_controls(
                        profile.clone(),
                        http.as_ref().clone(),
                        credentials.as_ref(),
                        *controls,
                    )
                    .map_err(|_| CoreError::other("Claude Vertex provider is invalid"))?,
                ),
                GoogleInferenceRoute::LazyVertex {
                    profile,
                    credential,
                    evidence,
                    policy,
                } => {
                    let gcp = context
                        .get::<GcpAuthService>(SERVICE_GCP_AUTH)
                        .ok_or_else(|| CoreError::MissingService(SERVICE_GCP_AUTH.to_string()))?;
                    Arc::new(LazyVertexProvider::vertex(
                        profile.clone(),
                        credential.clone(),
                        evidence.clone(),
                        policy.clone(),
                        gcp,
                        http.as_ref().clone(),
                        credentials.as_ref().clone(),
                    ))
                }
                GoogleInferenceRoute::LazyClaudeVertex {
                    profile,
                    credential,
                    controls,
                } => {
                    let gcp = context
                        .get::<GcpAuthService>(SERVICE_GCP_AUTH)
                        .ok_or_else(|| CoreError::MissingService(SERVICE_GCP_AUTH.to_string()))?;
                    Arc::new(LazyVertexProvider::claude(
                        profile.clone(),
                        credential.clone(),
                        *controls,
                        gcp,
                        http.as_ref().clone(),
                        credentials.as_ref().clone(),
                    ))
                }
            };
            let registration = providers
                .register_owned(provider)
                .map_err(CoreError::DuplicatePlugin)?;
            context.effect(move || drop(registration));
            if let Some(policy) = self.0.policy().filter(|policy| policy.has_candidates()) {
                register_candidates(context, policy, provider_id(&self.0.route))?;
            }
            Ok(())
        }
    }

    Box::new(GoogleInferencePlugin(config))
}

fn build_gemini_provider(
    mut provider: GoogleGeminiProvider,
    evidence: &GoogleGeminiModelEvidence,
    policy: &GoogleGeminiPolicy,
) -> Result<GoogleGeminiProvider, CoreError> {
    if let Some(feature) = &policy.search {
        provider = provider
            .with_google_search(
                feature.request.clone(),
                feature.models.iter().cloned().collect(),
            )
            .map_err(|_| CoreError::other("Google Search provider policy is invalid"))?;
    }
    if let Some(feature) = &policy.external_grounding {
        provider = provider
            .with_external_grounding(
                feature.request.clone(),
                feature.models.iter().cloned().collect(),
            )
            .map_err(|_| CoreError::other("Vertex grounding provider policy is invalid"))?;
    }
    if let Some(feature) = &policy.code_execution {
        provider = provider
            .with_code_execution(feature.request, feature.models.iter().cloned().collect())
            .map_err(|_| CoreError::other("Google code provider policy is invalid"))?;
    }
    if let Some(cache) = &policy.cache {
        provider = provider
            .with_cache(cache.clone())
            .map_err(|_| CoreError::other("Google cache provider policy is invalid"))?;
    }
    Ok(provider.with_model_evidence(evidence.models.clone()))
}

fn provider_id(route: &GoogleInferenceRoute) -> &'static str {
    match route {
        GoogleInferenceRoute::Developer { .. } => crate::catalog::GOOGLE_PROVIDER,
        GoogleInferenceRoute::Vertex { .. } | GoogleInferenceRoute::LazyVertex { .. } => {
            GOOGLE_VERTEX_PROVIDER
        }
        GoogleInferenceRoute::ClaudeVertex { .. }
        | GoogleInferenceRoute::LazyClaudeVertex { .. } => CLAUDE_VERTEX_PROVIDER,
    }
}

fn register_candidates(
    context: &Context,
    policy: &GoogleGeminiPolicy,
    provider: &'static str,
) -> Result<(), CoreError> {
    let native = context
        .get::<NativeToolRegistry>(SERVICE_NATIVE_TOOLS)
        .ok_or_else(|| CoreError::MissingService(SERVICE_NATIVE_TOOLS.to_string()))?;
    for (enabled, logical, implementation) in [
        (
            policy.search.is_some(),
            GOOGLE_WEB_SEARCH_LOGICAL,
            GOOGLE_SEARCH_IMPLEMENTATION,
        ),
        (
            policy.external_grounding.is_some(),
            GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
            GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
        ),
        (
            policy.code_execution.is_some(),
            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
            GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
        ),
    ] {
        if !enabled {
            continue;
        }
        let implementation = NativeToolImplementation::new(
            logical,
            implementation,
            NativeToolImplementationKind::Provider,
            Some(provider.to_owned()),
            100,
        )
        .map_err(|error| CoreError::other(error.to_string()))?;
        native
            .register(context, implementation)
            .map_err(|error| CoreError::other(error.to_string()))?;
    }
    Ok(())
}

fn model_set(models: Vec<String>) -> Result<BTreeSet<String>, GoogleInferencePluginError> {
    let count = models.len();
    let models = models.into_iter().collect::<BTreeSet<_>>();
    if models.is_empty()
        || models.len() != count
        || models.iter().any(|model| !safe_model_id(model))
    {
        Err(GoogleInferencePluginError::InvalidModelEvidence)
    } else {
        Ok(models)
    }
}

fn safe_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model.trim() == model
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}
