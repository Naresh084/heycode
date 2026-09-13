//! Gemini `generateContent` protocol adapter over the shared raw HTTP/SSE
//! service.
//!
//! Every wire fact encoded here cites the Google documentation it came from.
//! Anything the current documentation does not state is modelled as unknown and
//! refused during [`InferenceAdapter::resolve`] rather than guessed.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures::StreamExt as _;

use crate::{
    AuthenticationBinding, ChatMessage, FinishReason, InferenceAdapter, InferenceEvent,
    InferenceInput, InferenceStream, InferenceTarget, LlmError, ModelDescriptor, NativeFeature,
    ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId,
    ReasoningEffortOptions, RequestDraft, ResolveError, ResolveSpec, ResolvedCall, Role,
    StreamItemKind, TokenUsage, ToolSpec, resolve_request,
};

/// Streaming method plus the server-sent-events alternative representation.
///
/// <https://ai.google.dev/api/generate-content> — `models.streamGenerateContent`
/// is documented as
/// `POST https://generativelanguage.googleapis.com/v1beta/{model=models/*}:streamGenerateContent`
/// and its curl sample appends `?alt=sse` to receive server-sent events.
const STREAM_METHOD: &str = ":streamGenerateContent?alt=sse";

/// Documented API-key header.
///
/// <https://ai.google.dev/gemini-api/docs/api-key> — REST requests send the key
/// as `-H "x-goog-api-key: YOUR_API_KEY"`.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Mutually exclusive `Part` content fields.
///
/// <https://ai.google.dev/api/generate-content> — a `Part` sets exactly one of
/// these; `thought` and `thoughtSignature` are modifiers of the chosen one.
const PART_CONTENT_FIELDS: [&str; 7] = [
    "text",
    "inlineData",
    "functionCall",
    "functionResponse",
    "fileData",
    "executableCode",
    "codeExecutionResult",
];

/// Image MIME types every current Google source agrees on.
///
/// The sources disagree, so this is deliberately their intersection:
/// - <https://ai.google.dev/gemini-api/docs/image-understanding> — "Gemini
///   supports the following image format MIME types: PNG, JPEG, WEBP, HEIC,
///   HEIF" (no GIF).
/// - <https://ai.google.dev/gemini-api/docs/file-input-methods> — image types
///   `image/bmp`, `image/jpeg`, `image/png`, `image/webp`, with the caveat that
///   the list "is intended as initial guidance and is not comprehensive ...
///   Unsupported types will result in an error."
/// - the discovery document's `Blob.mimeType` adds `image/jpg`, `image/gif` and
///   `image/avif`, but as *examples* spanning every media kind rather than an
///   image-specific claim.
///
/// `ChatImage` also admits `image/gif` because other providers document GIF
/// support. Only the weakest of the three sources mentions it for Gemini, so a
/// GIF is refused before transport instead of being sent on that evidence.
const SUPPORTED_IMAGE_MIME: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// Largest inline request this route will send.
///
/// <https://ai.google.dev/gemini-api/docs/image-understanding> — "Inline image
/// data limits your total request size (text prompts, system instructions, and
/// inline bytes) to 20MB." The file-input guide states 100 MB for inline data
/// generally; the smaller image-specific bound is the one enforced, because
/// refusing early is something the caller can act on while the alternative is a
/// provider error mid-turn. "20MB" is read as decimal, the smaller reading.
const MAX_INLINE_REQUEST_BYTES: usize = 20_000_000;

/// Authentication header dialect for a `generateContent`-compatible route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthWire {
    /// Native `x-goog-api-key: <secret>` authentication.
    ApiKeyHeader,
    /// OAuth/gateway `Authorization: Bearer <secret>` authentication.
    Bearer,
}

/// Exact `generationConfig.thinkingConfig` request chosen for one canonical
/// reasoning id.
///
/// <https://ai.google.dev/gemini-api/docs/generate-content/thinking> documents
/// both dialects: `thinkingLevel` for the Gemini 3 family and `thinkingBudget`
/// for the Gemini 2.5 family, alongside `includeThoughts`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThinkingMode {
    /// `thinkingConfig.thinkingLevel`, whose accepted values are model-owned.
    Level {
        /// Exact provider level token, for example `low`, `medium` or `high`.
        level: String,
        /// Request `thinkingConfig.includeThoughts`.
        include_thoughts: bool,
    },
    /// `thinkingConfig.thinkingBudget`, where `0` disables thinking and `-1`
    /// asks the model to size its own budget.
    Budget {
        /// Exact provider budget value.
        budget_tokens: i64,
        /// Request `thinkingConfig.includeThoughts`.
        include_thoughts: bool,
    },
}

impl ThinkingMode {
    fn request_value(&self) -> serde_json::Value {
        match self {
            Self::Level {
                level,
                include_thoughts,
            } => serde_json::json!({
                "thinkingLevel":level,
                "includeThoughts":include_thoughts,
            }),
            Self::Budget {
                budget_tokens,
                include_thoughts,
            } => serde_json::json!({
                "thinkingBudget":budget_tokens,
                "includeThoughts":include_thoughts,
            }),
        }
    }
}

/// Whether this exact route's models require a returned `thoughtSignature` to
/// be echoed verbatim on the next turn.
///
/// <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
/// — Gemini 3 attaches a signature to the first `functionCall` part and
/// *requires* it back (a missing signature is a 400); Gemini 2.5 places a
/// signature in the first part regardless of type and makes returning it
/// optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureRule {
    /// Signatures must be echoed. The first `functionCall` part of a response
    /// must carry one, and a continuation replays the model turn as
    /// [`ProviderStateKind::GeminiModelContent`] so every signature returns in
    /// the exact part that carried it. A continuation rebuilt from the neutral
    /// vocabulary is still refused during `resolve`: that shape has no slot for
    /// a signature, so sending it would silently drop one.
    Mandatory,
    /// Signatures are accepted but not required by the route's models. A
    /// continuation assembled from the neutral vocabulary is permitted and
    /// carries no signature, which the documentation describes as a reasoning
    /// quality loss rather than an error.
    Optional,
}

/// Closed provider-extension failure. Provider response/configuration bodies
/// cannot cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GeminiExtensionFault {
    /// The configured request option/wire projection is invalid.
    #[error("invalid Gemini provider extension configuration")]
    InvalidConfiguration,
    /// One provider response fragment does not satisfy its exact extension.
    #[error("invalid Gemini provider extension response")]
    InvalidResponse,
}

/// Per-response provider-owned normalizer invoked by the shared Gemini parser.
pub trait GeminiStreamNormalizer: Send {
    /// Whether this extension owns a response `Part` content discriminator.
    fn accepts_part(&self, _field: &str) -> bool {
        false
    }

    /// Observe one complete streamed candidate after shared structural checks.
    ///
    /// # Errors
    /// Provider-specific malformed or unsolicited data returns a closed fault.
    fn observe_candidate(
        &mut self,
        _response_id: &str,
        _candidate: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        Ok(Vec::new())
    }

    /// Observe one exact candidate part and its shared output index.
    ///
    /// # Errors
    /// Provider-specific malformed or uncorrelated data returns a closed fault.
    fn observe_part(
        &mut self,
        _response_id: &str,
        _output_index: u32,
        _part: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        Ok(Vec::new())
    }

    /// Observe one cumulative `usageMetadata` object.
    ///
    /// # Errors
    /// Provider-specific malformed cache/usage data returns a closed fault.
    fn observe_usage(
        &mut self,
        _usage: &serde_json::Value,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        Ok(Vec::new())
    }

    /// Settle the extension before provider state/Usage/Finish publication.
    ///
    /// # Errors
    /// Unsettled provider calls or inconsistent accumulated evidence fail.
    fn finish(
        &mut self,
        _response_id: &str,
        _next_output_index: u32,
    ) -> Result<Vec<InferenceEvent>, GeminiExtensionFault> {
        Ok(Vec::new())
    }
}

/// Factory for one fresh provider normalizer per transport attempt.
pub trait GeminiStreamNormalizerFactory: Send + Sync {
    /// Stable safe identity used to reject duplicate normalizers.
    fn id(&self) -> &'static str;

    /// Create isolated state for one streamed response.
    fn start(&self) -> Box<dyn GeminiStreamNormalizer>;
}

