//! Request-specific Gemini provider wrapper and shared normalizer bridges.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use heycode_core::{NativeToolImplementationKind, NativeToolRoute, ProviderRequestOption};
use heycode_credentials::CredentialsService;
use heycode_http::HttpService;
use heycode_llm::{
    AnthropicAuthWire, AnthropicMessagesAdapter, AnthropicMessagesConfig, AnthropicMessagesDialect,
    AnthropicThinkingMode, AuthenticationBinding, CapabilitySupport, ChatRequest, ChunkStream,
    GeminiAdapter, GeminiConfig, GeminiExtensionFault, GeminiProviderOptionPlan,
    GeminiProviderOptionWire, GeminiStreamNormalizer, GeminiStreamNormalizerFactory,
    InferenceAdapter, InferenceEvent, InferenceStream, LlmError, ModelDescriptor, NativeFeature,
    Provider, ProviderDescriptor, ProviderInfo, ProviderOptionContext, ReasoningEffortId,
    RequestDraft, RequestedCapability, ResolveError, ResolvedCall, RouteCredential,
};

use crate::{
    CLAUDE_VERTEX_ANTHROPIC_VERSION, ClaudeVertexControls, ClaudeVertexProfile,
    ClaudeVertexThinking, CodeExecutionProjector, CodeExecutionRequest, ExternalGroundingRequest,
    GOOGLE_CODE_EXECUTION_IMPLEMENTATION, GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
    GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL, GOOGLE_SEARCH_IMPLEMENTATION, GOOGLE_VERTEX_PROVIDER,
    GOOGLE_WEB_SEARCH_LOGICAL, GeminiCacheMode, GeminiCacheRequest, GeminiCacheUsage,
    GoogleSearchRequest, GroundingProjector,
};

const GOOGLE_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
/// Provider implementation id reserved for Vertex external grounding.
pub const GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION: &str = "vertex-google:external_grounding";

/// Production Claude-on-Vertex provider using the shared Messages serializer,
/// parser, tool normalization, reasoning state and replay boundary.
pub struct ClaudeVertexProvider {
    descriptor: ProviderDescriptor,
    default_model: String,
    model: ModelDescriptor,
    credential_reference: String,
    adapter: AnthropicMessagesAdapter,
}

impl ClaudeVertexProvider {
    /// Bind a validated profile to operation-time Google Cloud credentials.
    ///
    /// # Errors
    /// Invalid route metadata, endpoint/model dialect, reasoning ids or shared
    /// Messages configuration fails before the provider can be published.
    pub fn new(
        profile: ClaudeVertexProfile,
        http: HttpService,
        credentials: &CredentialsService,
    ) -> Result<Self, LlmError> {
        Self::new_with_controls(
            profile,
            http,
            credentials,
            ClaudeVertexControls::sonnet_five_default(),
        )
    }

    /// Bind the route with one explicit provider-owned thinking/effort policy.
    ///
    /// # Errors
    /// The same failures as [`Self::new`].
    pub fn new_with_controls(
        profile: ClaudeVertexProfile,
        http: HttpService,
        credentials: &CredentialsService,
        controls: ClaudeVertexControls,
    ) -> Result<Self, LlmError> {
        let provider_profile = profile.provider_profile();
        let descriptor = provider_profile.descriptor;
        let default_model = provider_profile.default_model;
        let credential_reference = provider_profile
            .credential_reference
            .ok_or_else(|| invalid("Claude on Vertex profile has no credential reference"))?;
        let model = profile.model().clone();
        let credential =
            RouteCredential::registry(credentials.clone(), profile.credential_query().clone());
        let dialect = AnthropicMessagesDialect::exact_model_endpoint(profile.endpoint(), &model.id)
            .with_body_field(
                "anthropic_version",
                serde_json::json!(CLAUDE_VERTEX_ANTHROPIC_VERSION),
            );
        let mut thinking = Vec::with_capacity(5);
        for effort_name in ["low", "medium", "high", "xhigh", "max"] {
            let effort = reasoning_effort(effort_name)?;
            let mode = match controls.thinking() {
                ClaudeVertexThinking::Adaptive => AnthropicThinkingMode::adaptive(None),
                ClaudeVertexThinking::Disabled => AnthropicThinkingMode::disabled(),
            }
            .with_wire_effort(effort_name);
            thinking.push((effort, mode));
        }
        let default_effort = reasoning_effort(controls.effort().as_str())?;
        let config = AnthropicMessagesConfig::with_credential(
            descriptor.clone(),
            profile.endpoint(),
            credential,
        )
        .with_auth_wire(AnthropicAuthWire::Bearer)
        .with_anthropic_version(None)
        .with_dialect(dialect)
        .with_default_max_output_tokens(profile.model().max_output_tokens)
        .with_thinking(thinking, Some(default_effort));
        let adapter = AnthropicMessagesAdapter::new(config, http)?;
        Ok(Self {
            descriptor,
            default_model,
            model,
            credential_reference,
            adapter,
        })
    }
}

