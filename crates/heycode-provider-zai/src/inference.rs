//! Z.ai thinking/function-call continuation state (PZA03).
//!
//! GLM returns its reasoning as one `reasoning_content` **string** on the
//! assistant message, alongside `tool_calls` — not as a separate block type, an
//! opaque signature or a parallel array. The replay unit is therefore the whole
//! assistant message, which is exactly what C04's
//! `ProviderStateKind::ChatAssistantMessage` means, and that kind's validator
//! constrains only `role`, so `reasoning_content` survives verbatim. No
//! `heycode-core` vocabulary change is needed for Z.ai.
//!
//! Verbatim is the contract, not a convenience. Z.ai states that all
//! consecutive `reasoning_content` blocks must exactly match the sequence the
//! model generated, and that reordering or editing them degrades performance
//! and cache hits
//! (<https://docs.z.ai/guides/capabilities/thinking-mode>). This route
//! therefore replays provider state untouched, and — for the models Z.ai
//! documents as always thinking, see [`ZAI_ALWAYS_THINKING_MODELS`] — refuses a
//! tool-call turn that arrives without its thinking. Replay is verbatim for
//! every model; only the refusal is model-scoped, because Z.ai says the rest
//! decide for themselves whether to think and a thinking-free turn from one of
//! those is valid rather than lossy.
//!
//! # Request controls
//!
//! The route sends `thinking: {type: enabled, clear_thinking: false}` for
//! maintained reasoning-capable models, independently of the exact model's
//! effort vocabulary. This preserves reasoning on the general endpoint without
//! offering a disabled toggle to models that cannot disable thinking. Models
//! with unknown or unsupported reasoning receive neither field. Temperature
//! is validated against Z.ai's documented [0, 1] interval.
//!
//! # Stream shape
//!
//! Z.ai documents that `delta.tool_calls` carries an `index`, that
//! `function.arguments` arrives as string fragments concatenated per index, and
//! that `delta.reasoning_content` streams alongside them
//! (<https://docs.z.ai/guides/capabilities/stream-tool>); a finished turn is
//! appended back as the assistant message followed by
//! `{"role":"tool","tool_call_id":...,"content":...}`
//! (<https://docs.z.ai/guides/capabilities/function-calling>). Whether the first
//! tool-call fragment carries `id` and `type` is **not** stated there — the
//! shared Chat parser requires `id` and `function.name` on it, so a stream that
//! omitted them fails loudly rather than producing a call heycode cannot name.
//!
//! Z.ai's non-streaming response schema types `function.arguments` as an
//! *object* while its request schema and its own example code treat it as a
//! JSON-format *string*
//! (<https://docs.z.ai/api-reference/llm/chat-completion>). heycode only streams,
//! so it reads the string form the examples show and replays the string form the
//! request schema requires; the object typing is a documentation inconsistency,
//! not a shape this route produces.

use futures::StreamExt as _;
use heycode_http::HttpService;
use heycode_llm::{
    AuthenticationBinding, ChatReasoningContinuation, ChatReasoningWire, ChatRequest, ChunkStream,
    InferenceAdapter, InferenceEvent, InferenceInput, InferenceStream, LlmError, ModelDescriptor,
    OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig, Provider, ProviderDescriptor,
    ProviderInfo, ProviderProtocol, ReasoningEffortId, RequestDraft, ResolveError, ResolvedCall,
    Role,
};

use crate::catalog::zai_model_descriptor;
use crate::{ZAI_GLM_5_3, ZaiEndpoint, ZaiPlan, ZaiPlanKind, ZaiProfileError, ZaiProtocol};

/// Reasoning efforts this route advertises for
/// [`ZAI_REASONING_EFFORT_MODELS`], weakest first.
///
/// Source: <https://docs.z.ai/guides/llm/glm-5.3> — `reasoning_effort` takes
/// `low`, `high` or `max` for GLM-5.3, the model
/// [`ZAI_GLM_5_3`] makes this crate's default. The endpoint's own parameter
/// enum is wider (`minimal`, `medium`, `xhigh` also appear at
/// <https://docs.z.ai/api-reference/llm/chat-completion>), but Z.ai names no
/// model those extra values are valid for, so advertising them would offer a
/// choice heycode cannot say works anywhere.
pub const ZAI_REASONING_EFFORTS: [&str; 3] = ["low", "high", "max"];