/// How one exact provider option projects into a GenerateContent request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiProviderOptionWire {
    /// Append the object-valued option member to top-level `tools[]`.
    ToolMember {
        /// Exact member containing one tool object.
        member: String,
    },
    /// Copy every exact option member to its declared top-level field.
    TopLevelMembers {
        /// `(option member, request field)` mappings.
        members: Vec<(String, String)>,
    },
}

/// One exact provider-option-gated Gemini request/normalization plan.
#[derive(Clone)]
pub struct GeminiProviderOptionPlan {
    option: heycode_core::ProviderRequestOption,
    wire: GeminiProviderOptionWire,
    required_route: Option<heycode_core::NativeToolRoute>,
    required_native_feature: Option<NativeFeature>,
    normalizer: Option<Arc<dyn GeminiStreamNormalizerFactory>>,
}

impl GeminiProviderOptionPlan {
    /// Bind one exact durable option and its all-or-nothing wire projection.
    ///
    /// # Errors
    /// Invalid option/member mappings fail before adapter publication.
    pub fn new(
        option: heycode_core::ProviderRequestOption,
        wire: GeminiProviderOptionWire,
    ) -> Result<Self, GeminiExtensionFault> {
        let plan = Self {
            option,
            wire,
            required_route: None,
            required_native_feature: None,
            normalizer: None,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Require the exact N01 route that authorizes this option.
    #[must_use]
    pub fn with_required_route(mut self, route: heycode_core::NativeToolRoute) -> Self {
        self.required_route = Some(route);
        self
    }

    /// Require one existing durable native feature.
    #[must_use]
    pub fn with_required_native_feature(mut self, feature: NativeFeature) -> Self {
        self.required_native_feature = Some(feature);
        self
    }

    /// Attach provider-owned response normalization for this exact plan.
    #[must_use]
    pub fn with_normalizer(mut self, normalizer: Arc<dyn GeminiStreamNormalizerFactory>) -> Self {
        self.normalizer = Some(normalizer);
        self
    }

    /// Exact provider option this plan accepts.
    #[must_use]
    pub const fn option(&self) -> &heycode_core::ProviderRequestOption {
        &self.option
    }

    fn validate(&self) -> Result<(), GeminiExtensionFault> {
        self.option
            .validate()
            .map_err(|_| GeminiExtensionFault::InvalidConfiguration)?;
        let data = self
            .option
            .data()
            .as_object()
            .ok_or(GeminiExtensionFault::InvalidConfiguration)?;
        match &self.wire {
            GeminiProviderOptionWire::ToolMember { member } => {
                if data.len() != 1 || data.get(member).is_none_or(|value| !value.is_object()) {
                    return Err(GeminiExtensionFault::InvalidConfiguration);
                }
            }
            GeminiProviderOptionWire::TopLevelMembers { members } => {
                const RESERVED: &[&str] = &[
                    "contents",
                    "systemInstruction",
                    "tools",
                    "toolConfig",
                    "generationConfig",
                ];
                let mut option_members = BTreeSet::new();
                let mut fields = BTreeSet::new();
                if members.is_empty()
                    || members.iter().any(|(member, field)| {
                        member.is_empty()
                            || field.is_empty()
                            || RESERVED.contains(&field.as_str())
                            || !option_members.insert(member.as_str())
                            || !fields.insert(field.as_str())
                            || !data.contains_key(member)
                    })
                    || data.len() != members.len()
                {
                    return Err(GeminiExtensionFault::InvalidConfiguration);
                }
            }
        }
        if self
            .required_route
            .as_ref()
            .is_some_and(|route| route.validate().is_err())
            || self.normalizer.as_ref().is_some_and(|normalizer| {
                let id = normalizer.id();
                id.is_empty()
                    || id.len() > 128
                    || id.bytes().any(|byte| {
                        !(byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'-' | b'_' | b'.'))
                    })
            })
        {
            return Err(GeminiExtensionFault::InvalidConfiguration);
        }
        Ok(())
    }
}

impl std::fmt::Debug for GeminiProviderOptionPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeminiProviderOptionPlan")
            .field("kind", &self.option.kind())
            .field("wire", &self.wire)
            .field("required_route", &self.required_route)
            .field("required_native_feature", &self.required_native_feature)
            .field(
                "normalizer",
                &self.normalizer.as_ref().map(|normalizer| normalizer.id()),
            )
            .finish()
    }
}

#[derive(Clone)]
struct GeminiFeatureNormalizerPlan {
    feature: NativeFeature,
    normalizer: Arc<dyn GeminiStreamNormalizerFactory>,
}

/// Reusable Gemini `generateContent` route configuration. Debug output is
/// redacted.
#[derive(Clone)]
pub struct GeminiConfig {
    provider: ProviderDescriptor,
    base_url: String,
    credential: crate::RouteCredential,
    auth_wire: AuthWire,
    extra_headers: Vec<(String, String)>,
    thinking: Vec<(ReasoningEffortId, ThinkingMode)>,
    default_reasoning_effort: Option<ReasoningEffortId>,
    default_max_output_tokens: Option<u64>,
    thought_signatures: SignatureRule,
    provider_option_plans: Vec<GeminiProviderOptionPlan>,
    feature_normalizers: Vec<GeminiFeatureNormalizerPlan>,
    retry_spec: crate::RetrySpec,
}

impl GeminiConfig {
    /// Build one route authenticated with the documented `x-goog-api-key`
    /// header. Thought signatures start mandatory: an unproven route gets the
    /// strict Gemini 3 rule rather than the permissive one.
    #[must_use]
    pub fn with_key(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::new(
            provider,
            base_url,
            crate::RouteCredential::fixed(api_key),
            AuthWire::ApiKeyHeader,
        )
    }

    /// Build one `x-goog-api-key` route whose credential is resolved once per
    /// operation.
    #[must_use]
    pub fn with_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self::new(provider, base_url, credential, AuthWire::ApiKeyHeader)
    }

    /// Build one bearer route whose credential is resolved once per operation.
    #[must_use]
    pub fn with_bearer_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self::new(provider, base_url, credential, AuthWire::Bearer)
    }

    /// Build one route authenticated with `Authorization: Bearer <token>`, as
    /// an OAuth-fronted or gateway deployment requires.
    #[must_use]
    pub fn with_bearer_token(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self::new(
            provider,
            base_url,
            crate::RouteCredential::fixed(token),
            AuthWire::Bearer,
        )
    }

    fn new(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
        auth_wire: AuthWire,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential,
            auth_wire,
            extra_headers: Vec::new(),
            thinking: Vec::new(),
            default_reasoning_effort: None,
            default_max_output_tokens: None,
            thought_signatures: SignatureRule::Mandatory,
            provider_option_plans: Vec::new(),
            feature_normalizers: Vec::new(),
            retry_spec: crate::RetrySpec::standard(),
        }
    }

    /// Attach validated-at-construction attribution or gateway headers.
    #[must_use]
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Map one canonical reasoning id onto `thinkingConfig.thinkingLevel`, the
    /// Gemini 3 dialect. Accepted level tokens are model-owned, so the exact
    /// value is route data.
    #[must_use]
    pub fn with_thinking_level(
        mut self,
        effort: ReasoningEffortId,
        level: impl Into<String>,
        include_thoughts: bool,
    ) -> Self {
        self.thinking.push((
            effort,
            ThinkingMode::Level {
                level: level.into(),
                include_thoughts,
            },
        ));
        self
    }

    /// Map one canonical reasoning id onto `thinkingConfig.thinkingBudget`, the
    /// Gemini 2.5 dialect, where `0` disables thinking and `-1` lets the model
    /// size its own budget.
    #[must_use]
    pub fn with_thinking_budget(
        mut self,
        effort: ReasoningEffortId,
        budget_tokens: i64,
        include_thoughts: bool,
    ) -> Self {
        self.thinking.push((
            effort,
            ThinkingMode::Budget {
                budget_tokens,
                include_thoughts,
            },
        ));
        self
    }

    /// Attach the adapter-owned reasoning default, which must be one of the
    /// mapped canonical ids.
    #[must_use]
    pub fn with_default_reasoning_effort(mut self, effort: Option<ReasoningEffortId>) -> Self {
        self.default_reasoning_effort = effort;
        self
    }

    /// Attach an adapter-owned `generationConfig.maxOutputTokens` default.
    #[must_use]
    pub const fn with_default_max_output_tokens(mut self, value: Option<u64>) -> Self {
        self.default_max_output_tokens = value;
        self
    }

    /// Declare that this route's models accept a continuation with no
    /// `thoughtSignature`, which the documentation states for the Gemini 2.5
    /// family. Declare it only with that evidence: the strict default refuses
    /// function calling rather than emitting a continuation the model would
    /// reject or silently reason worse about.
    #[must_use]
    pub const fn with_optional_thought_signatures(mut self) -> Self {
        self.thought_signatures = SignatureRule::Optional;
        self
    }

    /// Attach an explicit validated retry policy.
    #[must_use]
    pub fn with_retry_spec(mut self, retry_spec: crate::RetrySpec) -> Self {
        self.retry_spec = retry_spec;
        self
    }

    /// Register one exact provider-option request/response extension.
    #[must_use]
    pub fn with_provider_option_plan(mut self, plan: GeminiProviderOptionPlan) -> Self {
        self.provider_option_plans.push(plan);
        self
    }

    /// Register provider-owned response normalization selected by an existing
    /// durable native feature but requiring no additional request field.
    #[must_use]
    pub fn with_feature_normalizer(
        mut self,
        feature: NativeFeature,
        normalizer: Arc<dyn GeminiStreamNormalizerFactory>,
    ) -> Self {
        self.feature_normalizers.push(GeminiFeatureNormalizerPlan {
            feature,
            normalizer,
        });
        self
    }

    /// Secret-free exact-route resolution proposal.
    #[must_use]
    pub fn resolve_spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::GeminiGenerateContent,
            target: InferenceTarget::Http {
                base_url: self.base_url.clone(),
            },
            authentication: self.credential.binding(),
            default_max_output_tokens: self.default_max_output_tokens,
            reasoning_efforts: self
                .thinking
                .iter()
                .map(|(effort, _)| effort.clone())
                .collect(),
            default_reasoning_effort: self.default_reasoning_effort.clone(),
        }
    }

    fn thinking_mode(&self, effort: &ReasoningEffortId) -> Option<&ThinkingMode> {
        self.thinking
            .iter()
            .find_map(|(candidate, mode)| (candidate == effort).then_some(mode))
    }
}