impl Provider for ClaudeVertexProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.descriptor.id.clone(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
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

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(invalid(
                "Claude on Vertex dispatches only through its exact Messages adapter",
            ))
        }))
    }
}

impl InferenceAdapter for ClaudeVertexProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, ResolveError> {
        self.adapter.reasoning_effort_options(model)
    }

    fn resolve(
        &self,
        mut draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        // Sonnet 5 rejects non-default sampling even with thinking disabled.
        // Canonicalize the accepted default to omission so the durable resolved
        // call and Google Cloud wire agree exactly.
        if draft
            .temperature
            .is_some_and(|temperature| temperature != 1.0)
        {
            return Err(ResolveError::InvalidRequest {
                field: "temperature",
                message: "Claude Sonnet 5 accepts only the default sampling value".to_owned(),
            });
        }
        draft.temperature = None;
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

fn reasoning_effort(value: &str) -> Result<ReasoningEffortId, LlmError> {
    ReasoningEffortId::new(value)
        .map_err(|_| invalid("Claude on Vertex reasoning effort id is invalid"))
}

#[derive(Clone)]
struct AvailableOption {
    logical: &'static str,
    implementation: &'static str,
    option: ProviderRequestOption,
    supported_models: BTreeSet<String>,
}

/// Strict Gemini provider wrapper whose options are derived per selected
/// model and exact N01 route.
pub struct GoogleGeminiProvider {
    provider: ProviderDescriptor,
    config: GeminiConfig,
    adapter: GeminiAdapter,
    http: HttpService,
    default_model: String,
    credential_reference: String,
    available: Vec<AvailableOption>,
    cache: Option<GeminiCacheRequest>,
    model_evidence: Option<BTreeMap<String, ModelDescriptor>>,
}

impl GoogleGeminiProvider {
    /// Build the Gemini Developer API route with operation-time credentials.
    ///
    /// # Errors
    /// Invalid shared adapter configuration fails before publication.
    pub fn developer(
        http: HttpService,
        credential: RouteCredential,
        credential_reference: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let provider = crate::catalog::provider_descriptor();
        let config =
            GeminiConfig::with_credential(provider.clone(), GOOGLE_GEMINI_BASE_URL, credential);
        Self::build(http, provider, config, credential_reference, default_model)
    }

    /// Build one Vertex Gemini route whose base ends immediately before
    /// `/models/{model}:streamGenerateContent`.
    ///
    /// # Errors
    /// Invalid provider/base/credential configuration fails before publication.
    pub fn vertex(
        http: HttpService,
        base_url: impl Into<String>,
        credential: RouteCredential,
        credential_reference: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let provider = crate::maintained_catalog::vertex_gemini_provider_descriptor();
        let config = GeminiConfig::with_bearer_credential(provider.clone(), base_url, credential);
        Self::build(http, provider, config, credential_reference, default_model)
    }

    fn build(
        http: HttpService,
        provider: ProviderDescriptor,
        config: GeminiConfig,
        credential_reference: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let adapter = GeminiAdapter::new(config.clone(), http.clone())?;
        Ok(Self {
            provider,
            config,
            adapter,
            http,
            default_model: default_model.into(),
            credential_reference: credential_reference.into(),
            available: Vec::new(),
            cache: None,
            model_evidence: None,
        })
    }

    /// Register Google Search for an explicit maintained model set.
    ///
    /// # Errors
    /// Invalid request/plan/model evidence fails before publication.
    pub fn with_google_search(
        self,
        request: GoogleSearchRequest,
        supported_models: Vec<String>,
    ) -> Result<Self, LlmError> {
        let option = request
            .provider_option()
            .map_err(|_| invalid("Google Search option is invalid"))?;
        let option = provider_bound_option(&self.provider.id, option)?;
        let route = NativeToolRoute::new(
            GOOGLE_WEB_SEARCH_LOGICAL,
            GOOGLE_SEARCH_IMPLEMENTATION,
            NativeToolImplementationKind::Provider,
            Some(self.provider.id.clone()),
        )
        .map_err(|_| invalid("Google Search route is invalid"))?;
        let plan = GeminiProviderOptionPlan::new(
            option.clone(),
            GeminiProviderOptionWire::ToolMember {
                member: "tool".to_owned(),
            },
        )
        .map_err(|_| invalid("Google Search plan is invalid"))?
        .with_required_route(route)
        .with_required_native_feature(NativeFeature::Web)
        .with_normalizer(Arc::new(GroundingFactory::search(request)));
        self.install_plan(
            GOOGLE_WEB_SEARCH_LOGICAL,
            GOOGLE_SEARCH_IMPLEMENTATION,
            option,
            supported_models,
            plan,
        )
    }

    /// Register Vertex external grounding for an explicit model set.
    ///
    /// # Errors
    /// Only the `vertex-google` route accepts this extension.
    pub fn with_external_grounding(
        self,
        request: ExternalGroundingRequest,
        supported_models: Vec<String>,
    ) -> Result<Self, LlmError> {
        if self.provider.id != GOOGLE_VERTEX_PROVIDER {
            return Err(invalid(
                "external grounding requires the Vertex Gemini provider",
            ));
        }
        let option = request
            .provider_option()
            .map_err(|_| invalid("external grounding option is invalid"))?;
        let route = NativeToolRoute::new(
            GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
            GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
            NativeToolImplementationKind::Provider,
            Some(self.provider.id.clone()),
        )
        .map_err(|_| invalid("external grounding route is invalid"))?;
        let plan = GeminiProviderOptionPlan::new(
            option.clone(),
            GeminiProviderOptionWire::ToolMember {
                member: "tool".to_owned(),
            },
        )
        .map_err(|_| invalid("external grounding plan is invalid"))?
        .with_required_route(route)
        .with_normalizer(Arc::new(GroundingFactory::external(request)));
        self.install_plan(
            GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
            GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION,
            option,
            supported_models,
            plan,
        )
    }

    /// Register server-side Python code execution for an explicit model set.
    ///
    /// # Errors
    /// Invalid request/route/plan evidence fails before publication.
    pub fn with_code_execution(
        self,
        request: CodeExecutionRequest,
        supported_models: Vec<String>,
    ) -> Result<Self, LlmError> {
        let option = request
            .provider_option()
            .map_err(|_| invalid("code execution option is invalid"))?;
        let option = provider_bound_option(&self.provider.id, option)?;
        let route = NativeToolRoute::new(
            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
            GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
            NativeToolImplementationKind::Provider,
            Some(self.provider.id.clone()),
        )
        .map_err(|_| invalid("code execution route is invalid"))?;
        let plan = GeminiProviderOptionPlan::new(
            option.clone(),
            GeminiProviderOptionWire::ToolMember {
                member: "tool".to_owned(),
            },
        )
        .map_err(|_| invalid("code execution plan is invalid"))?
        .with_required_route(route)
        .with_normalizer(Arc::new(CodeFactory { request }));
        self.install_plan(
            GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
            GOOGLE_CODE_EXECUTION_IMPLEMENTATION,
            option,
            supported_models,
            plan,
        )
    }

    /// Configure implicit or explicit context-cache usage normalization.
    ///
    /// # Errors
    /// Invalid exact option/plan data fails before publication.
    pub fn with_cache(mut self, request: GeminiCacheRequest) -> Result<Self, LlmError> {
        if !request.supports_provider(&self.provider.id) {
            return Err(invalid(
                "Gemini cache resource belongs to another Google product",
            ));
        }
        let factory = Arc::new(CacheFactory {
            request: request.clone(),
        });
        match request
            .provider_option()
            .map_err(|_| invalid("Gemini cache option is invalid"))?
        {
            Some(option) => {
                let option = provider_bound_option(&self.provider.id, option)?;
                let plan = GeminiProviderOptionPlan::new(
                    option,
                    GeminiProviderOptionWire::TopLevelMembers {
                        members: vec![("cachedContent".to_owned(), "cachedContent".to_owned())],
                    },
                )
                .map_err(|_| invalid("Gemini cache plan is invalid"))?
                .with_required_native_feature(NativeFeature::PromptCache)
                .with_normalizer(factory);
                self.config = self.config.clone().with_provider_option_plan(plan);
            }
            None => {
                self.config = self
                    .config
                    .clone()
                    .with_feature_normalizer(NativeFeature::PromptCache, factory);
            }
        }
        self.cache = Some(request);
        self.rebuild()
    }

    fn install_plan(
        mut self,
        logical: &'static str,
        implementation: &'static str,
        option: ProviderRequestOption,
        supported_models: Vec<String>,
        plan: GeminiProviderOptionPlan,
    ) -> Result<Self, LlmError> {
        let models = validate_model_set(supported_models)?;
        if self
            .available
            .iter()
            .any(|available| available.logical == logical)
        {
            return Err(invalid("Gemini provider extension was configured twice"));
        }
        self.available.push(AvailableOption {
            logical,
            implementation,
            option,
            supported_models: models,
        });
        self.config = self.config.clone().with_provider_option_plan(plan);
        self.rebuild()
    }

    fn rebuild(mut self) -> Result<Self, LlmError> {
        self.adapter = GeminiAdapter::new(self.config.clone(), self.http.clone())?;
        Ok(self)
    }

    pub(crate) fn with_model_evidence(
        mut self,
        evidence: BTreeMap<String, ModelDescriptor>,
    ) -> Self {
        self.model_evidence = Some(evidence);
        self
    }

    fn expected_options(
        &self,
        context: ProviderOptionContext<'_>,
    ) -> Result<Vec<ProviderRequestOption>, ResolveError> {
        let mut options = Vec::new();
        for route in context.native_tool_routes().iter().filter(|route| {
            route.kind() == NativeToolImplementationKind::Provider
                && route.provider() == Some(self.provider.id.as_str())
        }) {
            let available = self
                .available
                .iter()
                .find(|available| {
                    available.logical == route.logical()
                        && available.implementation == route.implementation()
                })
                .ok_or_else(|| ResolveError::InvalidRequest {
                    field: "native_tool_routes",
                    message: "selected Gemini provider route is not configured".to_owned(),
                })?;
            if !available.supported_models.contains(&context.model().id) {
                return Err(ResolveError::Unproven {
                    provider: self.provider.id.clone(),
                    model: context.model().id.clone(),
                    capability: if available.logical == GOOGLE_WEB_SEARCH_LOGICAL {
                        RequestedCapability::NativeWeb
                    } else {
                        RequestedCapability::Tools
                    },
                });
            }
            options.push(available.option.clone());
        }
        if let Some(cache) = &self.cache {
            match context.model().capabilities.prompt_cache {
                CapabilitySupport::Supported => {}
                CapabilitySupport::Unsupported => {
                    return Err(ResolveError::Unsupported {
                        provider: self.provider.id.clone(),
                        model: context.model().id.clone(),
                        capability: RequestedCapability::PromptCache,
                    });
                }
                CapabilitySupport::Unknown => {
                    return Err(ResolveError::Unproven {
                        provider: self.provider.id.clone(),
                        model: context.model().id.clone(),
                        capability: RequestedCapability::PromptCache,
                    });
                }
            }
            if let Some(option) =
                cache
                    .provider_option()
                    .map_err(|_| ResolveError::InvalidRequest {
                        field: "provider_options",
                        message: "Gemini cache option is invalid".to_owned(),
                    })?
            {
                options.push(
                    provider_bound_option(&self.provider.id, option).map_err(|_| {
                        ResolveError::InvalidRequest {
                            field: "provider_options",
                            message: "Gemini cache option could not be route-bound".to_owned(),
                        }
                    })?,
                );
            }
        }
        options.sort_by(|left, right| left.kind().cmp(right.kind()));
        Ok(options)
    }
}

impl Provider for GoogleGeminiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.provider.id.clone(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.credential_reference)
    }

    fn descriptor(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    fn describe_model(&self, model: &str) -> ModelDescriptor {
        self.model_evidence
            .as_ref()
            .and_then(|evidence| evidence.get(model))
            .cloned()
            .unwrap_or_else(|| ModelDescriptor::unknown(model))
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    fn request_options_for(
        &self,
        context: ProviderOptionContext<'_>,
    ) -> Result<Vec<ProviderRequestOption>, ResolveError> {
        self.expected_options(context)
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(invalid(
                "Gemini provider dispatches only through its exact inference adapter",
            ))
        }))
    }
}

