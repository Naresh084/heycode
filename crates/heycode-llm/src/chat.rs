//! OpenAI Chat Completions protocol adapter over shared raw HTTP/SSE.

use crate::inference::{ToolAdmission, resolve_request_with_tool_admission};
use std::collections::{BTreeMap, BTreeSet};

use futures::StreamExt as _;

use crate::{
    AuthenticationBinding, ChatMessage, FinishReason, InferenceAdapter, InferenceEvent,
    InferenceInput, InferenceStream, InferenceTarget, LlmError, ModelDescriptor, NativeFeature,
    ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId,
    ReasoningEffortOptions, RequestDraft, ResolveError, ResolveSpec, ResolvedCall, Role,
    StreamItemKind, TokenUsage, ToolSpec,
};

/// Provider-specific reasoning request shape for Chat-compatible routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatReasoningWire {
    /// No reasoning effort field is supported by this route.
    None,
    /// `{ "reasoning": { "effort": "..." } }` (for example OpenRouter).
    ObjectEffort,
    /// `{ "reasoning_effort": "..." }`.
    ScalarEffort,
}

/// Provider-required assistant reasoning state for tool continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatReasoningContinuation {
    /// No additional reasoning-state requirement.
    None,
    /// Require nonempty `reasoning_content` (DeepSeek thinking contract).
    ReasoningContent,
    /// Require a nonempty `reasoning`/`reasoning_content` string or complete
    /// nonempty `reasoning_details` sequence (OpenRouter contract).
    ReasoningOrDetails,
    /// Retain authoritative native assistant state, including any reasoning returned,
    /// while allowing a provider to emit a tool call without a new reasoning block.
    NativeStateWithOptionalReasoning,
}

/// Provider-configured `thinking.type` toggle plus exact effort mapping for a
/// Chat-compatible route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatThinkingConfig {
    disabled_effort: ReasoningEffortId,
    effort_map: Vec<(ReasoningEffortId, ReasoningEffortId)>,
    omit_temperature_when_enabled: bool,
    omit_automatic_tool_controls_when_enabled: bool,
    require_reasoning_content_for_tool_calls: bool,
}

impl ChatThinkingConfig {
    /// Configure `{"thinking":{"type":"enabled|disabled"}}` with one
    /// canonical disabled id and canonical-to-wire enabled effort mappings.
    #[must_use]
    pub fn object_type(
        disabled_effort: ReasoningEffortId,
        effort_map: Vec<(ReasoningEffortId, ReasoningEffortId)>,
    ) -> Self {
        Self {
            disabled_effort,
            effort_map,
            omit_temperature_when_enabled: false,
            omit_automatic_tool_controls_when_enabled: false,
            require_reasoning_content_for_tool_calls: false,
        }
    }

    /// Omit `temperature` whenever the resolved toggle is enabled.
    #[must_use]
    pub fn omit_temperature_when_enabled(mut self) -> Self {
        self.omit_temperature_when_enabled = true;
        self
    }

    /// Omit generic `tool_choice` and `parallel_tool_calls` controls whenever
    /// thinking is enabled while retaining the tool schemas themselves.
    #[must_use]
    pub fn omit_automatic_tool_controls_when_enabled(mut self) -> Self {
        self.omit_automatic_tool_controls_when_enabled = true;
        self
    }

    /// Require complete nonempty `reasoning_content` on every assistant
    /// tool-call state emitted or replayed while thinking is enabled.
    #[must_use]
    pub fn require_reasoning_content_for_tool_calls(mut self) -> Self {
        self.require_reasoning_content_for_tool_calls = true;
        self
    }
}

/// Reusable Chat Completions route configuration. Debug output is redacted.
#[derive(Clone)]
pub struct OpenAiChatCompletionsConfig {
    provider: ProviderDescriptor,
    base_url: String,
    credential: Option<crate::RouteCredential>,
    extra_headers: Vec<(String, String)>,
    reasoning_efforts: Vec<ReasoningEffortId>,
    default_reasoning_effort: Option<ReasoningEffortId>,
    reasoning_vocabulary: ChatReasoningVocabulary,
    reasoning_wire: ChatReasoningWire,
    thinking: Option<ChatThinkingConfig>,
    preserve_thinking: bool,
    reasoning_split: Option<bool>,
    image_detail: String,
    default_max_output_tokens: Option<u64>,
    retry_spec: crate::RetrySpec,
    tool_admission: ToolAdmission,
    provider_request_options: Vec<ProviderRequestOptionWire>,
    anthropic_cache_option: Option<String>,
    reasoning_continuation: ChatReasoningContinuation,
    server_tools: Vec<(NativeFeature, serde_json::Value)>,
    max_server_tool_calls: Option<u32>,
    url_citations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRequestOptionWire {
    kind: String,
    field: String,
    data_member: Option<String>,
}

impl OpenAiChatCompletionsConfig {
    /// Attempt requested function tools when a user-selected endpoint has no
    /// model capability evidence. Unknown remains Unknown; explicit Unsupported
    /// still fails locally and endpoint errors propagate without dropping tools.
    #[must_use]
    pub fn with_unknown_tool_attempts(mut self) -> Self {
        self.tool_admission = ToolAdmission::AttemptUnknown;
        self
    }

    /// Build one bearer-authenticated Chat Completions route from a literal
    /// key captured for this adapter's lifetime.
    #[must_use]
    pub fn with_key(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_credential(provider, base_url, crate::RouteCredential::fixed(api_key))
    }