impl std::fmt::Debug for GeminiConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeminiConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("auth_wire", &self.auth_wire)
            .field("extra_header_count", &self.extra_headers.len())
            .field("thinking", &self.thinking)
            .field("default_reasoning_effort", &self.default_reasoning_effort)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("thought_signatures", &self.thought_signatures)
            .field(
                "provider_option_plan_count",
                &self.provider_option_plans.len(),
            )
            .field("feature_normalizer_count", &self.feature_normalizers.len())
            .field("retry_spec", &self.retry_spec)
            .finish()
    }
}

/// Reusable Gemini `generateContent` protocol adapter.
#[derive(Clone)]
pub struct GeminiAdapter {
    config: GeminiConfig,
    http: heycode_http::HttpService,
}

impl GeminiAdapter {
    /// Validate configuration and bind the shared HTTP service.
    ///
    /// # Errors
    /// Missing protocol declaration, invalid endpoint/key/header, or duplicate
    /// or malformed reasoning choices and output defaults.
    pub fn new(config: GeminiConfig, http: heycode_http::HttpService) -> Result<Self, LlmError> {
        validate_config(&config)?;
        Ok(Self { config, http })
    }
}

impl InferenceAdapter for GeminiAdapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.config.provider.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.config.resolve_spec().authentication
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<ReasoningEffortOptions>, ResolveError> {
        let spec = self.config.resolve_spec();
        ReasoningEffortOptions::for_model(
            model,
            spec.reasoning_efforts,
            spec.default_reasoning_effort,
        )
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        let selected = select_gemini_extensions(
            &self.config,
            &draft.provider_options,
            &draft.native_tool_routes,
            &draft.native_features,
        )?;
        validate_draft(&self.config, &draft)?;
        let retry_spec = if selected.provider_operation {
            self.config.retry_spec.clone().disable_replay()
        } else {
            self.config.retry_spec.clone()
        };
        let call = resolve_request(
            &self.config.provider,
            draft,
            model,
            &self.config.resolve_spec(),
        )?
        .with_retry_spec(retry_spec);
        // The resolved model id becomes one URL path segment, so it must not be
        // able to change the method, resource or query the adapter dispatches.
        if !is_bare_path_segment(call.model()) {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "Gemini model id must be one bare URL path segment".to_owned(),
            });
        }
        Ok(call)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.stream_cancellable(call, tokio_util::sync::CancellationToken::new())
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        let selected = match select_gemini_extensions(
            &self.config,
            call.provider_options(),
            call.native_tool_routes(),
            call.native_features(),
        ) {
            Ok(selected) => selected,
            Err(_) => return one_error(invalid("resolved Gemini provider extensions are invalid")),
        };
        let body = match generate_content_request_body(&call, &self.config, &selected) {
            Ok(body) => body,
            Err(error) => return one_error(error),
        };
        let url = format!(
            "{}/models/{}{STREAM_METHOD}",
            self.config.base_url.trim_end_matches('/'),
            call.model()
        );
        let body = body.to_string().into_bytes();
        // Measured on the exact serialized request, because the documented
        // limit is the whole payload — prompt, system instruction and inline
        // bytes together — not the image alone. Nothing earlier knows that
        // number without serializing twice, and this still precedes transport.
        if body.len() > MAX_INLINE_REQUEST_BYTES {
            return one_error(invalid(
                "Gemini inline request exceeds the documented 20 MB total request size",
            ));
        }
        let client_tool_names = call
            .tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<BTreeSet<_>>();
        let signature_rule = self.config.thought_signatures;
        let provider_id = self.config.provider.id.clone();
        let model_id = call.model().to_owned();
        let retry_spec = call.retry_spec().clone();
        let normalizer_factories = selected.normalizers.clone();
        let config = self.config.clone();
        let http = self.http.clone();
        // Resolved once, before the first attempt: every retry of this one
        // operation reuses it, and the next operation resolves again.
        let credential = match self.config.credential.acquire() {
            Ok(credential) => credential,
            Err(error) => return one_error(LlmError::UnresolvedCredential(error)),
        };
        crate::retry::retrying_stream(retry_spec, cancellation, move |attempt_cancellation| {
            let mut request = match heycode_http::HttpSseRequest::post(url.clone(), body.clone())
                .and_then(|request| request.header("content-type", "application/json"))
            {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            let auth_request = match config.auth_wire {
                AuthWire::ApiKeyHeader => request.header(API_KEY_HEADER, credential.expose()),
                AuthWire::Bearer => {
                    request.header("authorization", &format!("Bearer {}", credential.expose()))
                }
            };
            request = match auth_request {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            for (name, value) in &config.extra_headers {
                request = match request.header(name, value) {
                    Ok(request) => request,
                    Err(error) => return one_error(crate::classify_transport_error(error)),
                };
            }
            let events = http.sse(request, attempt_cancellation);
            Box::pin(
                futures::stream::unfold(
                    GeminiPhase::Read(
                        events,
                        Box::new(GeminiParser::new(
                            client_tool_names.clone(),
                            signature_rule,
                            provider_id.clone(),
                            model_id.clone(),
                            normalizer_factories
                                .iter()
                                .map(|factory| factory.start())
                                .collect(),
                        )),
                    ),
                    drive_gemini,
                )
                .flat_map(futures::stream::iter),
            )
        })
    }
}