impl InferenceAdapter for GoogleGeminiProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, ResolveError> {
        self.adapter.reasoning_effort_options(model)
    }

    fn resolve(
        &self,
        mut draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        if self
            .model_evidence
            .as_ref()
            .is_some_and(|evidence| !evidence.contains_key(&model.id))
        {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "selected Gemini model is outside exact provider evidence".to_owned(),
            });
        }
        let expected =
            self.expected_options(ProviderOptionContext::new(model, &draft.native_tool_routes))?;
        if draft.provider_options != expected {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Gemini provider options differ from selected model/routes".to_owned(),
            });
        }
        if self.cache.is_some() && !draft.native_features.contains(&NativeFeature::PromptCache) {
            draft.native_features.push(NativeFeature::PromptCache);
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

fn provider_bound_option(
    provider: &str,
    option: ProviderRequestOption,
) -> Result<ProviderRequestOption, LlmError> {
    if option.provider() == provider {
        Ok(option)
    } else {
        ProviderRequestOption::new(provider, option.kind(), option.data().clone())
            .map_err(|_| invalid("Gemini provider option could not be route-bound"))
    }
}

fn validate_model_set(models: Vec<String>) -> Result<BTreeSet<String>, LlmError> {
    let count = models.len();
    let models = models.into_iter().collect::<BTreeSet<_>>();
    if models.is_empty()
        || models.len() != count
        || models.iter().any(|model| {
            model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
                || model.trim() != model
        })
    {
        return Err(invalid("Gemini feature model evidence is invalid"));
    }
    Ok(models)
}

enum GroundingFactoryRequest {
    Search(GoogleSearchRequest),
    External(ExternalGroundingRequest),
}

struct GroundingFactory {
    request: GroundingFactoryRequest,
}

impl GroundingFactory {
    fn search(request: GoogleSearchRequest) -> Self {
        Self {
            request: GroundingFactoryRequest::Search(request),
        }
    }

    fn external(request: ExternalGroundingRequest) -> Self {
        Self {
            request: GroundingFactoryRequest::External(request),
        }
    }
}

impl GeminiStreamNormalizerFactory for GroundingFactory {
    fn id(&self) -> &'static str {
        match self.request {
            GroundingFactoryRequest::Search(_) => "google-search-grounding",
            GroundingFactoryRequest::External(_) => "google-external-grounding",
        }
    }

    fn start(&self) -> Box<dyn GeminiStreamNormalizer> {
        let projector = match &self.request {
            GroundingFactoryRequest::Search(request) => {
                GroundingProjector::new(Some(request.clone()))
            }
            GroundingFactoryRequest::External(request) => {
                GroundingProjector::external(request.clone())
            }
        };
        Box::new(GroundingNormalizer { projector })
    }
}