    /// Build one bearer-authenticated Chat Completions route whose credential
    /// is resolved once per operation.
    #[must_use]
    pub fn with_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential: Some(credential),
            extra_headers: Vec::new(),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            reasoning_vocabulary: ChatReasoningVocabulary::RouteWide,
            reasoning_wire: ChatReasoningWire::None,
            thinking: None,
            preserve_thinking: false,
            reasoning_split: None,
            image_detail: "auto".into(),
            default_max_output_tokens: None,
            retry_spec: crate::RetrySpec::standard(),
            tool_admission: ToolAdmission::RequireEvidence,
            provider_request_options: Vec::new(),
            anthropic_cache_option: None,
            reasoning_continuation: ChatReasoningContinuation::None,
            server_tools: Vec::new(),
            max_server_tool_calls: None,
            url_citations: false,
        }
    }

    /// Build one Chat Completions route that sends no authentication header.
    ///
    /// This is distinct from a blank or unresolved credential: the resolved
    /// call records [`AuthenticationBinding::None`] and dispatch never builds
    /// an `Authorization` header.
    #[must_use]
    pub fn without_authentication(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential: None,
            extra_headers: Vec::new(),
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            reasoning_vocabulary: ChatReasoningVocabulary::RouteWide,
            reasoning_wire: ChatReasoningWire::None,
            thinking: None,
            preserve_thinking: false,
            reasoning_split: None,
            image_detail: "auto".into(),
            default_max_output_tokens: None,
            retry_spec: crate::RetrySpec::standard(),
            tool_admission: ToolAdmission::RequireEvidence,
            provider_request_options: Vec::new(),
            anthropic_cache_option: None,
            reasoning_continuation: ChatReasoningContinuation::None,
            server_tools: Vec::new(),
            max_server_tool_calls: None,
            url_citations: false,
        }
    }

    /// Restrict effort selection to each model's own published vocabulary.
    ///
    /// The configured route list then applies only to the exactly named ids
    /// whose vocabulary this route has separately verified. Every other model
    /// exposes what its catalog row published, and nothing when the row
    /// published no vocabulary.
    #[must_use]
    pub fn with_model_published_reasoning(mut self, verified_route_models: Vec<String>) -> Self {
        self.reasoning_vocabulary = ChatReasoningVocabulary::ModelPublished {
            verified_route_models,
        };
        self
    }

    /// Attach exact effort choices/default and their provider request dialect.
    #[must_use]
    pub fn with_reasoning(
        mut self,
        efforts: Vec<ReasoningEffortId>,
        default: Option<ReasoningEffortId>,
        wire: ChatReasoningWire,
    ) -> Self {
        self.reasoning_efforts = efforts;
        self.default_reasoning_effort = default;
        self.reasoning_wire = wire;
        self
    }

    /// Attach an explicit thinking toggle and canonical-to-wire effort map.
    #[must_use]
    pub fn with_thinking(mut self, thinking: ChatThinkingConfig) -> Self {
        self.thinking = Some(thinking);
        self
    }

    /// Preserve prior GLM reasoning without exposing an unsupported off toggle.
    /// Sends `thinking.clear_thinking=false` independently of effort controls.
    #[must_use]
    pub fn with_preserved_thinking(mut self) -> Self {
        self.preserve_thinking = true;
        self
    }

    /// Pin MiniMax's documented reasoning output format, independently of effort.
    #[must_use]
    pub fn with_reasoning_split(mut self, split: bool) -> Self {
        self.reasoning_split = Some(split);
        self
    }

    /// Select the documented image detail spelling for this Chat endpoint.
    /// Accepted wire values are auto, default, low and high; construction validates it.
    #[must_use]
    pub fn with_image_detail(mut self, detail: impl Into<String>) -> Self {
        self.image_detail = detail.into();
        self
    }

    /// Attach an adapter-owned output default.
    #[must_use]
    pub fn with_default_max_output_tokens(mut self, value: Option<u64>) -> Self {
        self.default_max_output_tokens = value;
        self
    }

    /// Attach an explicit validated retry policy.
    #[must_use]
    pub fn with_retry_spec(mut self, retry_spec: crate::RetrySpec) -> Self {
        self.retry_spec = retry_spec;
        self
    }

    /// Attach validated-at-dispatch extra headers (attribution/gateway).
    #[must_use]
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Bind a durable ephemeral-cache policy to Anthropic message breakpoints.
    /// Other model families retain their automatic caching behavior. This uses
    /// content-block markers without changing provider routing or prompt text.
    #[must_use]
    pub fn with_anthropic_cache_breakpoints(mut self, kind: impl Into<String>) -> Self {
        self.anthropic_cache_option = Some(kind.into());
        self
    }

    /// Bind one provider-owned durable option kind to an exact top-level Chat
    /// request field.
    #[must_use]
    pub fn with_provider_request_option(
        mut self,
        kind: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        self.provider_request_options
            .push(ProviderRequestOptionWire {
                kind: kind.into(),
                field: field.into(),
                data_member: None,
            });
        self
    }

    /// Bind one exact member of a provider-owned durable option to a top-level
    /// Chat request field.
    ///
    /// The option object must contain exactly `member`; refusing additional
    /// members prevents a wire projection from silently dropping durable data.
    #[must_use]
    pub fn with_provider_request_option_member(
        mut self,
        kind: impl Into<String>,
        member: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        self.provider_request_options
            .push(ProviderRequestOptionWire {
                kind: kind.into(),
                field: field.into(),
                data_member: Some(member.into()),
            });
        self
    }

    /// Require provider reasoning state on assistant tool-call continuation.
    #[must_use]
    pub fn with_reasoning_continuation(mut self, continuation: ChatReasoningContinuation) -> Self {
        self.reasoning_continuation = continuation;
        self
    }

    /// Attach one exact provider server-tool definition to a native feature.
    #[must_use]
    pub fn with_server_tool(
        mut self,
        feature: NativeFeature,
        definition: serde_json::Value,
    ) -> Self {
        self.server_tools.push((feature, definition));
        self
    }

    /// Set the explicit top-level server-tool execution budget.
    #[must_use]
    pub fn with_max_server_tool_calls(mut self, maximum: u32) -> Self {
        self.max_server_tool_calls = Some(maximum);
        self
    }

    /// Enable validated `url_citation` response annotations for this route.
    #[must_use]
    pub fn with_url_citations(mut self) -> Self {
        self.url_citations = true;
        self
    }

    fn server_tools(&self, feature: NativeFeature) -> impl Iterator<Item = &serde_json::Value> {
        self.server_tools
            .iter()
            .filter_map(move |(configured, value)| (*configured == feature).then_some(value))
    }

    /// Secret-free exact-route resolution proposal.
    #[must_use]
    pub fn resolve_spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::OpenAiChatCompletions,
            target: InferenceTarget::Http {
                base_url: self.base_url.clone(),
            },
            authentication: self
                .credential
                .as_ref()
                .map_or(AuthenticationBinding::None, crate::RouteCredential::binding),
            default_max_output_tokens: self.default_max_output_tokens,
            reasoning_efforts: self.reasoning_efforts.clone(),
            default_reasoning_effort: self.default_reasoning_effort.clone(),
        }
    }

    /// Exact-route resolution proposal narrowed to one model's own effort
    /// vocabulary. Identical to [`Self::resolve_spec`] on a route that owns a
    /// single vocabulary for every model it serves.
    ///
    /// # Errors
    /// A published effort id that cannot be a request value fails loudly
    /// rather than being dropped from the model's vocabulary.
    pub fn resolve_spec_for_model(
        &self,
        model: &ModelDescriptor,
    ) -> Result<ResolveSpec, ResolveError> {
        let mut spec = self.resolve_spec();
        let ChatReasoningVocabulary::ModelPublished {
            verified_route_models,
        } = &self.reasoning_vocabulary
        else {
            return Ok(spec);
        };
        match model
            .reasoning
            .as_ref()
            .filter(|published| published.names_efforts())
        {
            Some(published) => {
                spec.reasoning_efforts = published
                    .efforts()
                    .iter()
                    .map(ReasoningEffortId::new)
                    .collect::<Result<_, _>>()?;
                spec.default_reasoning_effort = published
                    .default_effort()
                    .map(ReasoningEffortId::new)
                    .transpose()?;
            }
            // A verified route id keeps the list this adapter proved for it.
            // Any other model without a published vocabulary exposes none.
            None if verified_route_models.contains(&model.id) => {}
            None => {
                spec.reasoning_efforts = Vec::new();
                spec.default_reasoning_effort = None;
            }
        }
        Ok(spec)
    }
}

/// Whether one Chat route's configured effort list describes every model it
/// serves, or only the ids whose vocabulary it verified.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatReasoningVocabulary {
    /// The configured list is the vocabulary for every reasoning-capable model.
    RouteWide,
    /// Each model's published vocabulary wins; the configured list belongs to
    /// these verified ids alone.
    ModelPublished {
        /// Exact model ids whose configured vocabulary this route verified.
        verified_route_models: Vec<String>,
    },
}

impl std::fmt::Debug for OpenAiChatCompletionsConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiChatCompletionsConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("extra_header_count", &self.extra_headers.len())
            .field("reasoning_efforts", &self.reasoning_efforts)
            .field("reasoning_vocabulary", &self.reasoning_vocabulary)
            .field("default_reasoning_effort", &self.default_reasoning_effort)
            .field("reasoning_wire", &self.reasoning_wire)
            .field("thinking", &self.thinking)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("retry_spec", &self.retry_spec)
            .field("provider_request_options", &self.provider_request_options)
            .field("anthropic_cache_option", &self.anthropic_cache_option)
            .field("reasoning_continuation", &self.reasoning_continuation)
            .field("server_tool_count", &self.server_tools.len())
            .field("max_server_tool_calls", &self.max_server_tool_calls)
            .field("url_citations", &self.url_citations)
            .finish()
    }
}

/// Reusable OpenAI-compatible Chat Completions protocol adapter.
#[derive(Clone)]
pub struct OpenAiChatCompletionsAdapter {
    config: OpenAiChatCompletionsConfig,
    http: heycode_http::HttpService,
}

impl OpenAiChatCompletionsAdapter {
    /// Validate configuration and bind the shared HTTP service.
    ///
    /// # Errors
    /// Missing protocol declaration, unusable key/endpoint/header, duplicate
    /// effort metadata, invalid default, or reasoning dialect mismatch.
    pub fn new(
        config: OpenAiChatCompletionsConfig,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        validate_config(&config)?;
        if !matches!(
            config.image_detail.as_str(),
            "auto" | "default" | "low" | "high"
        ) {
            return Err(invalid("Chat image detail is invalid"));
        }
        Ok(Self { config, http })
    }
}