fn validate_config(config: &GeminiConfig) -> Result<(), LlmError> {
    if !config
        .provider
        .protocols
        .contains(&ProviderProtocol::GeminiGenerateContent)
    {
        return Err(invalid(
            "Gemini adapter provider does not declare Gemini GenerateContent",
        ));
    }
    if config.credential.fixed_is_blank() {
        return Err(crate::retry::local_failure(
            crate::ProviderErrorClass::Authentication,
        ));
    }
    let url = format!(
        "{}/models/probe{STREAM_METHOD}",
        config.base_url.trim_end_matches('/')
    );
    let mut request = heycode_http::HttpSseRequest::post(url, Vec::new())
        .and_then(|request| request.header("content-type", "application/json"))
        .map_err(map_transport_error)?;
    let probe = config.credential.probe_value();
    request = match config.auth_wire {
        AuthWire::ApiKeyHeader => request.header(API_KEY_HEADER, probe),
        AuthWire::Bearer => request.header("authorization", &format!("Bearer {probe}")),
    }
    .map_err(map_transport_error)?;
    let mut header_names = BTreeSet::new();
    for (name, value) in &config.extra_headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "content-type" | "authorization" | API_KEY_HEADER
        ) || !header_names.insert(normalized)
        {
            return Err(invalid(
                "Gemini extra headers must be unique and cannot replace protocol/auth headers",
            ));
        }
        request = request.header(name, value).map_err(map_transport_error)?;
    }
    drop(request);

    let mut efforts = BTreeSet::new();
    for (effort, mode) in &config.thinking {
        if !efforts.insert(effort.as_str()) {
            return Err(invalid("Gemini adapter has duplicate reasoning effort ids"));
        }
        match mode {
            ThinkingMode::Level { level, .. } => {
                if level.is_empty() || level.trim() != level || level.len() > 128 {
                    return Err(invalid(
                        "Gemini thinking level must be 1..=128 bytes and trimmed",
                    ));
                }
            }
            // `-1` asks the model to size its own budget and `0` disables
            // thinking; nothing below `-1` is documented.
            // <https://ai.google.dev/gemini-api/docs/generate-content/thinking>
            ThinkingMode::Budget { budget_tokens, .. } => {
                if *budget_tokens < -1 {
                    return Err(invalid(
                        "Gemini thinking budget must be -1, 0 or a positive token count",
                    ));
                }
            }
        }
    }
    if config
        .default_reasoning_effort
        .as_ref()
        .is_some_and(|default| !efforts.contains(default.as_str()))
    {
        return Err(invalid(
            "Gemini reasoning default is not in its exact choice list",
        ));
    }
    if config.default_max_output_tokens == Some(0) {
        return Err(invalid("Gemini adapter output default must be positive"));
    }
    let mut option_kinds = BTreeSet::new();
    let mut normalizer_ids = BTreeSet::new();
    for plan in &config.provider_option_plans {
        plan.validate()
            .map_err(|_| invalid("Gemini provider option plan is invalid"))?;
        if plan.option.provider() != config.provider.id
            || !option_kinds.insert(plan.option.kind())
            || plan
                .normalizer
                .as_ref()
                .is_some_and(|normalizer| !normalizer_ids.insert(normalizer.id()))
        {
            return Err(invalid(
                "Gemini provider option plans must be uniquely provider-bound",
            ));
        }
    }
    let mut features = BTreeSet::new();
    for plan in &config.feature_normalizers {
        if !features.insert(plan.feature)
            || !normalizer_ids.insert(plan.normalizer.id())
            || plan.normalizer.id().is_empty()
        {
            return Err(invalid("Gemini feature normalizers must be unique"));
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct SelectedGeminiExtensions {
    tools: Vec<serde_json::Value>,
    top_level_fields: Vec<(String, serde_json::Value)>,
    normalizers: Vec<Arc<dyn GeminiStreamNormalizerFactory>>,
    provider_operation: bool,
}

fn select_gemini_extensions(
    config: &GeminiConfig,
    options: &[heycode_core::ProviderRequestOption],
    routes: &[heycode_core::NativeToolRoute],
    features: &[NativeFeature],
) -> Result<SelectedGeminiExtensions, ResolveError> {
    let mut selected = SelectedGeminiExtensions::default();
    let mut selected_routes = Vec::new();
    let mut normalizer_ids = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for option in options {
        let plan = config
            .provider_option_plans
            .iter()
            .find(|plan| plan.option.kind() == option.kind())
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Gemini received an undeclared provider option kind".to_owned(),
            })?;
        if plan.option != *option {
            return request_error(
                "provider_options",
                "Gemini provider option differs from its exact configured plan",
            );
        }
        if let Some(required) = &plan.required_route {
            if !routes.contains(required) {
                return request_error(
                    "native_tool_routes",
                    "Gemini provider option is missing its exact N01 route",
                );
            }
            if selected_routes.contains(required) {
                return request_error(
                    "native_tool_routes",
                    "Gemini selected one provider-native route twice",
                );
            }
            selected_routes.push(required.clone());
            selected.provider_operation = true;
        }
        if plan
            .required_native_feature
            .is_some_and(|feature| !features.contains(&feature))
        {
            return request_error(
                "native_features",
                "Gemini provider option is missing its required native feature",
            );
        }
        let data = option
            .data()
            .as_object()
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Gemini provider option data must be an object".to_owned(),
            })?;
        match &plan.wire {
            GeminiProviderOptionWire::ToolMember { member } => {
                selected.tools.push(data[member].clone());
            }
            GeminiProviderOptionWire::TopLevelMembers { members } => {
                for (member, field) in members {
                    if !fields.insert(field.clone()) {
                        return request_error(
                            "provider_options",
                            "Gemini provider options target one field twice",
                        );
                    }
                    selected
                        .top_level_fields
                        .push((field.clone(), data[member].clone()));
                }
            }
        }
        if let Some(normalizer) = &plan.normalizer {
            if !normalizer_ids.insert(normalizer.id()) {
                return request_error(
                    "provider_options",
                    "Gemini selected one response normalizer twice",
                );
            }
            selected.normalizers.push(normalizer.clone());
        }
    }
    for plan in &config.feature_normalizers {
        if features.contains(&plan.feature) {
            if !normalizer_ids.insert(plan.normalizer.id()) {
                return request_error(
                    "native_features",
                    "Gemini selected one response normalizer twice",
                );
            }
            selected.normalizers.push(plan.normalizer.clone());
        }
    }
    for feature in features {
        let configured = config
            .feature_normalizers
            .iter()
            .any(|plan| plan.feature == *feature)
            || config.provider_option_plans.iter().any(|plan| {
                plan.required_native_feature == Some(*feature) && options.contains(&plan.option)
            });
        if !configured {
            return request_error(
                "native_features",
                "Gemini native feature has no configured request/response dialect",
            );
        }
    }
    for route in routes.iter().filter(|route| {
        route.kind() == heycode_core::NativeToolImplementationKind::Provider
            && route.provider() == Some(config.provider.id.as_str())
    }) {
        if !selected_routes.contains(route) {
            return request_error(
                "native_tool_routes",
                "Gemini provider-native route has no selected provider option",
            );
        }
    }
    Ok(selected)
}

fn validate_draft(config: &GeminiConfig, draft: &RequestDraft) -> Result<(), ResolveError> {
    if draft.structured_output.is_some() {
        // The current structured-output documentation describes the newer
        // `response_format` request object rather than a `generateContent`
        // field, so this route encodes no schema dialect.
        // <https://ai.google.dev/gemini-api/docs/structured-output>
        return request_error(
            "structured_output",
            "Gemini structured-output dialect is not configured yet",
        );
    }
    for modality in &draft.input_modalities {
        match modality {
            crate::InputModality::Text | crate::InputModality::Image => {}
            // PDF input rides the same `inlineData` part, but its own size cap
            // and page semantics are a separate contract that nothing has
            // proven for this route yet.
            crate::InputModality::Document => {
                return request_error(
                    "input_modalities",
                    "Gemini document input dialect is not configured yet",
                );
            }
        }
    }
    for input in &draft.inputs {
        let InferenceInput::Message(message) = input else {
            continue;
        };
        for image in &message.images {
            if !SUPPORTED_IMAGE_MIME.contains(&image.media_type().as_str()) {
                return request_error(
                    "input_modalities",
                    "Gemini image input accepts only PNG, JPEG and WebP",
                );
            }
        }
    }
    let advertised = draft
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    for input in &draft.inputs {
        match input {
            InferenceInput::ProviderState(state) => {
                if state.kind() != ProviderStateKind::GeminiModelContent {
                    return request_error(
                        "provider_state",
                        "Gemini route received non-Gemini provider state",
                    );
                }
                // A replayed `functionCall` still needs its declaration in this
                // request, exactly as a neutral replay does; the lossless path
                // must not be the less validated one.
                for name in state_function_names(state.data()) {
                    if !advertised.contains(name) {
                        return request_error(
                            "tools",
                            "Gemini function-call replay requires the same function declaration",
                        );
                    }
                }
            }
            InferenceInput::Message(message) => {
                validate_neutral_message(message)?;
                if config.thought_signatures == SignatureRule::Mandatory
                    && message.role == Role::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty())
                {
                    // A model turn rebuilt from the neutral vocabulary has no
                    // slot for the `thoughtSignature` this route's models
                    // returned, and Gemini 3 answers a `functionCall` that lost
                    // its signature with a 400. `heycode-core` has no provider-state
                    // kind that can carry a Gemini turn losslessly, so this
                    // continuation is refused before transport instead of being
                    // sent with the signature silently dropped.
                    // <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
                    return request_error(
                        "provider_state",
                        "Gemini function-call continuation needs its `thoughtSignature`, which no Gemini provider state can carry yet",
                    );
                }
                if message.role == Role::Assistant
                    && message.tool_calls.as_ref().is_some_and(|calls| {
                        calls
                            .iter()
                            .any(|call| !advertised.contains(call.name.as_str()))
                    })
                {
                    return request_error(
                        "tools",
                        "Gemini function-call replay requires the same function declaration",
                    );
                }
            }
        }
    }
    validate_tool_result_chronology(&draft.inputs)?;
    Ok(())
}