struct GroundingNormalizer {
    projector: GroundingProjector,
}

impl GeminiStreamNormalizer for GroundingNormalizer {
    fn observe_candidate(
        &mut self,
        _response_id: &str,
        candidate: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        self.projector
            .observe(candidate)
            .map(|()| Vec::new())
            .map_err(|_| GeminiExtensionFault::InvalidResponse)
    }

    fn finish(
        &mut self,
        response_id: &str,
        next_output_index: u32,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        self.projector
            .project(next_output_index, response_id)
            .map(|projection| projection.into_events())
            .map_err(|_| GeminiExtensionFault::InvalidResponse)
    }
}

struct CodeFactory {
    request: CodeExecutionRequest,
}

impl GeminiStreamNormalizerFactory for CodeFactory {
    fn id(&self) -> &'static str {
        "google-code-execution"
    }

    fn start(&self) -> Box<dyn GeminiStreamNormalizer> {
        Box::new(CodeNormalizer {
            request: self.request,
            projector: None,
        })
    }
}

struct CodeNormalizer {
    request: CodeExecutionRequest,
    projector: Option<CodeExecutionProjector>,
}

impl GeminiStreamNormalizer for CodeNormalizer {
    fn accepts_part(&self, field: &str) -> bool {
        matches!(field, "executableCode" | "codeExecutionResult")
    }