/// Models Z.ai documents as accepting `reasoning_effort`.
///
/// Source: <https://docs.z.ai/api-reference/llm/chat-completion> — the field is
/// supported by GLM-5.2 and above. The maintained catalog currently names
/// GLM-5.2, GLM-5.3 and GLM-5.3-Flash in that range; older reasoning-capable
/// models still preserve `reasoning_content` but receive no effort field.
pub const ZAI_REASONING_EFFORT_MODELS: [&str; 3] = ["glm-5.2", "glm-5.3", "glm-5.3-flash"];

/// Models Z.ai documents as thinking on **every** enabled turn.
///
/// Source: <https://docs.z.ai/api-reference/llm/chat-completion> — the
/// `thinking.type` description says `GLM-5.3` and `GLM-5.3-FLASH` "can only be
/// enabled", that "GLM-4.7 and GLM-4.5V will think compulsorily", and that
/// "GLM-5.2 GLM-5.1 GLM-5 GLM-4.6 GLM-4.5 and others will automatically
/// determine whether to think".
///
/// That last clause is why this list exists. Demanding `reasoning_content` back
/// from a model Z.ai says *chooses* whether to think turns a valid thinking-free
/// tool turn into a hard failure, so the strict requirement is applied only
/// where Z.ai says thinking is unconditional.
///
/// Only the ids Z.ai names are listed. "GLM-4.7" may or may not mean the whole
/// series; the ambiguity resolves toward the weaker claim, because a model
/// wrongly listed here has valid turns rejected while one wrongly left out only
/// loses a safety net.
pub const ZAI_ALWAYS_THINKING_MODELS: [&str; 4] =
    ["glm-5.3", "glm-5.3-flash", "glm-4.7", "glm-4.5v"];

/// Effort used when a caller resolves none.
///
/// Source: <https://docs.z.ai/guides/llm/glm-5.3> — "Default is `max`". This
/// is Z.ai's default rather than a heycode product choice.
pub const ZAI_DEFAULT_REASONING_EFFORT: &str = "max";

/// Whether a plan's endpoint keeps replayed `reasoning_content`.
///
/// Z.ai's `thinking.clear_thinking` defaults to `true` — "Controls whether to
/// clear `reasoning_content` from previous conversation turns"
/// (<https://docs.z.ai/api-reference/llm/chat-completion>) — and Preserved
/// Thinking "is enabled by default on the Coding Plan endpoint and disabled by
/// default on the standard API endpoint"
/// (<https://docs.z.ai/guides/capabilities/thinking-mode>).
///
/// Endpoint defaults and explicit adapter preservation are distinct evidence.
/// The inference route enables preservation for reasoning-capable models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiPreservedThinking {
    /// The endpoint preserves replayed `reasoning_content` with no extra
    /// request field. heycode's verbatim replay reaches the model intact.
    EndpointDefault,
    /// The adapter explicitly sends `thinking.clear_thinking=false`.
    ExplicitPreservation,
    /// The endpoint clears prior-turn `reasoning_content` unless the request
    /// carries `thinking.clear_thinking: false`. This describes the endpoint
    /// default, not the configured inference adapter.
    RequiresClearThinkingFalse,
}

impl ZaiPreservedThinking {
    /// Whether replayed thinking actually survives to the model today.
    #[must_use]
    pub fn survives_replay(self) -> bool {
        matches!(self, Self::EndpointDefault | Self::ExplicitPreservation)
    }

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EndpointDefault => "endpoint-default",
            Self::ExplicitPreservation => "explicit-preservation",
            Self::RequiresClearThinkingFalse => "requires-clear-thinking-false",
        }
    }
}

/// Provenance of the `Authorization: Bearer` header this route sends.
///
/// Z.ai documents that header for the general endpoint
/// (<https://docs.z.ai/guides/develop/http/introduction>) and publishes none at
/// all for the GLM Coding Plan inference endpoints — its integration guides
/// only say to enter the API key in each tool
/// (<https://docs.z.ai/devpack/quick-start>, <https://docs.z.ai/devpack/tool/others>).
/// The shared Chat adapter sends `Authorization: Bearer` either way, so the
/// Coding Plan route ships an **unverified** header; naming it as unverified is
/// the difference between a known risk and a guess dressed as a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiAuthHeaderEvidence {
    /// Z.ai documents `Authorization: Bearer YOUR_API_KEY` for this endpoint.
    Documented,
    /// Z.ai documents no request header for this endpoint.
    Undocumented,
}