fn validate_neutral_message(message: &ChatMessage) -> Result<(), ResolveError> {
    match message.role {
        // <https://ai.google.dev/api/generate-content> — developer instructions
        // are the top-level `systemInstruction` Content, not a `contents` role.
        Role::System => request_error(
            "inputs",
            "Gemini system instructions belong in the top-level system slot",
        ),
        Role::User => {
            if message.tool_calls.is_some()
                || message.tool_call_id.is_some()
                || message.tool_result_is_error.is_some()
            {
                return request_error("inputs", "Gemini user text has invalid tool metadata");
            }
            Ok(())
        }
        Role::Assistant => {
            if !message.images.is_empty()
                || !message.documents.is_empty()
                || message.tool_call_id.is_some()
                || message.tool_result_is_error.is_some()
            {
                return request_error("inputs", "Gemini model input cannot be a tool result");
            }
            if let Some(calls) = &message.tool_calls {
                let mut ids = BTreeSet::new();
                for call in calls {
                    if call.id.is_empty()
                        || call.id.trim() != call.id
                        || call.name.is_empty()
                        || call.name.trim() != call.name
                        || !ids.insert(call.id.as_str())
                        || serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .ok()
                            .is_none_or(|value| !value.is_object())
                    {
                        return request_error(
                            "inputs",
                            "Gemini model tool calls require unique trimmed ids/names and object JSON args",
                        );
                    }
                }
            }
            Ok(())
        }
        Role::Tool => {
            if !message.images.is_empty() || !message.documents.is_empty() {
                return request_error("inputs", "Gemini function response cannot contain media");
            }
            if message.tool_calls.is_some()
                || message
                    .tool_call_id
                    .as_ref()
                    .is_none_or(|id| id.is_empty() || id.trim() != id)
            {
                return request_error(
                    "inputs",
                    "Gemini function responses require one trimmed call id",
                );
            }
            Ok(())
        }
    }
}

/// A `functionResponse` part carries the `id` **and** the `name` of the call it
/// answers, so every tool result must sit immediately behind the model turn
/// that requested it.
/// <https://ai.google.dev/gemini-api/docs/generate-content/function-calling>
fn validate_tool_result_chronology(inputs: &[InferenceInput]) -> Result<(), ResolveError> {
    let mut pending: BTreeSet<&str> = BTreeSet::new();
    for input in inputs {
        if !pending.is_empty() {
            let InferenceInput::Message(message) = input else {
                return request_error(
                    "inputs",
                    "Gemini function calls must be followed immediately by their function responses",
                );
            };
            if message.role != Role::Tool {
                return request_error(
                    "inputs",
                    "Gemini function calls must be followed immediately by their function responses",
                );
            }
            let id =
                message
                    .tool_call_id
                    .as_deref()
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "inputs",
                        message: "Gemini function response has no call id".to_owned(),
                    })?;
            if !pending.remove(id) {
                return request_error(
                    "inputs",
                    "Gemini function response does not match a pending function call",
                );
            }
            continue;
        }
        match input {
            InferenceInput::Message(message) if message.role == Role::Tool => {
                return request_error(
                    "inputs",
                    "Gemini function response has no immediately preceding function call",
                );
            }
            InferenceInput::Message(message) if message.role == Role::Assistant => {
                if let Some(calls) = &message.tool_calls {
                    pending.extend(calls.iter().map(|call| call.id.as_str()));
                }
            }
            // A replayed model turn is a model turn: its `functionCall` parts
            // open exactly the same obligation a neutral assistant's calls do.
            InferenceInput::ProviderState(state) => {
                pending.extend(state_function_call_ids(state.data()));
            }
            InferenceInput::Message(_) => {}
        }
    }
    if pending.is_empty() {
        Ok(())
    } else {
        request_error(
            "inputs",
            "Gemini function calls are missing immediate function responses",
        )
    }
}

fn request_error<T>(field: &'static str, message: impl Into<String>) -> Result<T, ResolveError> {
    Err(ResolveError::InvalidRequest {
        field,
        message: message.into(),
    })
}

/// Gemini model ids become one path segment of
/// `.../models/{model}:streamGenerateContent`.
fn is_bare_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn generate_content_request_body(
    call: &ResolvedCall,
    config: &GeminiConfig,
    extensions: &SelectedGeminiExtensions,
) -> Result<serde_json::Value, LlmError> {
    if call.protocol() != ProviderProtocol::GeminiGenerateContent {
        return Err(invalid(
            "resolved call protocol is not Gemini GenerateContent",
        ));
    }
    let mut contents = Vec::new();
    let mut pending_responses = Vec::new();
    let mut call_names: BTreeMap<String, String> = BTreeMap::new();
    for input in call.inputs() {
        match input {
            InferenceInput::Message(message) if message.role == Role::Tool => {
                pending_responses.push(gemini_function_response_part(message, &call_names)?);
            }
            InferenceInput::Message(message) => {
                flush_function_responses(&mut contents, &mut pending_responses);
                if let Some(calls) = &message.tool_calls {
                    for tool_call in calls {
                        call_names.insert(tool_call.id.clone(), tool_call.name.clone());
                    }
                }
                contents.push(gemini_content(message)?);
            }
            // The model turn is replayed byte-for-byte, so every part keeps the
            // `thoughtSignature` it arrived with, in its own part. Its function
            // calls still seed the id → name map the following
            // `functionResponse` needs.
            InferenceInput::ProviderState(state) => {
                flush_function_responses(&mut contents, &mut pending_responses);
                for (id, name) in state_function_calls(state.data()) {
                    call_names.insert(id.to_owned(), name.to_owned());
                }
                contents.push(state.data().clone());
            }
        }
    }
    flush_function_responses(&mut contents, &mut pending_responses);

    let mut body = serde_json::json!({ "contents": contents });
    if let Some(system) = call.system() {
        body["systemInstruction"] = serde_json::json!({
            "parts":[{"text":system}],
        });
    }
    let mut tools = Vec::new();
    if !call.tools().is_empty() {
        tools.push(serde_json::json!({
            "functionDeclarations": call
                .tools()
                .iter()
                .map(gemini_function_declaration)
                .collect::<Vec<_>>(),
        }));
        // <https://ai.google.dev/gemini-api/docs/generate-content/function-calling>
        // documents `toolConfig.functionCallingConfig.mode` with `AUTO` as the
        // default behaviour.
        body["toolConfig"] = serde_json::json!({
            "functionCallingConfig":{"mode":"AUTO"},
        });
    }
    tools.extend(extensions.tools.iter().cloned());
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
    }
    for (field, value) in &extensions.top_level_fields {
        if body.get(field).is_some() {
            return Err(invalid(
                "Gemini provider option targets an owned request field",
            ));
        }
        body[field] = value.clone();
    }
    let mut generation_config = serde_json::Map::new();
    if let Some(max_output_tokens) = call.max_output_tokens() {
        generation_config.insert(
            "maxOutputTokens".to_owned(),
            serde_json::json!(max_output_tokens),
        );
    }
    if let Some(temperature) = call.temperature() {
        generation_config.insert("temperature".to_owned(), serde_json::json!(temperature));
    }
    if let Some(effort) = call.reasoning_effort() {
        let mode = config
            .thinking_mode(effort)
            .ok_or_else(|| invalid("resolved Gemini effort has no thinkingConfig wire mode"))?;
        generation_config.insert("thinkingConfig".to_owned(), mode.request_value());
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(generation_config);
    }
    Ok(body)
}