    fn observe_part(
        &mut self,
        response_id: &str,
        output_index: u32,
        part: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        if self.projector.is_none() {
            self.projector = Some(
                CodeExecutionProjector::new(Some(self.request), response_id)
                    .map_err(|_| GeminiExtensionFault::InvalidResponse)?,
            );
        }
        self.projector
            .as_mut()
            .ok_or(GeminiExtensionFault::InvalidResponse)?
            .observe_part(output_index, part)
            .map_err(|_| GeminiExtensionFault::InvalidResponse)
    }

    fn finish(
        &mut self,
        _response_id: &str,
        _next_output_index: u32,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        if let Some(projector) = &self.projector {
            projector
                .finish()
                .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
        }
        Ok(Vec::new())
    }
}

struct CacheFactory {
    request: GeminiCacheRequest,
}

impl GeminiStreamNormalizerFactory for CacheFactory {
    fn id(&self) -> &'static str {
        match self.request.mode() {
            GeminiCacheMode::Implicit => "google-cache-implicit",
            GeminiCacheMode::Explicit => "google-cache-explicit",
        }
    }

    fn start(&self) -> Box<dyn GeminiStreamNormalizer> {
        Box::new(CacheNormalizer {
            request: self.request.clone(),
            prompt_tokens: None,
            cached_tokens: None,
            candidates_tokens: None,
            thoughts_tokens: None,
        })
    }
}