impl InferenceAdapter for OpenAiChatCompletionsAdapter {
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
        let spec = self.config.resolve_spec_for_model(model)?;
        ReasoningEffortOptions::for_model(
            model,
            spec.reasoning_efforts,
            spec.default_reasoning_effort,
        )
    }

    fn resolve(
        &self,
        mut draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        validate_provider_option_dialect(&self.config, &draft.provider_options)?;
        if draft.temperature.is_some_and(|value| !value.is_finite()) {
            return Err(ResolveError::InvalidRequest {
                field: "temperature",
                message: "temperature must be finite".to_owned(),
            });
        }
        if thinking_enabled_for_draft(&self.config, &draft, model) == Some(true)
            && self
                .config
                .thinking
                .as_ref()
                .is_some_and(|thinking| thinking.omit_temperature_when_enabled)
        {
            draft.temperature = None;
        }
        if draft.structured_output.is_some() {
            return Err(ResolveError::InvalidRequest {
                field: "structured_output",
                message: "Chat structured-output dialect is not configured yet".to_owned(),
            });
        }
        for feature in &draft.native_features {
            if self.config.server_tools(*feature).next().is_none() {
                return Err(ResolveError::InvalidRequest {
                    field: "native_features",
                    message: "Chat native feature has no configured server-tool dialect".to_owned(),
                });
            }
        }
        if draft.inputs.iter().any(|input| {
            matches!(input, InferenceInput::ProviderState(state)
                if state.kind() != ProviderStateKind::ChatAssistantMessage)
        }) {
            return Err(ResolveError::InvalidRequest {
                field: "provider_state",
                message: "Chat route received non-Chat provider state".to_owned(),
            });
        }
        for state in draft.inputs.iter().filter_map(|input| match input {
            InferenceInput::ProviderState(state) => Some(state),
            InferenceInput::Message(_) => None,
        }) {
            validate_chat_annotations(&self.config, state.data()).map_err(|_| {
                ResolveError::InvalidRequest {
                    field: "provider_state",
                    message: "Chat provider-state annotations are invalid for this route"
                        .to_owned(),
                }
            })?;
        }
        let native_server_tools = !draft.native_features.is_empty();
        let call = resolve_request_with_tool_admission(
            &self.config.provider,
            draft,
            model,
            &self.config.resolve_spec_for_model(model)?,
            self.config.tool_admission,
        )?
        .with_retry_spec(if native_server_tools {
            self.config.retry_spec.clone().disable_replay()
        } else {
            self.config.retry_spec.clone()
        });
        let continuation = reasoning_continuation_for_call(&self.config, &call);
        if continuation != ChatReasoningContinuation::None {
            validate_reasoning_tool_inputs(call.inputs(), continuation)?;
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
        let response_policy = ChatResponsePolicy {
            reasoning_continuation: reasoning_continuation_for_call(&self.config, &call),
            url_citations: self.config.url_citations && !call.native_features().is_empty(),
            max_server_tool_calls: call
                .native_features()
                .first()
                .and(self.config.max_server_tool_calls),
        };
        let body = match chat_request_body(&call, &self.config) {
            Ok(body) => body,
            Err(error) => return one_error(error),
        };
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = body.to_string().into_bytes();
        let provider = call.provider().to_owned();
        let model = call.model().to_owned();
        let retry_spec = call.retry_spec().clone();
        let config = self.config.clone();
        let http = self.http.clone();
        // Resolved once, before the first attempt: every retry of this one
        // operation reuses it, and the next operation resolves again.
        let credential = match self
            .config
            .credential
            .as_ref()
            .map(crate::RouteCredential::acquire)
        {
            Some(Ok(credential)) => Some(credential),
            Some(Err(error)) => return one_error(LlmError::UnresolvedCredential(error)),
            None => None,
        };
        crate::retry::retrying_stream(retry_spec, cancellation, move |attempt_cancellation| {
            let mut request = match heycode_http::HttpSseRequest::post(url.clone(), body.clone()) {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            if let Some(credential) = credential.as_ref() {
                request = match request
                    .header("authorization", &format!("Bearer {}", credential.expose()))
                {
                    Ok(request) => request,
                    Err(error) => return one_error(crate::classify_transport_error(error)),
                };
            }
            request = match request.header("content-type", "application/json") {
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
            normalize_chat_events_with_policy(
                events,
                provider.clone(),
                model.clone(),
                response_policy,
            )
        })
    }
}

fn thinking_enabled_for_call(
    config: &OpenAiChatCompletionsConfig,
    call: &ResolvedCall,
) -> Option<bool> {
    let thinking = config.thinking.as_ref()?;
    call.reasoning_effort()
        .map(|effort| effort != &thinking.disabled_effort)
}

fn reasoning_continuation_for_call(
    config: &OpenAiChatCompletionsConfig,
    call: &ResolvedCall,
) -> ChatReasoningContinuation {
    if thinking_enabled_for_call(config, call) == Some(true)
        && config
            .thinking
            .as_ref()
            .is_some_and(|thinking| thinking.require_reasoning_content_for_tool_calls)
    {
        ChatReasoningContinuation::ReasoningContent
    } else if call.reasoning_effort().is_some() {
        // Anthropic can continue a tool cycle without interleaved thinking. The
        // native message remains required so absence is not confused with dropped
        // reasoning. DeepSeek/GLM strict contracts keep their existing requirements.
        if config.reasoning_continuation == ChatReasoningContinuation::ReasoningOrDetails
            && call.provider() == "openrouter"
            && call.model().starts_with("anthropic/")
        {
            ChatReasoningContinuation::NativeStateWithOptionalReasoning
        } else {
            config.reasoning_continuation
        }
    } else {
        ChatReasoningContinuation::None
    }
}

fn validate_reasoning_tool_inputs(
    inputs: &[InferenceInput],
    requirement: ChatReasoningContinuation,
) -> Result<(), ResolveError> {
    for input in inputs {
        match input {
            InferenceInput::Message(message)
                if message.role == Role::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()) =>
            {
                return Err(missing_reasoning_tool_state(requirement));
            }
            InferenceInput::ProviderState(state) => {
                let Some(tool_calls) = state.data().get("tool_calls") else {
                    continue;
                };
                if tool_calls.is_null() {
                    continue;
                }
                let calls = tool_calls
                    .as_array()
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "provider_state",
                        message: "Chat assistant tool_calls must be an array".to_owned(),
                    })?;
                if !calls.is_empty() && !has_required_reasoning(state.data(), requirement) {
                    return Err(missing_reasoning_tool_state(requirement));
                }
            }
            InferenceInput::Message(_) => {}
        }
    }
    Ok(())
}

fn has_required_reasoning(
    data: &serde_json::Value,
    requirement: ChatReasoningContinuation,
) -> bool {
    let reasoning_content = data
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reasoning| !reasoning.is_empty());
    match requirement {
        ChatReasoningContinuation::None
        | ChatReasoningContinuation::NativeStateWithOptionalReasoning => true,
        ChatReasoningContinuation::ReasoningContent => reasoning_content,
        ChatReasoningContinuation::ReasoningOrDetails => {
            reasoning_content
                || data
                    .get("reasoning")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|reasoning| !reasoning.is_empty())
                || data
                    .get("reasoning_details")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|details| !details.is_empty())
        }
    }
}

fn missing_reasoning_tool_state(requirement: ChatReasoningContinuation) -> ResolveError {
    ResolveError::InvalidRequest {
        field: "provider_state",
        message: match requirement {
            ChatReasoningContinuation::ReasoningContent => {
                "thinking tool-call history requires complete reasoning_content"
            }
            ChatReasoningContinuation::ReasoningOrDetails => {
                "reasoning tool-call history requires complete reasoning or reasoning_details"
            }
            ChatReasoningContinuation::NativeStateWithOptionalReasoning => {
                "tool-call history requires authoritative native assistant state"
            }
            ChatReasoningContinuation::None => "reasoning tool-call history is invalid",
        }
        .to_owned(),
    }
}

fn thinking_enabled_for_draft(
    config: &OpenAiChatCompletionsConfig,
    draft: &RequestDraft,
    model: &ModelDescriptor,
) -> Option<bool> {
    let thinking = config.thinking.as_ref()?;
    let effort = match draft.reasoning_effort.as_ref() {
        Some(effort) => effort,
        None if model.capabilities.reasoning == crate::CapabilitySupport::Supported => {
            config.default_reasoning_effort.as_ref()?
        }
        None => return None,
    };
    Some(effort != &thinking.disabled_effort)
}

pub(crate) fn normalize_chat_events(
    events: heycode_http::SseEventStream,
    provider: String,
    model: String,
) -> InferenceStream {
    normalize_chat_events_with_policy(events, provider, model, ChatResponsePolicy::default())
}

fn normalize_chat_events_with_policy(
    events: heycode_http::SseEventStream,
    provider: String,
    model: String,
    policy: ChatResponsePolicy,
) -> InferenceStream {
    Box::pin(
        futures::stream::unfold(
            ChatPhase::Read(events, Box::new(ChatParser::new(provider, model, policy))),
            drive_chat,
        )
        .flat_map(futures::stream::iter),
    )
}