impl ZaiAuthHeaderEvidence {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Documented => "documented",
            Self::Undocumented => "undocumented",
        }
    }
}

/// What heycode can and cannot guarantee about one plan's thinking replay.
///
/// Every field is non-secret: plans, endpoints and evidence grades are public
/// identifiers, and there is no field a key could occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZaiThinkingReport {
    /// Plan this route serves.
    pub plan: ZaiPlan,
    /// Base URL requests go to.
    pub endpoint: &'static str,
    /// Whether replayed `reasoning_content` survives at that endpoint.
    pub preserved_thinking: ZaiPreservedThinking,
    /// Provenance of the request auth header.
    pub auth_header: ZaiAuthHeaderEvidence,
}

impl ZaiThinkingReport {
    /// One safe line naming the plan, endpoint and both evidence grades.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} · {} · reasoning_effort {} of {:?} · thinking replay {}",
            self.plan.as_str(),
            self.endpoint,
            ZAI_DEFAULT_REASONING_EFFORT,
            ZAI_REASONING_EFFORTS,
            self.preserved_thinking.as_str(),
        );
        if !self.preserved_thinking.survives_replay() {
            line.push_str(
                " — replayed reasoning_content is cleared server-side until heycode can send thinking.clear_thinking=false",
            );
        }
        if self.auth_header == ZaiAuthHeaderEvidence::Undocumented {
            line.push_str(
                " — Z.ai documents no request header for this endpoint; `Authorization: Bearer` is unverified here",
            );
        }
        line
    }
}

/// Failures constructing a Z.ai inference route.
///
/// Every field is non-secret: no variant carries a key, and the shared route's
/// own [`std::fmt::Debug`] redacts one.
#[derive(Debug)]
pub enum ZaiInferenceError {
    /// Z.ai publishes no base URL for this plan and protocol.
    Endpoint(ZaiProfileError),
    /// A compiled reasoning effort id stopped satisfying the request grammar.
    ReasoningEffort(ResolveError),
    /// The shared Chat Completions route rejected this configuration.
    Route(LlmError),
}

impl std::fmt::Display for ZaiInferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Endpoint(error) => write!(formatter, "Z.ai inference endpoint: {error}"),
            Self::ReasoningEffort(error) => {
                write!(formatter, "Z.ai reasoning effort vocabulary: {error}")
            }
            Self::Route(error) => write!(formatter, "Z.ai Chat Completions route: {error}"),
        }
    }
}

impl std::error::Error for ZaiInferenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Endpoint(error) => Some(error),
            Self::ReasoningEffort(error) => Some(error),
            Self::Route(error) => Some(error),
        }
    }
}

/// One plan's Z.ai Chat Completions inference route.
///
/// The plan is part of the type for the same reason it is on
/// [`crate::ZaiProfile`]: a route is where a key is actually spent, so the
/// endpoint it dispatches to may not be a runtime choice a caller can get
/// wrong.
///
/// Only OpenAI Chat Completions is served. It is the one protocol both plans
/// document, and it is the protocol [`ProviderStateKind::ChatAssistantMessage`]
/// pairs with in the C04 vocabulary — the Anthropic and Responses endpoints
/// carry different state kinds and are not this row's work.
///
/// [`ProviderStateKind::ChatAssistantMessage`]: heycode_llm::ProviderStateKind::ChatAssistantMessage
pub struct ZaiInference<P: ZaiPlanKind> {
    always_thinking: OpenAiChatCompletionsAdapter,
    optional_thinking: OpenAiChatCompletionsAdapter,
    without_reasoning_effort: OpenAiChatCompletionsAdapter,
    without_thinking: OpenAiChatCompletionsAdapter,
    credential: heycode_llm::RouteCredential,
    reference: String,
    endpoint: ZaiEndpoint<P>,
    default_model: String,
}