/// Every `(id, name)` a replayed model `Content` requests, in part order.
///
/// The shape is already validated by `ProviderStateItem`, so a part that is not
/// a well-formed `functionCall` is simply not a call here rather than an error:
/// this is a lookup over trusted state, not a second parse of untrusted bytes.
fn state_function_calls(state: &serde_json::Value) -> Vec<(&str, &str)> {
    let Some(parts) = state.get("parts").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| part.get("functionCall"))
        .filter_map(|call| {
            let id = call.get("id").and_then(serde_json::Value::as_str)?;
            let name = call.get("name").and_then(serde_json::Value::as_str)?;
            Some((id, name))
        })
        .collect()
}

/// Every function name a replayed model `Content` requests.
fn state_function_names(state: &serde_json::Value) -> Vec<&str> {
    state_function_calls(state)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// Every call id a replayed model `Content` leaves awaiting its response.
fn state_function_call_ids(state: &serde_json::Value) -> Vec<&str> {
    state_function_calls(state)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// <https://ai.google.dev/api/generate-content> — `Content` carries an ordered
/// `parts` array and a `role` of `user` or `model`.
fn gemini_content(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    match message.role {
        Role::System => Err(invalid(
            "Gemini system input appeared outside the top-level system slot",
        )),
        Role::Tool => Err(invalid(
            "Gemini function responses are serialized as one coalesced user content",
        )),
        Role::User => {
            let mut parts = Vec::new();
            // "When using a single image with text, place the text prompt
            // before the image in the input array."
            // <https://ai.google.dev/gemini-api/docs/image-understanding>
            // This is the opposite of the Anthropic ordering, so it is taken
            // from Gemini's own guidance rather than the house precedent.
            if !message.content.is_empty() || message.images.is_empty() {
                parts.push(serde_json::json!({"text":message.content}));
            }
            for image in &message.images {
                // <https://generativelanguage.googleapis.com/$discovery/rest?version=v1>
                // — `Part.inlineData` is a `Blob` of `mimeType` plus base64
                // `data`. The camelCase spelling is the canonical one; the
                // guides' `inline_data`/`mime_type` samples are the proto
                // aliases.
                parts.push(serde_json::json!({
                    "inlineData":{
                        "mimeType":image.media_type().as_str(),
                        "data":crate::vocab::image_base64(image),
                    }
                }));
            }
            Ok(serde_json::json!({"role":"user","parts":parts}))
        }
        Role::Assistant => {
            let mut parts = Vec::new();
            if !message.content.is_empty() {
                parts.push(serde_json::json!({"text":message.content}));
            }
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    let args: serde_json::Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| invalid("Gemini model tool args are not JSON"))?;
                    if !args.is_object() {
                        return Err(invalid("Gemini model tool args must be a JSON object"));
                    }
                    // The neutral vocabulary has no slot for `thoughtSignature`,
                    // so a model turn rebuilt from it never carries one. A route
                    // whose models require the echo refuses this continuation in
                    // `resolve` rather than emitting a signature-less turn here.
                    parts.push(serde_json::json!({
                        "functionCall":{
                            "id":call.id,
                            "name":call.name,
                            "args":args,
                        }
                    }));
                }
            }
            Ok(serde_json::json!({"role":"model","parts":parts}))
        }
    }
}

/// <https://ai.google.dev/gemini-api/docs/generate-content/function-calling> —
/// results travel as `{"role":"user","parts":[{"functionResponse":{...}}]}` and
/// the call's exact `id` must be echoed so the model can map the result back.
/// `response` itself is a free-form object, so `result`/`error` are heycode payload
/// conventions carrying the durable tool outcome, not protocol field names.
fn gemini_function_response_part(
    message: &ChatMessage,
    call_names: &BTreeMap<String, String>,
) -> Result<serde_json::Value, LlmError> {
    let id = message
        .tool_call_id
        .as_ref()
        .ok_or_else(|| invalid("Gemini function response has no call id"))?;
    let name = call_names
        .get(id.as_str())
        .ok_or_else(|| invalid("Gemini function response has no preceding call name"))?;
    let response = if message.tool_result_is_error == Some(true) {
        serde_json::json!({"error":message.content})
    } else {
        serde_json::json!({"result":message.content})
    };
    Ok(serde_json::json!({
        "functionResponse":{
            "id":id,
            "name":name,
            "response":response,
        }
    }))
}

fn flush_function_responses(
    contents: &mut Vec<serde_json::Value>,
    pending: &mut Vec<serde_json::Value>,
) {
    if !pending.is_empty() {
        contents.push(serde_json::json!({
            "role":"user",
            "parts":std::mem::take(pending),
        }));
    }
}

/// <https://ai.google.dev/gemini-api/docs/generate-content/function-calling> —
/// `tools[].functionDeclarations[]` carries `name`, `description` and
/// `parametersJsonSchema` preserves the native tools' JSON Schema vocabulary
/// (including unions and additionalProperties) without pretending it is the
/// narrower Google `Schema` message. Source: https://ai.google.dev/api/generate-content#FunctionDeclaration
fn gemini_function_declaration(tool: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "name":tool.name,
        "description":tool.description,
        "parametersJsonSchema":tool.parameters,
    })
}

enum GeminiPhase {
    Read(heycode_http::SseEventStream, Box<GeminiParser>),
    Done,
}

async fn drive_gemini(
    phase: GeminiPhase,
) -> Option<(Vec<Result<InferenceEvent, LlmError>>, GeminiPhase)> {
    match phase {
        GeminiPhase::Read(mut events, mut parser) => match events.next().await {
            Some(Ok(event)) => {
                let output = parser.event(event);
                let next = if parser.terminal {
                    GeminiPhase::Done
                } else {
                    GeminiPhase::Read(events, parser)
                };
                Some((output, next))
            }
            Some(Err(error)) => Some((vec![Err(map_transport_error(error))], GeminiPhase::Done)),
            None => Some((parser.finish(), GeminiPhase::Done)),
        },
        GeminiPhase::Done => None,
    }
}

struct OpenItem {
    output_index: u32,
    item_id: String,
    kind: StreamItemKind,
}

struct GeminiParser {
    allowed_function_names: BTreeSet<String>,
    signature_rule: SignatureRule,
    provider: String,
    model: String,
    /// Every `parts` entry this response streamed, kept byte-for-byte in
    /// arrival order. This is the only place a `thoughtSignature` survives:
    /// normalization turns parts into neutral events that have no slot for one,
    /// and the documentation requires the signature to return "in the exact
    /// part where it was received".
    /// <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
    model_parts: Vec<serde_json::Value>,
    response_id: Option<String>,
    open: Option<OpenItem>,
    next_output_index: u32,
    function_call_ids: BTreeSet<String>,
    function_call_count: usize,
    prompt_tokens: Option<u64>,
    candidates_tokens: Option<u64>,
    thoughts_tokens: Option<u64>,
    finish_reason: Option<String>,
    normalizers: Vec<Box<dyn GeminiStreamNormalizer>>,
    terminal: bool,
}

impl GeminiParser {
    fn new(
        allowed_function_names: BTreeSet<String>,
        signature_rule: SignatureRule,
        provider: String,
        model: String,
        normalizers: Vec<Box<dyn GeminiStreamNormalizer>>,
    ) -> Self {
        Self {
            allowed_function_names,
            signature_rule,
            provider,
            model,
            model_parts: Vec::new(),
            response_id: None,
            open: None,
            next_output_index: 0,
            function_call_ids: BTreeSet::new(),
            function_call_count: 0,
            prompt_tokens: None,
            candidates_tokens: None,
            thoughts_tokens: None,
            finish_reason: None,
            normalizers,
            terminal: false,
        }
    }