fn validate_config(config: &OpenAiChatCompletionsConfig) -> Result<(), LlmError> {
    if !config
        .provider
        .protocols
        .contains(&ProviderProtocol::OpenAiChatCompletions)
    {
        return Err(invalid(
            "Chat adapter provider does not declare OpenAI Chat Completions",
        ));
    }
    if config
        .credential
        .as_ref()
        .is_some_and(crate::RouteCredential::fixed_is_blank)
    {
        return Err(crate::retry::local_failure(
            crate::ProviderErrorClass::Authentication,
        ));
    }
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let mut request =
        heycode_http::HttpSseRequest::post(url, Vec::new()).map_err(map_transport_error)?;
    if let Some(credential) = config.credential.as_ref() {
        request = request
            .header(
                "authorization",
                &format!("Bearer {}", credential.probe_value()),
            )
            .map_err(map_transport_error)?;
    }
    for (name, value) in &config.extra_headers {
        request = request.header(name, value).map_err(map_transport_error)?;
    }
    drop(request);
    let mut seen = BTreeSet::new();
    for effort in &config.reasoning_efforts {
        if !seen.insert(effort.as_str()) {
            return Err(invalid("Chat adapter has duplicate reasoning effort ids"));
        }
    }
    if config
        .default_reasoning_effort
        .as_ref()
        .is_some_and(|default| !seen.contains(default.as_str()))
    {
        return Err(invalid(
            "Chat adapter reasoning default is not in its effort list",
        ));
    }
    if !config.reasoning_efforts.is_empty() && config.reasoning_wire == ChatReasoningWire::None {
        return Err(invalid(
            "Chat adapter advertises reasoning efforts without a request dialect",
        ));
    }
    validate_thinking_config(config, &seen)?;
    if config.default_max_output_tokens == Some(0) {
        return Err(invalid("Chat adapter output default must be positive"));
    }
    let mut option_kinds = BTreeSet::new();
    let mut request_fields = BTreeSet::new();
    for option in &config.provider_request_options {
        if !safe_json_field(&option.kind)
            || !safe_json_field(&option.field)
            || option
                .data_member
                .as_ref()
                .is_some_and(|member| !safe_json_field(member))
            || !option_kinds.insert(option.kind.as_str())
            || !request_fields.insert(option.field.as_str())
            || reserved_chat_field(&option.field)
        {
            return Err(invalid(
                "Chat provider request option kind/field is invalid, duplicated, or reserved",
            ));
        }
    }
    if let Some(kind) = &config.anthropic_cache_option
        && (!safe_json_field(kind) || !option_kinds.insert(kind.as_str()))
    {
        return Err(invalid("Chat cache option kind is invalid or duplicated"));
    }
    validate_server_tool_config(config)?;
    Ok(())
}

fn validate_server_tool_config(config: &OpenAiChatCompletionsConfig) -> Result<(), LlmError> {
    if config.server_tools.is_empty() {
        if config.max_server_tool_calls.is_some() || config.url_citations {
            return Err(invalid(
                "Chat server-tool controls require at least one exact definition",
            ));
        }
        return Ok(());
    }
    if config
        .max_server_tool_calls
        .is_none_or(|maximum| !(1..=30).contains(&maximum))
    {
        return Err(invalid(
            "Chat server tools require an explicit 1..=30 execution budget",
        ));
    }
    let mut seen = BTreeSet::new();
    for (feature, definition) in &config.server_tools {
        let object = definition
            .as_object()
            .ok_or_else(|| invalid("Chat server-tool definition must be an object"))?;
        let tool_type = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .filter(|tool_type| safe_server_tool_type(tool_type))
            .ok_or_else(|| invalid("Chat server-tool type is invalid"))?;
        if !seen.insert((*feature, tool_type)) {
            return Err(invalid("Chat server-tool definition is duplicated"));
        }
        if object
            .get("parameters")
            .is_some_and(|parameters| !parameters.is_object())
        {
            return Err(invalid("Chat server-tool parameters must be an object"));
        }
        if serde_json::to_vec(definition)
            .map_err(|_| invalid("Chat server-tool definition cannot serialize"))?
            .len()
            > 64 * 1024
        {
            return Err(invalid(
                "Chat server-tool definition exceeds the size limit",
            ));
        }
    }
    Ok(())
}

fn validate_provider_option_dialect(
    config: &OpenAiChatCompletionsConfig,
    options: &[heycode_core::ProviderRequestOption],
) -> Result<(), ResolveError> {
    for option in options {
        if config.anthropic_cache_option.as_deref() == Some(option.kind()) {
            if option.data() != &serde_json::json!({"type":"ephemeral"}) {
                return Err(ResolveError::InvalidRequest {
                    field: "provider_options",
                    message: "Anthropic message caching requires the exact ephemeral policy"
                        .to_owned(),
                });
            }
            continue;
        }
        let wire = config
            .provider_request_options
            .iter()
            .find(|wire| option.kind() == wire.kind)
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Chat route received an unsupported provider request option kind"
                    .to_owned(),
            })?;
        provider_option_wire_value(wire, option).map_err(|error| ResolveError::InvalidRequest {
            field: "provider_options",
            message: error.to_owned(),
        })?;
    }
    Ok(())
}

fn provider_option_wire_value(
    wire: &ProviderRequestOptionWire,
    option: &heycode_core::ProviderRequestOption,
) -> Result<serde_json::Value, &'static str> {
    let Some(member) = &wire.data_member else {
        return Ok(option.data().clone());
    };
    let data = option
        .data()
        .as_object()
        .ok_or("Chat provider request option data must be an object")?;
    if data.len() != 1 {
        return Err("Chat provider request option member projection would drop data");
    }
    data.get(member)
        .cloned()
        .ok_or("Chat provider request option is missing its configured member")
}

// Anthropic caches tools before system text, then conversation content. Two
// message breakpoints preserve the standing prefix and the latest content.
// Existing caller markers are authoritative; do not add to their four-marker
// allowance. Never mutate opaque reasoning or provider-native state fields.
fn apply_anthropic_cache_breakpoints(body: &mut serde_json::Value) {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    if messages.iter().any(|message| {
        message.get("cache_control").is_some()
            || message
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block.get("cache_control").is_some())
                })
    }) {
        return;
    }
    fn mark(message: &mut serde_json::Value) -> bool {
        let Some(content) = message.get_mut("content") else {
            return false;
        };
        if let Some(text) = content.as_str() {
            if text.is_empty() {
                return false;
            }
            *content = serde_json::json!([{"type":"text", "text":text, "cache_control":{"type":"ephemeral"}}]);
            return true;
        }
        if let Some(blocks) = content.as_array_mut()
            && let Some(block) = blocks.iter_mut().rev().find(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            })
        {
            block["cache_control"] = serde_json::json!({"type":"ephemeral"});
            return true;
        }
        false
    }
    for message in messages
        .iter_mut()
        .filter(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("system"))
        .take(1)
    {
        mark(message);
    }
    for message in messages
        .iter_mut()
        .rev()
        .filter(|message| message.get("role").and_then(serde_json::Value::as_str) != Some("system"))
    {
        if mark(message) {
            break;
        }
    }
}

fn reserved_chat_field(field: &str) -> bool {
    matches!(
        field,
        "model"
            | "messages"
            | "stream"
            | "stream_options"
            | "tools"
            | "tool_choice"
            | "parallel_tool_calls"
            | "max_tool_calls"
            | "temperature"
            | "max_tokens"
            | "response_format"
            | "reasoning"
            | "reasoning_effort"
            | "thinking"
            | "reasoning_split"
    )
}

fn validate_thinking_config(
    config: &OpenAiChatCompletionsConfig,
    accepted: &BTreeSet<&str>,
) -> Result<(), LlmError> {
    let Some(thinking) = &config.thinking else {
        return Ok(());
    };
    if config.reasoning_wire == ChatReasoningWire::None {
        return Err(invalid(
            "Chat thinking toggle requires a reasoning effort request dialect",
        ));
    }
    if !accepted.contains(thinking.disabled_effort.as_str()) {
        return Err(invalid(
            "Chat thinking disabled id is not in the accepted effort list",
        ));
    }
    let mut mapped = BTreeSet::new();
    for (canonical, _wire) in &thinking.effort_map {
        if canonical == &thinking.disabled_effort {
            return Err(invalid(
                "Chat thinking disabled id must not have a wire effort mapping",
            ));
        }
        if !accepted.contains(canonical.as_str()) {
            return Err(invalid(
                "Chat thinking effort map contains an unaccepted canonical id",
            ));
        }
        if !mapped.insert(canonical.as_str()) {
            return Err(invalid(
                "Chat thinking effort map contains a duplicate canonical id",
            ));
        }
    }
    if accepted
        .iter()
        .any(|effort| *effort != thinking.disabled_effort.as_str() && !mapped.contains(effort))
    {
        return Err(invalid(
            "Chat thinking effort map does not cover every enabled effort",
        ));
    }
    Ok(())
}