struct CacheNormalizer {
    request: GeminiCacheRequest,
    prompt_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    candidates_tokens: Option<u64>,
    thoughts_tokens: Option<u64>,
}

impl GeminiStreamNormalizer for CacheNormalizer {
    fn observe_usage(
        &mut self,
        usage: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        let cache = GeminiCacheUsage::parse(&self.request, usage)
            .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
        update_cache_counter(&mut self.prompt_tokens, cache.prompt_tokens())?;
        update_cache_counter(&mut self.cached_tokens, cache.cached_tokens())?;
        let usage = usage
            .as_object()
            .ok_or(GeminiExtensionFault::InvalidResponse)?;
        update_cache_counter(
            &mut self.candidates_tokens,
            cache_usage_counter(usage, "candidatesTokenCount")?,
        )?;
        update_cache_counter(
            &mut self.thoughts_tokens,
            cache_usage_counter(usage, "thoughtsTokenCount")?,
        )?;
        Ok(Vec::new())
    }

    fn finish(
        &mut self,
        _response_id: &str,
        _next_output_index: u32,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        // Explicit cachedContent use is read-only during generation. Implicit
        // caching can create provider-managed entries, so its absent write
        // counter must not be converted to zero.
        if self.request.mode() != GeminiCacheMode::Explicit {
            return Ok(Vec::new());
        }
        let (Some(prompt), Some(cached), Some(candidates)) = (
            self.prompt_tokens,
            self.cached_tokens,
            self.candidates_tokens,
        ) else {
            return Ok(Vec::new());
        };
        let output = candidates
            .checked_add(self.thoughts_tokens.unwrap_or(0))
            .ok_or(GeminiExtensionFault::InvalidResponse)?;
        let uncached = prompt
            .checked_sub(cached)
            .ok_or(GeminiExtensionFault::InvalidResponse)?;
        let cache = heycode_core::ProviderCacheUsage::new(prompt, output, cached, 0)
            .and_then(|cache| cache.with_uncached_input_tokens(uncached))
            .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
        let metadata = heycode_core::ProviderResponseMetadata::new(Some(cache), Vec::new(), None)
            .map_err(|_| GeminiExtensionFault::InvalidResponse)?;
        Ok(vec![InferenceEvent::ResponseMetadata(metadata)])
    }
}

fn cache_usage_counter(
    usage: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>, GeminiExtensionFault> {
    match usage.get(field).filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(GeminiExtensionFault::InvalidResponse),
    }
}

fn update_cache_counter(
    current: &mut Option<u64>,
    incoming: Option<u64>,
) -> Result<(), GeminiExtensionFault> {
    let Some(incoming) = incoming else {
        return Ok(());
    };
    if current.is_some_and(|current| incoming < current) {
        return Err(GeminiExtensionFault::InvalidResponse);
    }
    *current = Some(incoming);
    Ok(())
}

fn invalid(message: &str) -> LlmError {
    LlmError::InvalidResponse(message.to_owned())
}