    fn event(&mut self, event: heycode_http::SseEvent) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        match self.parse_chunk(&event.data) {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => {
                self.terminal = true;
                vec![Err(error)]
            }
        }
    }

    /// Each server-sent event's data is one whole `GenerateContentResponse`.
    /// <https://ai.google.dev/api/generate-content>
    fn parse_chunk(&mut self, data: &str) -> Result<Vec<InferenceEvent>, LlmError> {
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|error| invalid(format!("Gemini chunk is not JSON: {error}")))?;
        let chunk = as_object(&value, "Gemini chunk")?;
        let mut output = Vec::new();

        let response_id = required_nonempty_str(chunk, "responseId", "Gemini chunk")?.to_owned();
        // The reference says `responseId` "is used to identify each response"
        // but does not state whether one streamed response repeats one id, so
        // the adapter reports the first and does not encode a stability rule it
        // cannot verify. <https://ai.google.dev/api/generate-content>
        if self.response_id.is_none() {
            self.response_id = Some(response_id.clone());
            output.push(InferenceEvent::ResponseStarted {
                response_id: response_id.clone(),
            });
        }
        // `modelVersion` is the concrete version that served the request, which
        // an alias such as a `-latest` id resolves to, so it is shape-checked
        // and never compared with the requested model id.
        if let Some(model_version) = chunk.get("modelVersion").filter(|value| !value.is_null())
            && model_version
                .as_str()
                .is_none_or(|value| value.is_empty() || value.trim() != value)
        {
            return Err(invalid("Gemini `modelVersion` must be a non-empty string"));
        }
        if let Some(usage) = chunk.get("usageMetadata").filter(|value| !value.is_null()) {
            self.usage_metadata(as_object(usage, "Gemini usageMetadata")?)?;
            for normalizer in &mut self.normalizers {
                output.extend(
                    normalizer
                        .observe_usage(usage)
                        .map_err(|_| invalid("Gemini provider usage extension is invalid"))?,
                );
            }
        }
        if let Some(candidates) = chunk.get("candidates").filter(|value| !value.is_null()) {
            let candidates = candidates
                .as_array()
                .ok_or_else(|| invalid("Gemini `candidates` must be an array"))?;
            if candidates.len() > 1 {
                return Err(invalid(
                    "Gemini multi-candidate responses are not normalized",
                ));
            }
            for candidate in candidates {
                let candidate_object = as_object(candidate, "Gemini candidate")?;
                output.extend(self.candidate(candidate_object)?);
                for normalizer in &mut self.normalizers {
                    output.extend(
                        normalizer
                            .observe_candidate(&response_id, candidate)
                            .map_err(|_| {
                                invalid("Gemini provider candidate extension is invalid")
                            })?,
                    );
                }
            }
        }
        Ok(output)
    }

    /// <https://ai.google.dev/api/generate-content> — `usageMetadata` counters
    /// are integers and every one of them is optional on the wire. An absent
    /// counter is unknown, so it never becomes a zero fact.
    fn usage_metadata(
        &mut self,
        usage: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), LlmError> {
        update_usage_component(
            &mut self.prompt_tokens,
            optional_u64(usage, "promptTokenCount", "Gemini usageMetadata")?,
            "promptTokenCount",
        )?;
        update_usage_component(
            &mut self.candidates_tokens,
            optional_u64(usage, "candidatesTokenCount", "Gemini usageMetadata")?,
            "candidatesTokenCount",
        )?;
        update_usage_component(
            &mut self.thoughts_tokens,
            optional_u64(usage, "thoughtsTokenCount", "Gemini usageMetadata")?,
            "thoughtsTokenCount",
        )?;
        // `cachedContentTokenCount` is documented as the count of tokens in the
        // *cached part of the prompt*, so it is already inside
        // `promptTokenCount` and is shape-checked rather than added.
        // `totalTokenCount` and the tool-use counters are likewise validated
        // without encoding an unverified arithmetic identity.
        for field in [
            "cachedContentTokenCount",
            "toolUsePromptTokenCount",
            "totalTokenCount",
        ] {
            optional_u64(usage, field, "Gemini usageMetadata")?;
        }
        Ok(())
    }

    fn candidate(
        &mut self,
        candidate: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if let Some(index) = candidate.get("index").filter(|value| !value.is_null())
            && index.as_u64() != Some(0)
        {
            return Err(invalid("Gemini candidate index must be zero"));
        }
        let mut output = Vec::new();
        if let Some(content) = candidate.get("content").filter(|value| !value.is_null()) {
            let content = as_object(content, "Gemini candidate content")?;
            if let Some(role) = content.get("role").filter(|value| !value.is_null())
                && role.as_str() != Some("model")
            {
                return Err(invalid("Gemini candidate content role must be `model`"));
            }
            if let Some(parts) = content.get("parts").filter(|value| !value.is_null()) {
                let parts = parts
                    .as_array()
                    .ok_or_else(|| invalid("Gemini candidate `parts` must be an array"))?;
                for part in parts {
                    let extension_index = self.next_output_index;
                    output.extend(self.part(as_object(part, "Gemini part")?)?);
                    for normalizer in &mut self.normalizers {
                        output.extend(
                            normalizer
                                .observe_part(
                                    self.response_id.as_deref().unwrap_or("gemini"),
                                    extension_index,
                                    part,
                                )
                                .map_err(|_| {
                                    invalid("Gemini provider part extension is invalid")
                                })?,
                        );
                    }
                    // The raw value is recorded rather than anything rebuilt
                    // from the normalized events, which is what makes a
                    // signature on a text part survive as well as one on a
                    // function call.
                    //
                    // Recording after validation is defensive ordering, not the
                    // guarantee: a failed part terminates the parser, so
                    // `settle` never runs and no half turn can be published
                    // either way. Mutation testing confirmed the ordering alone
                    // changes nothing observable, so it is not claimed to.
                    self.model_parts.push(part.clone());
                }
            }
        }
        if let Some(reason) = candidate
            .get("finishReason")
            .filter(|value| !value.is_null())
        {
            let reason = reason
                .as_str()
                .filter(|reason| !reason.is_empty())
                .ok_or_else(|| invalid("Gemini `finishReason` must be a non-empty string"))?;
            if self.finish_reason.replace(reason.to_owned()).is_some() {
                return Err(invalid("Gemini stream supplied finishReason twice"));
            }
        }
        Ok(output)
    }

    /// One ordered `Part`. Order inside a `parts` array is semantic, so parts
    /// are normalized strictly left to right and a run of same-kind parts
    /// continues one normalized item.
    /// <https://ai.google.dev/api/generate-content>
    fn part(
        &mut self,
        part: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let signature = match part.get("thoughtSignature") {
            None | Some(serde_json::Value::Null) => None,
            // <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
            // shows `"thoughtSignature": "<Signature A>"` beside the part it
            // belongs to.
            Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.as_str()),
            Some(_) => {
                return Err(invalid(
                    "Gemini `thoughtSignature` must be a non-empty string",
                ));
            }
        };
        // `Part`'s content fields are a union, so exactly one may be set. Two
        // set fields would make the dispatch order below silently discard
        // whichever the adapter checks second.
        // <https://ai.google.dev/api/generate-content>
        if PART_CONTENT_FIELDS
            .iter()
            .filter(|field| part.get(**field).is_some_and(|value| !value.is_null()))
            .count()
            > 1
        {
            return Err(invalid("Gemini part set more than one content field"));
        }
        if let Some(text) = part.get("text").filter(|value| !value.is_null()) {
            let text = text
                .as_str()
                .ok_or_else(|| invalid("Gemini part `text` must be a string"))?;
            // <https://ai.google.dev/gemini-api/docs/generate-content/thinking>
            // marks a thought summary part with `"thought": true`.
            let kind = match part.get("thought") {
                None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => {
                    StreamItemKind::Message
                }
                Some(serde_json::Value::Bool(true)) => StreamItemKind::Reasoning,
                Some(_) => return Err(invalid("Gemini part `thought` must be a boolean")),
            };
            let mut output = Vec::new();
            self.continue_item(kind.clone(), &mut output)?;
            if !text.is_empty() {
                output.push(if kind == StreamItemKind::Reasoning {
                    InferenceEvent::ReasoningDelta(text.to_owned())
                } else {
                    InferenceEvent::TextDelta(text.to_owned())
                });
            }
            return Ok(output);
        }
        if let Some(function_call) = part.get("functionCall").filter(|value| !value.is_null()) {
            return self.function_call(as_object(function_call, "Gemini functionCall")?, signature);
        }
        for input_only in ["functionResponse", "inlineData", "fileData"] {
            if part.get(input_only).is_some_and(|value| !value.is_null()) {
                return Err(invalid("Gemini response contained an input-only part"));
            }
        }
        for extension in ["executableCode", "codeExecutionResult"] {
            if part.get(extension).is_some_and(|value| !value.is_null()) {
                if !self
                    .normalizers
                    .iter()
                    .any(|normalizer| normalizer.accepts_part(extension))
                {
                    return Err(invalid(
                        "Gemini response extension part has no configured normalizer",
                    ));
                }
                let mut output = Vec::new();
                output.extend(self.close_open());
                let item_id = format!(
                    "{}/parts/{}",
                    self.response_id.as_deref().unwrap_or("gemini"),
                    self.next_output_index
                );
                output.push(self.start_item(StreamItemKind::Other(extension.to_owned()), item_id));
                output.extend(self.close_open());
                return Ok(output);
            }
        }
        // A part carrying only a signature has no visible output but is still
        // well formed; anything else is an unrecognized part the adapter must
        // not silently discard.
        if signature.is_some() {
            Ok(Vec::new())
        } else {
            Err(invalid("Gemini part has no recognized content"))
        }
    }

    /// <https://ai.google.dev/gemini-api/docs/generate-content/function-calling>
    /// — a `functionCall` carries `name`, `args` and, for current models, a
    /// unique `id` that the matching `functionResponse` must echo.
    fn function_call(
        &mut self,
        function_call: &serde_json::Map<String, serde_json::Value>,
        signature: Option<&str>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let name = required_nonempty_str(function_call, "name", "Gemini functionCall")?;
        if !self.allowed_function_names.contains(name) {
            return Err(invalid(
                "Gemini response requested an unadvertised function",
            ));
        }
        let id = required_nonempty_str(function_call, "id", "Gemini functionCall")?;
        if !self.function_call_ids.insert(id.to_owned()) {
            return Err(invalid("Gemini function call id was used twice"));
        }
        // Google's JSON transcoding omits an empty message field, so a call with
        // no arguments arrives without `args`.
        let args = match function_call.get("args") {
            None | Some(serde_json::Value::Null) => {
                serde_json::Value::Object(serde_json::Map::new())
            }
            Some(value) if value.is_object() => value.clone(),
            Some(_) => return Err(invalid("Gemini functionCall `args` must be an object")),
        };
        if self.signature_rule == SignatureRule::Mandatory
            && self.function_call_count == 0
            && signature.is_none()
        {
            // "The first `functionCall` part in each step of the current turn
            // must include its `thought_signature`."
            // <https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures>
            return Err(invalid(
                "Gemini first function call omitted its thought signature",
            ));
        }
        self.function_call_count = self.function_call_count.saturating_add(1);

        let mut output = Vec::new();
        output.extend(self.close_open());
        let output_index = self.next_output_index;
        output.push(self.start_item(StreamItemKind::FunctionCall, id.to_owned()));
        output.push(InferenceEvent::ToolCallDelta {
            output_index,
            id: Some(heycode_core::CallId::from_raw(id)),
            name: Some(name.to_owned()),
            arguments_delta: serde_json::to_string(&args)
                .map_err(|_| invalid("Gemini functionCall args could not be serialized"))?,
        });
        output.extend(self.close_open());
        Ok(output)
    }

    fn continue_item(
        &mut self,
        kind: StreamItemKind,
        output: &mut Vec<InferenceEvent>,
    ) -> Result<(), LlmError> {
        if self.open.as_ref().is_some_and(|open| open.kind == kind) {
            return Ok(());
        }
        output.extend(self.close_open());
        let response_id = self
            .response_id
            .as_deref()
            .ok_or_else(|| invalid("Gemini part arrived before a response id"))?;
        let item_id = format!("{response_id}/parts/{}", self.next_output_index);
        output.push(self.start_item(kind, item_id));
        Ok(())
    }

    fn start_item(&mut self, kind: StreamItemKind, item_id: String) -> InferenceEvent {
        let output_index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        self.open = Some(OpenItem {
            output_index,
            item_id: item_id.clone(),
            kind: kind.clone(),
        });
        InferenceEvent::ItemStarted {
            output_index,
            item_id,
            kind,
        }
    }

    fn close_open(&mut self) -> Option<InferenceEvent> {
        self.open.take().map(|item| InferenceEvent::ItemFinished {
            output_index: item.output_index,
            item_id: item.item_id,
            kind: item.kind,
        })
    }

    fn finish(mut self) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        self.terminal = true;
        match self.settle() {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => vec![Err(error)],
        }
    }

    fn settle(&mut self) -> Result<Vec<InferenceEvent>, LlmError> {
        let response_id = self
            .response_id
            .clone()
            .ok_or_else(|| invalid("Gemini SSE ended before any response chunk"))?;
        let finish_reason = self
            .finish_reason
            .clone()
            .ok_or_else(|| invalid("Gemini SSE ended before a terminal finishReason"))?;
        // <https://ai.google.dev/api/generate-content> lists the FinishReason
        // enum. There is no tool-call reason: a turn that requested functions
        // still stops with `STOP`.
        let finish = match finish_reason.as_str() {
            "STOP" if self.function_call_count > 0 => FinishReason::ToolCalls,
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            _ => {
                return Err(crate::retry::provider_event_error(
                    crate::ProviderErrorClass::InvalidRequest,
                    Some(finish_reason.as_str()),
                ));
            }
        };
        let mut output = Vec::new();
        output.extend(self.close_open());
        for normalizer in &mut self.normalizers {
            output.extend(
                normalizer
                    .finish(&response_id, self.next_output_index)
                    .map_err(|_| invalid("Gemini provider extension did not settle"))?,
            );
        }
        // The complete model turn, exactly as it arrived. A response that
        // produced no part at all (a blocked candidate) publishes no state
        // rather than an empty `Content` that says nothing.
        if !self.model_parts.is_empty() {
            let state = ProviderStateItem::new(
                self.provider.clone(),
                self.model.clone(),
                ProviderProtocol::GeminiGenerateContent,
                ProviderStateKind::GeminiModelContent,
                serde_json::json!({
                    "role":"model",
                    "parts":std::mem::take(&mut self.model_parts),
                }),
            )
            .map_err(|error| invalid(error.to_string()))?;
            output.push(InferenceEvent::ProviderState(state));
        }
        output.push(InferenceEvent::ResponseFinished {
            response_id,
            status: finish_reason,
        });
        // Thinking tokens are billed on top of the visible output — "response
        // pricing is the sum of output tokens and thinking tokens" — so they are
        // added rather than assumed to be inside `candidatesTokenCount`.
        // <https://ai.google.dev/gemini-api/docs/generate-content/thinking>
        if let (Some(prompt_tokens), Some(candidates_tokens)) =
            (self.prompt_tokens, self.candidates_tokens)
        {
            let completion_tokens = candidates_tokens
                .checked_add(self.thoughts_tokens.unwrap_or(0))
                .ok_or_else(|| invalid("Gemini completion usage overflowed u64"))?;
            output.push(InferenceEvent::Usage(TokenUsage {
                prompt_tokens,
                completion_tokens,
            }));
        }
        output.push(InferenceEvent::Finish(finish));
        Ok(output)
    }
}

fn update_usage_component(
    current: &mut Option<u64>,
    next: Option<u64>,
    field: &str,
) -> Result<(), LlmError> {
    if let Some(next) = next {
        if current.is_some_and(|previous| next < previous) {
            return Err(invalid(format!(
                "Gemini cumulative `{field}` usage moved backwards"
            )));
        }
        *current = Some(next);
    }
    Ok(())
}

fn as_object<'a>(
    value: &'a serde_json::Value,
    context: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn required_nonempty_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, LlmError> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a string")))?;
    if value.is_empty() || value.trim() != value {
        Err(invalid(format!("{context} `{field}` must be non-empty")))
    } else {
        Ok(value)
    }
}

fn optional_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<u64>, LlmError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{context} `{field}` must be a u64"))),
    }
}

fn invalid(message: impl Into<String>) -> LlmError {
    LlmError::InvalidResponse(message.into())
}

fn map_transport_error(error: heycode_http::TransportError) -> LlmError {
    crate::classify_transport_error(error)
}

fn one_error(error: LlmError) -> InferenceStream {
    Box::pin(futures::stream::once(async move { Err(error) }))
}