fn chat_request_body(
    call: &ResolvedCall,
    config: &OpenAiChatCompletionsConfig,
) -> Result<serde_json::Value, LlmError> {
    if call.protocol() != ProviderProtocol::OpenAiChatCompletions {
        return Err(invalid(
            "resolved call protocol is not OpenAI Chat Completions",
        ));
    }
    let mut messages = Vec::new();
    if let Some(system) = call.system() {
        messages.push(serde_json::json!({"role":"system","content":system}));
    }
    for input in call.inputs() {
        match input {
            InferenceInput::Message(message) => {
                messages.push(chat_message_with_detail(message, &config.image_detail)?)
            }
            InferenceInput::ProviderState(state) => messages.push(state.data().clone()),
        }
    }
    let mut body = serde_json::json!({
        "model": call.model(),
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage":true},
    });
    for option in call.provider_options() {
        if config.anthropic_cache_option.as_deref() == Some(option.kind()) {
            if call.model().starts_with("anthropic/") {
                apply_anthropic_cache_breakpoints(&mut body);
            }
            continue;
        }
        let wire = config
            .provider_request_options
            .iter()
            .find(|wire| option.kind() == wire.kind)
            .ok_or_else(|| invalid("resolved provider request option has no Chat wire dialect"))?;
        let value = provider_option_wire_value(wire, option).map_err(invalid)?;
        body[wire.field.as_str()] = value;
    }
    if config.preserve_thinking {
        body["thinking"] = serde_json::json!({"type":"enabled", "clear_thinking":false});
    }
    if let Some(split) = config.reasoning_split {
        body["reasoning_split"] = serde_json::json!(split);
    }
    let thinking_enabled = config.thinking.as_ref().and_then(|thinking| {
        call.reasoning_effort()
            .map(|effort| effort != &thinking.disabled_effort)
    });
    let mut wire_tools = call.tools().iter().map(chat_tool).collect::<Vec<_>>();
    for feature in call.native_features() {
        wire_tools.extend(config.server_tools(*feature).cloned());
    }
    if !wire_tools.is_empty() {
        body["tools"] = serde_json::Value::Array(wire_tools);
        let omit_controls = thinking_enabled == Some(true)
            && config
                .thinking
                .as_ref()
                .is_some_and(|thinking| thinking.omit_automatic_tool_controls_when_enabled);
        if !omit_controls {
            body["tool_choice"] = serde_json::json!("auto");
            body["parallel_tool_calls"] = serde_json::json!(true);
        }
    }
    if !call.native_features().is_empty() {
        body["max_tool_calls"] = serde_json::json!(
            config
                .max_server_tool_calls
                .ok_or_else(|| { invalid("resolved Chat server tool has no execution budget") })?
        );
    }
    let omit_temperature = thinking_enabled == Some(true)
        && config
            .thinking
            .as_ref()
            .is_some_and(|thinking| thinking.omit_temperature_when_enabled);
    if let Some(temperature) = call.temperature()
        && !omit_temperature
    {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(max_tokens) = call.max_output_tokens() {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    if let Some(effort) = call.reasoning_effort() {
        let wire_effort = if let Some(thinking) = &config.thinking {
            let enabled = effort != &thinking.disabled_effort;
            if body.get("thinking").is_none() {
                body["thinking"] = serde_json::json!({});
            }
            body["thinking"]["type"] =
                serde_json::json!(if enabled { "enabled" } else { "disabled" });
            if enabled {
                Some(
                    thinking
                        .effort_map
                        .iter()
                        .find_map(|(canonical, wire)| (canonical == effort).then_some(wire))
                        .ok_or_else(|| invalid("resolved reasoning effort has no Chat wire map"))?,
                )
            } else {
                None
            }
        } else {
            Some(effort)
        };
        let Some(wire_effort) = wire_effort else {
            return Ok(body);
        };
        match config.reasoning_wire {
            ChatReasoningWire::None => {
                return Err(invalid(
                    "resolved reasoning effort has no Chat request dialect",
                ));
            }
            ChatReasoningWire::ObjectEffort => {
                body["reasoning"] = serde_json::json!({"effort":wire_effort.as_str()});
            }
            ChatReasoningWire::ScalarEffort => {
                body["reasoning_effort"] = serde_json::json!(wire_effort.as_str());
            }
        }
    }
    Ok(body)
}

fn safe_json_field(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && bytes.len() <= 64
}

fn safe_server_tool_type(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b':' | b'.' | b'/')
        })
        && bytes.len() <= 128
}

#[cfg(test)]
fn chat_message(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    chat_message_with_detail(message, "auto")
}

fn chat_message_with_detail(
    message: &ChatMessage,
    image_detail: &str,
) -> Result<serde_json::Value, LlmError> {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    if (!message.images.is_empty() || !message.documents.is_empty()) && message.role != Role::User {
        return Err(invalid("Chat media input requires a user message"));
    }
    let content = if message.images.is_empty() && message.documents.is_empty() {
        serde_json::Value::String(message.content.clone())
    } else {
        let mut parts = Vec::with_capacity(message.documents.len() + message.images.len() + 1);
        parts.extend(message.documents.iter().map(|document| {
            serde_json::json!({
                "type":"file",
                "file":{
                    "filename":document.filename(),
                    "file_data":crate::vocab::document_data_url(document),
                }
            })
        }));
        if !message.content.is_empty() {
            parts.push(serde_json::json!({"type":"text","text":message.content}));
        }
        parts.extend(message.images.iter().map(|image| {
            serde_json::json!({
                "type":"image_url",
                "image_url":{"url":crate::vocab::image_data_url(image),"detail":image_detail}
            })
        }));
        serde_json::Value::Array(parts)
    };
    let mut value = serde_json::json!({"role":role,"content":content});
    if let Some(calls) = &message.tool_calls {
        value["tool_calls"] = serde_json::Value::Array(
            calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id":call.id,"type":"function",
                        "function":{"name":call.name,"arguments":call.arguments}
                    })
                })
                .collect(),
        );
    }
    if message.role == Role::Tool {
        let call_id = message
            .tool_call_id
            .as_ref()
            .ok_or_else(|| invalid("Chat tool message has no tool_call_id"))?;
        value["tool_call_id"] = serde_json::json!(call_id);
    }
    Ok(value)
}