impl<P: ZaiPlanKind> ZaiInference<P> {
    /// Build this plan's documented Chat Completions route.
    ///
    /// `default_model` overrides [`ZAI_GLM_5_3`]; the model a caller resolves
    /// still reaches the wire unchanged.
    ///
    /// # Errors
    /// [`ZaiInferenceError::Endpoint`] when Z.ai publishes no Chat Completions
    /// base URL for this plan, [`ZaiInferenceError::ReasoningEffort`] when a
    /// compiled effort id is rejected, and [`ZaiInferenceError::Route`] for an
    /// empty key or an unusable endpoint.
    pub fn new(
        http: HttpService,
        api_key: impl Into<String>,
        default_model: Option<String>,
    ) -> Result<Self, ZaiInferenceError> {
        Self::with_credential(
            http,
            heycode_llm::RouteCredential::fixed(api_key),
            default_model,
        )
    }

    /// Bind a rotating credential to this plan's exact documented endpoint.
    ///
    /// # Errors
    /// Invalid route configuration or the other plan's credential reference.
    pub fn with_credential(
        http: HttpService,
        credential: heycode_llm::RouteCredential,
        default_model: Option<String>,
    ) -> Result<Self, ZaiInferenceError> {
        let reference = credential
            .route()
            .map_or(P::DEFAULT_CREDENTIAL_REFERENCE, |r| r.as_str())
            .to_owned();
        if reference == P::FOREIGN_CREDENTIAL_REFERENCE {
            return Err(ZaiInferenceError::Route(LlmError::Transport(
                "Z.ai credential belongs to the other plan".into(),
            )));
        }
        let endpoint = ZaiEndpoint::<P>::documented(ZaiProtocol::OpenAiChatCompletions)
            .map_err(ZaiInferenceError::Endpoint)?;
        let mut efforts = Vec::with_capacity(ZAI_REASONING_EFFORTS.len());
        for effort in ZAI_REASONING_EFFORTS {
            efforts
                .push(ReasoningEffortId::new(effort).map_err(ZaiInferenceError::ReasoningEffort)?);
        }
        let default_effort = ReasoningEffortId::new(ZAI_DEFAULT_REASONING_EFFORT)
            .map_err(ZaiInferenceError::ReasoningEffort)?;
        // `&'static str`, so the closure below borrows nothing from `endpoint`.
        let base_url = endpoint.base_url();
        let base_config = || {
            OpenAiChatCompletionsConfig::with_credential(
                provider_descriptor::<P>(),
                base_url,
                credential.clone(),
            )
        };
        let reasoning_config = |continuation| {
            base_config()
                .with_preserved_thinking()
                .with_reasoning(
                    efforts.clone(),
                    Some(default_effort.clone()),
                    // Z.ai spells the field `reasoning_effort` at the top level.
                    ChatReasoningWire::ScalarEffort,
                )
                .with_reasoning_continuation(continuation)
        };
        Ok(Self {
            // Z.ai requires the historical `reasoning_content` to come back
            // "complete, unmodified and correctly ordered". Where thinking is
            // unconditional, a tool-call turn that reaches this route without
            // it — or a response that produced tool calls without it — fails
            // loudly rather than continuing a conversation whose reasoning has
            // already been lost.
            always_thinking: OpenAiChatCompletionsAdapter::new(
                reasoning_config(ChatReasoningContinuation::ReasoningContent),
                http.clone(),
            )
            .map_err(ZaiInferenceError::Route)?,
            // Everywhere else the model decides whether to think, so a missing
            // `reasoning_content` is a legitimate turn and not a lost one.
            // Replay stays verbatim; only the enforcement is off.
            optional_thinking: OpenAiChatCompletionsAdapter::new(
                reasoning_config(ChatReasoningContinuation::None),
                http.clone(),
            )
            .map_err(ZaiInferenceError::Route)?,
            // Z.ai documents `reasoning_effort` only for GLM-5.2 and above.
            // Older models may still think and must still replay their exact
            // assistant state, but sending `max` to them would invent support.
            without_reasoning_effort: OpenAiChatCompletionsAdapter::new(
                base_config().with_preserved_thinking(),
                http.clone(),
            )
            .map_err(ZaiInferenceError::Route)?,
            without_thinking: OpenAiChatCompletionsAdapter::new(base_config(), http)
                .map_err(ZaiInferenceError::Route)?,
            credential,
            reference,
            endpoint,
            default_model: default_model.unwrap_or_else(|| ZAI_GLM_5_3.to_owned()),
        })
    }