fn chat_tool(tool: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "type":"function",
        "function":{
            "name":tool.name,
            "description":tool.description,
            "parameters":tool.parameters,
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod image_tests {
    use super::*;

    #[test]
    fn chat_user_images_use_image_url_data_parts_after_text() {
        let image = crate::ChatImage::new(
            heycode_core::AttachmentMediaType::new("image/jpeg").unwrap(),
            vec![1, 2, 3],
        )
        .unwrap();
        let value = chat_message(&ChatMessage::user_with_images("describe", vec![image])).unwrap();
        assert_eq!(
            value["content"][0],
            serde_json::json!({
                "type":"text","text":"describe"
            })
        );
        assert_eq!(
            value["content"][1],
            serde_json::json!({
                "type":"image_url",
                "image_url":{"url":"data:image/jpeg;base64,AQID","detail":"auto"}
            })
        );
    }

    #[test]
    fn chat_user_pdf_uses_file_part_before_text() {
        let document = crate::ChatDocument::new(
            heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
            "guide.pdf",
            b"%PDF-".to_vec(),
        )
        .unwrap();
        let value = chat_message(&ChatMessage::user_with_media(
            "summarize",
            Vec::new(),
            vec![document],
        ))
        .unwrap();
        assert_eq!(
            value["content"][0],
            serde_json::json!({
                "type":"file",
                "file":{"filename":"guide.pdf","file_data":"data:application/pdf;base64,JVBERi0="}
            })
        );
        assert_eq!(
            value["content"][1],
            serde_json::json!({"type":"text","text":"summarize"})
        );
    }
}

enum ChatPhase {
    Read(heycode_http::SseEventStream, Box<ChatParser>),
    Done,
}

#[derive(Clone, Copy)]
struct ChatResponsePolicy {
    reasoning_continuation: ChatReasoningContinuation,
    url_citations: bool,
    max_server_tool_calls: Option<u32>,
}

impl Default for ChatResponsePolicy {
    fn default() -> Self {
        Self {
            reasoning_continuation: ChatReasoningContinuation::None,
            url_citations: false,
            max_server_tool_calls: None,
        }
    }
}

async fn drive_chat(
    phase: ChatPhase,
) -> Option<(Vec<Result<InferenceEvent, LlmError>>, ChatPhase)> {
    match phase {
        ChatPhase::Read(mut events, mut parser) => match events.next().await {
            Some(Ok(event)) => {
                let output = parser.event(event);
                let next = if parser.terminal {
                    ChatPhase::Done
                } else {
                    ChatPhase::Read(events, parser)
                };
                Some((output, next))
            }
            Some(Err(error)) => {
                let error = if parser.provider == crate::OpenRouterProvider::NAME {
                    crate::openrouter::classify_transport_error(error)
                } else {
                    map_transport_error(error)
                };
                Some((vec![Err(error)], ChatPhase::Done))
            }
            None => Some((parser.finish(), ChatPhase::Done)),
        },
        ChatPhase::Done => None,
    }
}

#[derive(Default)]
struct ChatToolState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    announced: bool,
}

struct ChatParser {
    provider: String,
    model: String,
    response_id: Option<String>,
    started: bool,
    content: String,
    reasoning: String,
    reasoning_field: Option<&'static str>,
    reasoning_details: Vec<serde_json::Value>,
    annotations: Vec<serde_json::Value>,
    tools: BTreeMap<u32, ChatToolState>,
    pending_finish: Option<FinishReason>,
    usage: Option<TokenUsage>,
    cache_usage: Option<heycode_core::ProviderCacheUsage>,
    web_search_requests: Option<u32>,
    policy: ChatResponsePolicy,
    terminal: bool,
}

impl ChatParser {
    fn new(provider: String, model: String, policy: ChatResponsePolicy) -> Self {
        Self {
            provider,
            model,
            response_id: None,
            started: false,
            content: String::new(),
            reasoning: String::new(),
            reasoning_field: None,
            reasoning_details: Vec::new(),
            annotations: Vec::new(),
            tools: BTreeMap::new(),
            pending_finish: None,
            usage: None,
            cache_usage: None,
            web_search_requests: None,
            policy,
            terminal: false,
        }
    }

    fn event(&mut self, event: heycode_http::SseEvent) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        let result = if event.data.trim() == "[DONE]" {
            self.finalize()
        } else {
            self.parse_frame(&event.data)
        };
        match result {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => {
                self.terminal = true;
                vec![Err(error)]
            }
        }
    }

    fn parse_frame(&mut self, data: &str) -> Result<Vec<InferenceEvent>, LlmError> {
        let value: serde_json::Value = serde_json::from_str(data)
            .map_err(|error| invalid(format!("Chat event is not JSON: {error}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| invalid("Chat event must be an object"))?;
        if let Some(error) = object.get("error").and_then(serde_json::Value::as_object) {
            let code = error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .or_else(|| error.get("type").and_then(serde_json::Value::as_str));
            return Err(crate::retry::provider_event_error(
                crate::ProviderErrorClass::Server,
                code,
            ));
        }
        let id = required_nonempty_str(object, "id")?;
        if self.response_id.as_ref().is_some_and(|known| known != id) {
            return Err(invalid("Chat response id changed mid-stream"));
        }
        if self.response_id.is_none() {
            self.response_id = Some(id.to_owned());
        }
        let usage_frame = object.get("usage").is_some_and(|value| !value.is_null());
        let mut output = Vec::new();
        let choices = object
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("Chat `choices` must be an array"))?;
        if choices.len() > 1 {
            return Err(invalid(
                "Chat coding-agent stream must contain at most one choice",
            ));
        }
        if let Some(choice) = choices.first() {
            let choice = choice
                .as_object()
                .ok_or_else(|| invalid("Chat choice must be an object"))?;
            if required_u64(choice, "index")? != 0 {
                return Err(invalid("Chat coding-agent choice index must be zero"));
            }
            output.extend(self.ensure_started()?);
            let delta = choice
                .get("delta")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| invalid("Chat choice delta must be an object"))?;
            let has_late_delta = delta.get("content").is_some_and(|value| {
                value.as_str().is_none_or(|content| !content.is_empty()) && !value.is_null()
            }) || delta
                .get("reasoning_content")
                .is_some_and(|value| !value.is_null())
                || delta.get("reasoning").is_some_and(|value| !value.is_null())
                || delta
                    .get("reasoning_details")
                    .is_some_and(|value| !value.is_null())
                || delta
                    .get("tool_calls")
                    .is_some_and(|value| !value.is_null())
                || delta
                    .get("annotations")
                    .is_some_and(|value| !value.is_null());
            if self.pending_finish.is_some() && has_late_delta {
                return Err(invalid("Chat delta arrived after finish_reason"));
            }
            if let Some(text) = optional_str(delta, "content")?
                && !text.is_empty()
            {
                self.content.push_str(text);
                output.push(InferenceEvent::TextDelta(text.to_owned()));
            }
            let reasoning_content = optional_str(delta, "reasoning_content")?;
            let reasoning = optional_str(delta, "reasoning")?;
            if reasoning_content.is_some_and(|value| !value.is_empty())
                && reasoning.is_some_and(|value| !value.is_empty())
            {
                return Err(invalid("Chat delta cannot carry both reasoning aliases"));
            }
            if let Some((field, reasoning)) = reasoning_content
                .filter(|value| !value.is_empty())
                .map(|value| ("reasoning_content", value))
                .or_else(|| {
                    reasoning
                        .filter(|value| !value.is_empty())
                        .map(|value| ("reasoning", value))
                })
            {
                if self.reasoning_field.is_some_and(|known| known != field) {
                    return Err(invalid("Chat reasoning alias changed mid-stream"));
                }
                self.reasoning_field = Some(field);
                self.reasoning.push_str(reasoning);
                output.push(InferenceEvent::ReasoningDelta(reasoning.to_owned()));
            }
            if let Some(details) = delta
                .get("reasoning_details")
                .filter(|value| !value.is_null())
            {
                let details = details
                    .as_array()
                    .ok_or_else(|| invalid("Chat reasoning_details must be an array"))?;
                if self.reasoning_details.len().saturating_add(details.len()) > 1024 {
                    return Err(invalid("Chat reasoning_details exceeds the item limit"));
                }
                for detail in details {
                    validate_reasoning_detail(detail)?;
                    // Structured public reasoning is a display stream as well as
                    // lossless continuation state. Never display opaque data.
                    if reasoning_content.is_none_or(str::is_empty)
                        && reasoning.is_none_or(str::is_empty)
                    {
                        let visible =
                            match detail.get("type").and_then(serde_json::Value::as_str) {
                                Some("reasoning.text") => detail.get("text"),
                                Some("reasoning.summary") => detail.get("summary"),
                                _ => None,
                            }
                            .and_then(serde_json::Value::as_str);
                        if let Some(text) = visible.filter(|text| !text.is_empty()) {
                            output.push(InferenceEvent::ReasoningDelta(text.to_owned()));
                        } else if detail.get("type").and_then(serde_json::Value::as_str)
                            == Some("reasoning.encrypted")
                            && self.reasoning_details.is_empty()
                        {
                            // An empty delta signals observed reasoning activity
                            // without manufacturing readable reasoning content.
                            output.push(InferenceEvent::ReasoningDelta(String::new()));
                        }
                    }
                    self.reasoning_details.push(detail.clone());
                }
            }
            if let Some(calls) = delta.get("tool_calls").filter(|value| !value.is_null()) {
                let calls = calls
                    .as_array()
                    .ok_or_else(|| invalid("Chat `tool_calls` must be an array"))?;
                for call in calls {
                    output.push(self.tool_delta(call)?);
                }
            }
            if let Some(annotations) = delta.get("annotations").filter(|value| !value.is_null()) {
                if !self.policy.url_citations {
                    return Err(invalid(
                        "Chat response annotations have no configured route dialect",
                    ));
                }
                let annotations = annotations
                    .as_array()
                    .ok_or_else(|| invalid("Chat annotations must be an array"))?;
                if self.annotations.len().saturating_add(annotations.len()) > 256 {
                    return Err(invalid("Chat annotations exceed the item limit"));
                }
                for annotation in annotations {
                    output.push(self.url_citation(annotation)?);
                }
            }
            if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
                let reason = reason
                    .as_str()
                    .ok_or_else(|| invalid("Chat finish_reason must be a string"))?;
                let reason = match reason {
                    "tool_calls" => FinishReason::ToolCalls,
                    "length" => FinishReason::Length,
                    _ => FinishReason::Stop,
                };
                if let Some(previous) = self.pending_finish {
                    if previous != reason || !usage_frame || !usage_only_finish_delta(delta) {
                        return Err(invalid(
                            "Chat stream repeated finish_reason outside one matching usage-only frame",
                        ));
                    }
                } else {
                    self.pending_finish = Some(reason);
                }
            }
        }
        if let Some(usage) = object.get("usage").filter(|value| !value.is_null()) {
            if self.usage.is_some() {
                return Err(invalid("Chat stream supplied usage twice"));
            }
            let usage = usage
                .as_object()
                .ok_or_else(|| invalid("Chat usage must be an object"))?;
            self.usage = Some(TokenUsage {
                prompt_tokens: required_u64(usage, "prompt_tokens")?,
                completion_tokens: required_u64(usage, "completion_tokens")?,
            });
            self.cache_usage = parse_chat_cache_usage(usage)?;
            self.web_search_requests = parse_web_search_requests(usage, self.policy)?;
        }
        Ok(output)
    }

    fn ensure_started(&mut self) -> Result<Vec<InferenceEvent>, LlmError> {
        if self.started {
            return Ok(Vec::new());
        }
        let response_id = self
            .response_id
            .clone()
            .ok_or_else(|| invalid("Chat stream has no response id"))?;
        self.started = true;
        Ok(vec![
            InferenceEvent::ResponseStarted {
                response_id: response_id.clone(),
            },
            InferenceEvent::ItemStarted {
                output_index: 0,
                item_id: format!("{response_id}/choice/0"),
                kind: StreamItemKind::Message,
            },
        ])
    }

    fn url_citation(&mut self, value: &serde_json::Value) -> Result<InferenceEvent, LlmError> {
        let normalized = parse_url_citation(value)?;
        self.annotations.push(value.clone());
        Ok(InferenceEvent::Citation {
            output_index: 0,
            citation: normalized,
        })
    }

    fn tool_delta(&mut self, value: &serde_json::Value) -> Result<InferenceEvent, LlmError> {
        let call = value
            .as_object()
            .ok_or_else(|| invalid("Chat tool call fragment must be an object"))?;
        let index = required_u32(call, "index")?;
        let id = optional_str(call, "id")?.map(str::to_owned);
        let function = call.get("function").and_then(serde_json::Value::as_object);
        let name = function
            .map(|function| optional_str(function, "name"))
            .transpose()?
            .flatten()
            .map(str::to_owned);
        let arguments = function
            .map(|function| optional_str(function, "arguments"))
            .transpose()?
            .flatten()
            .unwrap_or("")
            .to_owned();
        let state = self.tools.entry(index).or_default();
        if let (Some(known), Some(incoming)) = (&state.id, &id)
            && known != incoming
        {
            return Err(invalid("Chat tool call id changed across fragments"));
        }
        if let (Some(known), Some(incoming)) = (&state.name, &name)
            && known != incoming
        {
            return Err(invalid("Chat tool name changed across fragments"));
        }
        if state.id.is_none() {
            state.id = id;
        }
        if state.name.is_none() {
            state.name = name;
        }
        if !state.announced && (state.id.is_none() || state.name.is_none()) {
            return Err(invalid(
                "first Chat tool fragment must carry call id and function name",
            ));
        }
        state.arguments.push_str(&arguments);
        let first = !state.announced;
        state.announced = true;
        Ok(InferenceEvent::ToolCallDelta {
            output_index: index,
            id: first
                .then(|| state.id.clone())
                .flatten()
                .map(heycode_core::CallId::from_raw),
            name: first.then(|| state.name.clone()).flatten(),
            arguments_delta: arguments,
        })
    }

    fn finalize(&mut self) -> Result<Vec<InferenceEvent>, LlmError> {
        let finish = self
            .pending_finish
            .ok_or_else(|| invalid("Chat SSE ended before finish_reason"))?;
        if !self.annotations.is_empty()
            && self
                .web_search_requests
                .is_none_or(|requests| requests == 0)
        {
            return Err(invalid(
                "Chat URL citations require positive server web-search usage evidence",
            ));
        }
        let mut output = self.ensure_started()?;
        let response_id = self
            .response_id
            .clone()
            .ok_or_else(|| invalid("Chat stream has no response id"))?;
        let item_id = format!("{response_id}/choice/0");
        if !self.tools.is_empty()
            && !response_has_required_reasoning(
                &self.reasoning,
                self.reasoning_field,
                &self.reasoning_details,
                self.policy.reasoning_continuation,
            )
        {
            return Err(invalid(match self.policy.reasoning_continuation {
                ChatReasoningContinuation::ReasoningContent => {
                    "thinking tool-call response omitted required reasoning_content"
                }
                ChatReasoningContinuation::ReasoningOrDetails => {
                    "tool-call response omitted required reasoning or reasoning_details"
                }
                ChatReasoningContinuation::None
                | ChatReasoningContinuation::NativeStateWithOptionalReasoning => {
                    "tool-call response has invalid reasoning state"
                }
            }));
        }
        let mut message = serde_json::json!({
            "role":"assistant",
            "content": if self.content.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(self.content.clone())
            }
        });
        if !self.reasoning.is_empty() {
            message[self.reasoning_field.unwrap_or("reasoning_content")] =
                serde_json::json!(self.reasoning);
        }
        if !self.reasoning_details.is_empty() {
            message["reasoning_details"] = serde_json::Value::Array(self.reasoning_details.clone());
        }
        if !self.tools.is_empty() {
            let mut calls = Vec::new();
            for state in self.tools.values() {
                let id = state
                    .id
                    .as_ref()
                    .ok_or_else(|| invalid("Chat tool call finished without id"))?;
                let name = state
                    .name
                    .as_ref()
                    .ok_or_else(|| invalid("Chat tool call finished without name"))?;
                calls.push(serde_json::json!({
                    "id":id,"type":"function",
                    "function":{"name":name,"arguments":state.arguments}
                }));
            }
            message["tool_calls"] = serde_json::Value::Array(calls);
        }
        if !self.annotations.is_empty() {
            message["annotations"] = serde_json::Value::Array(self.annotations.clone());
        }
        output.push(InferenceEvent::ItemFinished {
            output_index: 0,
            item_id,
            kind: StreamItemKind::Message,
        });
        let state = ProviderStateItem::new(
            self.provider.clone(),
            self.model.clone(),
            ProviderProtocol::OpenAiChatCompletions,
            ProviderStateKind::ChatAssistantMessage,
            message,
        )
        .map_err(|error| invalid(error.to_string()))?;
        output.push(InferenceEvent::ProviderState(state));
        output.push(InferenceEvent::ResponseFinished {
            response_id,
            status: if finish == FinishReason::Length {
                "incomplete".to_owned()
            } else {
                "completed".to_owned()
            },
        });
        if let Some(requests) = self.web_search_requests.filter(|requests| *requests > 0) {
            output.push(InferenceEvent::ServerToolUsage(
                heycode_core::ServerToolUsage::new(
                    "web_search",
                    requests,
                    heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                    heycode_core::ServerToolUsageCost::Unknown,
                )
                .map_err(|error| invalid(error.to_string()))?,
            ));
        }
        if let Some(usage) = self.usage {
            output.push(InferenceEvent::Usage(usage));
        }
        if let Some(cache) = self.cache_usage {
            let metadata =
                heycode_core::ProviderResponseMetadata::new(Some(cache), Vec::new(), None)
                    .map_err(|error| invalid(error.to_string()))?;
            output.push(InferenceEvent::ResponseMetadata(metadata));
        }
        output.push(InferenceEvent::Finish(finish));
        self.terminal = true;
        Ok(output)
    }

    fn finish(mut self) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        match self.finalize() {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => vec![Err(error)],
        }
    }
}

fn validate_reasoning_detail(detail: &serde_json::Value) -> Result<(), LlmError> {
    let object = detail
        .as_object()
        .ok_or_else(|| invalid("Chat reasoning detail must be an object"))?;
    if object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        })
    {
        return Err(invalid("Chat reasoning detail type is invalid"));
    }
    let encoded = serde_json::to_vec(detail)
        .map_err(|_| invalid("Chat reasoning detail cannot be serialized"))?;
    if encoded.len() > 64 * 1024 {
        return Err(invalid("Chat reasoning detail exceeds the size limit"));
    }
    Ok(())
}

fn response_has_required_reasoning(
    reasoning: &str,
    reasoning_field: Option<&str>,
    reasoning_details: &[serde_json::Value],
    requirement: ChatReasoningContinuation,
) -> bool {
    match requirement {
        ChatReasoningContinuation::None
        | ChatReasoningContinuation::NativeStateWithOptionalReasoning => true,
        ChatReasoningContinuation::ReasoningContent => {
            !reasoning.is_empty() && reasoning_field == Some("reasoning_content")
        }
        ChatReasoningContinuation::ReasoningOrDetails => {
            !reasoning.is_empty() || !reasoning_details.is_empty()
        }
    }
}

fn validate_chat_annotations(
    config: &OpenAiChatCompletionsConfig,
    data: &serde_json::Value,
) -> Result<(), LlmError> {
    let Some(annotations) = data.get("annotations").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if !config.url_citations {
        return Err(invalid(
            "Chat provider state annotations have no configured route dialect",
        ));
    }
    let annotations = annotations
        .as_array()
        .ok_or_else(|| invalid("Chat provider state annotations must be an array"))?;
    if annotations.len() > 256 {
        return Err(invalid(
            "Chat provider state annotations exceed the item limit",
        ));
    }
    for annotation in annotations {
        parse_url_citation(annotation)?;
    }
    Ok(())
}