    /// The route that governs `model`'s replay contract.
    fn adapter(&self, model: &str) -> &OpenAiChatCompletionsAdapter {
        if !zai_model_descriptor(model)
            .is_some_and(|m| m.capabilities.reasoning == heycode_llm::CapabilitySupport::Supported)
        {
            &self.without_thinking
        } else if !ZAI_REASONING_EFFORT_MODELS.contains(&model) {
            &self.without_reasoning_effort
        } else if ZAI_ALWAYS_THINKING_MODELS.contains(&model) {
            &self.always_thinking
        } else {
            &self.optional_thinking
        }
    }

    /// Whether provider-local validation must enforce continuation because the
    /// shared route cannot tie it to a documented reasoning-effort field.
    fn requires_local_continuation(model: &str) -> bool {
        Self::requires_replayed_thinking(model) && !ZAI_REASONING_EFFORT_MODELS.contains(&model)
    }

    /// Whether Z.ai documents `model` as thinking on every enabled turn, and so
    /// whether this route demands `reasoning_content` back from it.
    #[must_use]
    pub fn requires_replayed_thinking(model: &str) -> bool {
        ZAI_ALWAYS_THINKING_MODELS.contains(&model)
    }

    /// Plan this route serves.
    #[must_use]
    pub fn plan(&self) -> ZaiPlan {
        P::PLAN
    }

    /// Base URL requests go to.
    #[must_use]
    pub fn endpoint(&self) -> &'static str {
        self.endpoint.base_url()
    }

    /// What this plan's endpoint does with replayed thinking, and where its
    /// auth header comes from.
    #[must_use]
    pub fn thinking_report(&self) -> ZaiThinkingReport {
        ZaiThinkingReport {
            plan: P::PLAN,
            endpoint: self.endpoint.base_url(),
            preserved_thinking: ZaiPreservedThinking::ExplicitPreservation,
            auth_header: P::AUTH_HEADER,
        }
    }
}

impl<P: ZaiPlanKind> std::fmt::Debug for ZaiInference<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ZaiInference")
            .field("plan", &P::PLAN)
            .field("endpoint", &self.endpoint.base_url())
            .field("default_model", &self.default_model)
            .field("preserved_thinking", &P::PRESERVED_THINKING)
            .field("auth_header", &P::AUTH_HEADER)
            .finish()
    }
}

fn validate_local_reasoning_tool_inputs(inputs: &[InferenceInput]) -> Result<(), ResolveError> {
    for input in inputs {
        match input {
            InferenceInput::Message(message)
                if message.role == Role::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()) =>
            {
                return Err(missing_local_reasoning_state());
            }
            InferenceInput::ProviderState(state) => {
                if !state_has_required_reasoning(state.data()).map_err(|message| {
                    ResolveError::InvalidRequest {
                        field: "provider_state",
                        message: message.to_owned(),
                    }
                })? {
                    return Err(missing_local_reasoning_state());
                }
            }
            InferenceInput::Message(_) => {}
        }
    }
    Ok(())
}

fn state_has_required_reasoning(data: &serde_json::Value) -> Result<bool, &'static str> {
    let Some(tool_calls) = data.get("tool_calls").filter(|value| !value.is_null()) else {
        return Ok(true);
    };
    let calls = tool_calls
        .as_array()
        .ok_or("Z.ai assistant tool_calls must be an array")?;
    if calls.is_empty() {
        return Ok(true);
    }
    Ok(data
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reasoning| !reasoning.is_empty()))
}

fn missing_local_reasoning_state() -> ResolveError {
    ResolveError::InvalidRequest {
        field: "provider_state",
        message: "Z.ai thinking tool-call history requires complete reasoning_content".to_owned(),
    }
}

fn enforce_local_reasoning_response(stream: InferenceStream) -> InferenceStream {
    Box::pin(stream.scan(false, |terminal, event| {
        let output = if *terminal {
            None
        } else {
            match event {
                Ok(InferenceEvent::ProviderState(state)) => {
                    match state_has_required_reasoning(state.data()) {
                        Ok(true) => Some(Ok(InferenceEvent::ProviderState(state))),
                        Ok(false) => {
                            *terminal = true;
                            Some(Err(LlmError::InvalidResponse(
                                "Z.ai thinking tool-call response omitted required reasoning_content"
                                    .to_owned(),
                            )))
                        }
                        Err(message) => {
                            *terminal = true;
                            Some(Err(LlmError::InvalidResponse(message.to_owned())))
                        }
                    }
                }
                other => Some(other),
            }
        };
        std::future::ready(output)
    }))
}