fn parse_url_citation(value: &serde_json::Value) -> Result<heycode_core::UrlCitation, LlmError> {
    let annotation = value
        .as_object()
        .ok_or_else(|| invalid("Chat annotation must be an object"))?;
    if required_nonempty_str(annotation, "type")? != "url_citation" {
        return Err(invalid(
            "Chat route received an unsupported annotation type",
        ));
    }
    if serde_json::to_vec(value)
        .map_err(|_| invalid("Chat annotation cannot be serialized"))?
        .len()
        > 64 * 1024
    {
        return Err(invalid("Chat annotation exceeds the size limit"));
    }
    let citation = annotation
        .get("url_citation")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("Chat url_citation must contain an object"))?;
    let url = required_nonempty_str(citation, "url")?;
    let title = optional_str(citation, "title")?
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let content = optional_str(citation, "content")?.filter(|content| !content.is_empty());
    let start_index = optional_u32(citation, "start_index")?;
    let end_index = optional_u32(citation, "end_index")?;
    heycode_core::UrlCitation::new(url, title, content, start_index, end_index)
        .map_err(|error| invalid(error.to_string()))
}

fn parse_web_search_requests(
    usage: &serde_json::Map<String, serde_json::Value>,
    policy: ChatResponsePolicy,
) -> Result<Option<u32>, LlmError> {
    let guide = parse_web_search_usage_alias(usage, "server_tool_use")?;
    let schema = parse_web_search_usage_alias(usage, "server_tool_use_details")?;
    let requests = match (guide, schema) {
        (None, None) => return Ok(None),
        (Some(guide), Some(schema)) if guide != schema => {
            return Err(invalid(
                "Chat server-tool usage aliases disagree on web_search_requests",
            ));
        }
        (Some(requests), _) | (_, Some(requests)) => requests,
    };
    if !policy.url_citations {
        return Err(invalid(
            "Chat server-tool usage has no configured route dialect",
        ));
    }
    if policy
        .max_server_tool_calls
        .is_some_and(|maximum| requests > maximum)
    {
        return Err(invalid(
            "Chat web_search_requests exceeds the resolved server-tool budget",
        ));
    }
    Ok(Some(requests))
}

fn usage_only_finish_delta(delta: &serde_json::Map<String, serde_json::Value>) -> bool {
    delta
        .keys()
        .all(|field| matches!(field.as_str(), "role" | "content"))
        && delta
            .get("role")
            .is_none_or(|value| value.as_str() == Some("assistant"))
        && delta
            .get("content")
            .is_none_or(|value| value.as_str() == Some(""))
}

fn parse_web_search_usage_alias(
    usage: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u32>, LlmError> {
    let Some(server_usage) = usage.get(field).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let server_usage = server_usage
        .as_object()
        .ok_or_else(|| invalid(format!("Chat {field} must be an object")))?;
    optional_u32(server_usage, "web_search_requests")
}

fn optional_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<&'a str>, LlmError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| invalid(format!("Chat `{field}` must be a string"))),
    }
}

fn optional_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u32>, LlmError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("Chat `{field}` must be a u32"))),
    }
}

fn required_nonempty_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, LlmError> {
    optional_str(object, field)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("Chat `{field}` must be a non-empty string")))
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid(format!("Chat `{field}` must be a u64")))
}

fn required_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u32, LlmError> {
    u32::try_from(required_u64(object, field)?)
        .map_err(|_| invalid(format!("Chat `{field}` must be a u32")))
}

fn parse_chat_cache_usage(
    usage: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<heycode_core::ProviderCacheUsage>, LlmError> {
    let Some(details) = usage
        .get("prompt_tokens_details")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let details = details
        .as_object()
        .ok_or_else(|| invalid("Chat prompt_tokens_details must be an object"))?;
    let Some(read) = details
        .get("cached_tokens")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let read = read
        .as_u64()
        .ok_or_else(|| invalid("Chat cached_tokens must be a u64"))?;
    let write = details
        .get("cache_write_tokens")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| invalid("Chat cache_write_tokens must be a u64"))
        })
        .transpose()?;
    let input = required_u64(usage, "prompt_tokens")?;
    let output = required_u64(usage, "completion_tokens")?;
    let mut cache = heycode_core::ProviderCacheUsage::new(input, output, read, write.unwrap_or(0))
        .map_err(|error| invalid(error.to_string()))?;
    if write.is_none() {
        cache = cache.with_unknown_cache_writes();
    }
    if let Some(reasoning) = usage
        .get("completion_tokens_details")
        .and_then(|value| value.get("reasoning_tokens"))
        .filter(|value| !value.is_null())
    {
        cache = cache
            .with_reasoning_tokens(
                reasoning
                    .as_u64()
                    .ok_or_else(|| invalid("Chat reasoning_tokens must be a u64"))?,
            )
            .map_err(|error| invalid(error.to_string()))?;
    }
    Ok(Some(cache))
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod cache_breakpoint_tests {
    use super::apply_anthropic_cache_breakpoints;
    use serde_json::json;

    #[test]
    fn breakpoints_preserve_prompt_text_tools_routing_and_opaque_reasoning() {
        let mut body = json!({"provider":{"order":["anthropic"],"allow_fallbacks":false},"tools":[{"type":"function","function":{"name":"read"}}],"messages":[
            {"role":"system","content":"standing\nexact"},
            {"role":"user","content":"question"},
            {"role":"assistant","content":null,"tool_calls":[{"id":"c1"}],"reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]},
            {"role":"tool","tool_call_id":"c1","content":"exact tool result"}
        ]});
        let original = body.clone();
        apply_anthropic_cache_breakpoints(&mut body);
        assert_eq!(body["provider"], original["provider"]);
        assert_eq!(body["tools"], original["tools"]);
        assert!(body.get("cache_control").is_none());
        assert_eq!(
            body["messages"][0]["content"][0]["text"],
            original["messages"][0]["content"]
        );
        assert_eq!(
            body["messages"][3]["content"][0]["text"],
            original["messages"][3]["content"]
        );
        assert_eq!(body["messages"][2], original["messages"][2]);
        assert_eq!(body["messages"][1], original["messages"][1]);
        assert_eq!(body["messages"][3]["tool_call_id"], "c1");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert_eq!(
            body["messages"][3]["content"][0]["cache_control"],
            json!({"type":"ephemeral"})
        );
        let once = body.clone();
        apply_anthropic_cache_breakpoints(&mut body);
        assert_eq!(body, once);
    }

    #[test]
    fn existing_markers_and_media_are_preserved_without_extra_breakpoints() {
        let mut existing = json!({"messages":[{"role":"user","content":[{"type":"text","text":"original","cache_control":{"type":"ephemeral","ttl":"1h"}}]}]});
        let original = existing.clone();
        apply_anthropic_cache_breakpoints(&mut existing);
        assert_eq!(existing, original);
        let mut media = json!({"messages":[{"role":"user","content":[{"type":"text","text":"caption"},{"type":"image_url","image_url":{"url":"https://example.test/image"}}]}]});
        let image = media["messages"][0]["content"][1].clone();
        apply_anthropic_cache_breakpoints(&mut media);
        assert_eq!(media["messages"][0]["content"][1], image);
        assert!(
            media["messages"][0]["content"][0]
                .get("cache_control")
                .is_some()
        );
    }
}