impl<P: ZaiPlanKind + Send + Sync + 'static> Provider for ZaiInference<P> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: P::REGISTRY_NAME.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(&self.reference)
    }

    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor::<P>()
    }

    /// The PZA02 maintained row for `model`, or a conservative unknown
    /// descriptor. Nothing is invented for an id Z.ai does not publish.
    fn describe_model(&self, model: &str) -> ModelDescriptor {
        zai_model_descriptor(model).unwrap_or_else(|| ModelDescriptor::unknown(model))
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    // No `rate_limit_headers` override: no Z.ai page read for this row
    // publishes a rate-limit response header, and naming an unverified
    // `x-ratelimit-*` spelling would make `/usage` display a guess.

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        // This route advertises an inference adapter, so the legacy path must
        // fail loud rather than dispatch a request that bypasses the thinking
        // replay contract the adapter enforces (AGENTS §5).
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "Z.ai route dispatches through its inference adapter; the legacy chat path is not available"
                    .to_owned(),
            ))
        }))
    }
}

impl<P: ZaiPlanKind + Send + Sync> InferenceAdapter for ZaiInference<P> {
    fn descriptor(&self) -> ProviderDescriptor {
        provider_descriptor::<P>()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.credential.binding()
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, ResolveError> {
        self.adapter(&model.id).reasoning_effort_options(model)
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        if draft
            .temperature
            .is_some_and(|t| !t.is_finite() || !(0.0..=1.0).contains(&t))
        {
            return Err(ResolveError::InvalidRequest {
                field: "temperature",
                message: "Z.ai temperature must be between 0 and 1".into(),
            });
        }
        if Self::requires_local_continuation(&model.id) {
            validate_local_reasoning_tool_inputs(&draft.inputs)?;
        }
        // The canonical resolved id, not the requested one: an alias must not
        // be able to pick a weaker replay contract than its model's.
        self.adapter(&model.id).resolve(draft, model)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        let local_continuation = Self::requires_local_continuation(call.model());
        let adapter = self.adapter(call.model());
        let stream = InferenceAdapter::stream(adapter, call);
        if local_continuation {
            enforce_local_reasoning_response(stream)
        } else {
            stream
        }
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        let local_continuation = Self::requires_local_continuation(call.model());
        let adapter = self.adapter(call.model());
        let stream = InferenceAdapter::stream_cancellable(adapter, call, cancellation);
        if local_continuation {
            enforce_local_reasoning_response(stream)
        } else {
            stream
        }
    }
}

/// Registry identity for one plan's Chat Completions route.
fn provider_descriptor<P: ZaiPlanKind>() -> ProviderDescriptor {
    ProviderDescriptor {
        id: P::REGISTRY_NAME.to_owned(),
        display_name: P::DISPLAY_NAME.to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

/// Both plans' thinking-replay reports, general first.
///
/// Both configured adapters explicitly preserve thinking. Authentication
/// evidence remains plan-specific. Building a route is not required.
///
/// # Errors
/// [`ZaiProfileError::UndocumentedEndpoint`] if a plan ever stops publishing a
/// Chat Completions base URL. heycode reports that absence rather than borrowing
/// the other plan's URL.
pub fn zai_thinking_reports() -> Result<[ZaiThinkingReport; 2], ZaiProfileError> {
    Ok([
        report_for::<crate::General>()?,
        report_for::<crate::Coding>()?,
    ])
}

fn report_for<P: ZaiPlanKind>() -> Result<ZaiThinkingReport, ZaiProfileError> {
    Ok(ZaiThinkingReport {
        plan: P::PLAN,
        endpoint: ZaiEndpoint::<P>::documented(ZaiProtocol::OpenAiChatCompletions)?.base_url(),
        preserved_thinking: ZaiPreservedThinking::ExplicitPreservation,
        auth_header: P::AUTH_HEADER,
    })
}
