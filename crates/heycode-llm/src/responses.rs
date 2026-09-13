//! OpenAI Responses protocol adapter over the shared raw HTTP/SSE service.

use crate::inference::{ToolAdmission, resolve_request_with_tool_admission};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures::StreamExt as _;

use crate::{
    AuthenticationBinding, CallPurpose, ChatMessage, FinishReason, InferenceAdapter,
    InferenceEvent, InferenceInput, InferenceStream, InferenceTarget, LlmError, ModelDescriptor,
    NativeFeature, ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind,
    ReasoningEffortId, ReasoningEffortOptions, RequestDraft, ResolveError, ResolveSpec,
    ResolvedCall, Role, StreamItemKind, TokenUsage,
};

/// Reusable OpenAI Responses route configuration. Debug output is redacted.
#[derive(Clone)]
pub struct OpenAiResponsesConfig {
    provider: ProviderDescriptor,
    base_url: String,
    credential: crate::RouteCredential,
    credential_header: ResponsesCredentialHeader,
    reasoning_efforts: Vec<ReasoningEffortId>,
    default_reasoning_effort: Option<ReasoningEffortId>,
    default_max_output_tokens: Option<u64>,
    retry_spec: crate::RetrySpec,
    tool_admission: ToolAdmission,
    continuation: ResponsesContinuation,
    provider_request_options: Vec<ResponsesProviderOptionWire>,
    server_tool_plans: Vec<ResponsesServerToolPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponsesCredentialHeader {
    BearerAuthorization,
    ApiKey,
}

#[derive(Debug, Clone)]
struct ResponsesProviderOptionWire {
    kind: String,
    members: Vec<(String, String)>,
}

/// Closed failure returned by provider-owned Responses server-tool
/// normalizers. No provider item body or configuration value crosses this
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResponsesServerToolFault {
    /// One configured definition or discriminator is structurally invalid.
    #[error("invalid Responses server-tool configuration")]
    InvalidConfiguration,
    /// One completed provider item does not satisfy its registered schema.
    #[error("invalid Responses server-tool item")]
    InvalidItem,
}

/// Safe normalized facts derived from one complete Responses output item.
///
/// A state-only result is intentional for client-executed output items such as
/// computer use: the shared parser still validates the registered
/// discriminator and retains exact provider state without misclassifying it as
/// a provider-executed call.
#[derive(Clone, PartialEq)]
pub struct ResponsesServerToolNormalization {
    call: Option<heycode_core::ServerToolCall>,
    result: Option<heycode_core::ServerToolResult>,
    citations: Vec<heycode_core::UrlCitation>,
}

impl ResponsesServerToolNormalization {
    /// Construct one normalized output-item projection.
    ///
    /// # Errors
    /// A call/result pair must use the same provider correlation id, and
    /// citations must remain valid bounded public URLs.
    pub fn new(
        call: Option<heycode_core::ServerToolCall>,
        result: Option<heycode_core::ServerToolResult>,
        citations: Vec<heycode_core::UrlCitation>,
    ) -> Result<Self, ResponsesServerToolFault> {
        if call
            .as_ref()
            .zip(result.as_ref())
            .is_some_and(|(call, result)| call.id() != result.call_id())
        {
            return Err(ResponsesServerToolFault::InvalidItem);
        }
        if citations
            .iter()
            .any(|citation| citation.validate().is_err())
        {
            return Err(ResponsesServerToolFault::InvalidItem);
        }
        Ok(Self {
            call,
            result,
            citations,
        })
    }

    /// Retain exact state without asserting a provider-executed call/result.
    #[must_use]
    pub const fn state_only() -> Self {
        Self {
            call: None,
            result: None,
            citations: Vec::new(),
        }
    }

    /// Optional normalized provider-executed call.
    #[must_use]
    pub const fn call(&self) -> Option<&heycode_core::ServerToolCall> {
        self.call.as_ref()
    }

    /// Optional normalized provider-executed result.
    #[must_use]
    pub const fn result(&self) -> Option<&heycode_core::ServerToolResult> {
        self.result.as_ref()
    }

    /// Public citations derived from this item.
    #[must_use]
    pub fn citations(&self) -> &[heycode_core::UrlCitation] {
        &self.citations
    }
}

impl std::fmt::Debug for ResponsesServerToolNormalization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponsesServerToolNormalization")
            .field("has_call", &self.call.is_some())
            .field("has_result", &self.result.is_some())
            .field("citation_count", &self.citations.len())
            .finish()
    }
}

/// Provider-owned completed-item normalizer used by the shared Responses
/// parser.
pub trait ResponsesServerToolNormalizer: Send + Sync {
    /// Exact completed `item.type` discriminator handled by this normalizer.
    fn output_item_type(&self) -> &'static str;

    /// Derive bounded inspection facts from one already route-tagged exact
    /// provider-state item.
    ///
    /// # Errors
    /// Malformed or mismatched provider items return only a closed fault.
    fn normalize(
        &self,
        state: &ProviderStateItem,
    ) -> Result<ResponsesServerToolNormalization, ResponsesServerToolFault>;
}

/// One exact request `tools[]` entry and all completed-item discriminators it
/// authorizes for parsing.
#[derive(Clone)]
pub struct ResponsesServerToolDefinition {
    request: serde_json::Value,
    normalizers: Vec<Arc<dyn ResponsesServerToolNormalizer>>,
    required_native_feature: Option<NativeFeature>,
}

impl ResponsesServerToolDefinition {
    /// Validate one exact request definition and its completed-item parsers.
    ///
    /// # Errors
    /// Definitions must be objects with a trimmed `type`; normalizer
    /// discriminators must be unique safe identifiers.
    pub fn new(
        request: serde_json::Value,
        normalizers: Vec<Arc<dyn ResponsesServerToolNormalizer>>,
    ) -> Result<Self, ResponsesServerToolFault> {
        let definition = Self {
            request,
            normalizers,
            required_native_feature: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    /// Require one existing shared native capability in addition to the
    /// provider-owned exact option gate.
    #[must_use]
    pub fn with_required_native_feature(mut self, feature: NativeFeature) -> Self {
        self.required_native_feature = Some(feature);
        self
    }

    /// Exact request definition.
    #[must_use]
    pub const fn request(&self) -> &serde_json::Value {
        &self.request
    }

    fn validate(&self) -> Result<(), ResponsesServerToolFault> {
        let request = self
            .request
            .as_object()
            .ok_or(ResponsesServerToolFault::InvalidConfiguration)?;
        let request_type = request
            .get("type")
            .and_then(serde_json::Value::as_str)
            .filter(|value| safe_server_tool_identifier(value))
            .ok_or(ResponsesServerToolFault::InvalidConfiguration)?;
        if request_type == "function" || self.normalizers.is_empty() {
            return Err(ResponsesServerToolFault::InvalidConfiguration);
        }
        let mut output_types = BTreeSet::new();
        for normalizer in &self.normalizers {
            let output_type = normalizer.output_item_type();
            if !safe_server_tool_identifier(output_type) || !output_types.insert(output_type) {
                return Err(ResponsesServerToolFault::InvalidConfiguration);
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for ResponsesServerToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponsesServerToolDefinition")
            .field(
                "request_type",
                &self.request.get("type").and_then(serde_json::Value::as_str),
            )
            .field("normalizer_count", &self.normalizers.len())
            .field("required_native_feature", &self.required_native_feature)
            .finish()
    }
}

/// Exact provider-option-gated hosted/server-tool plan for Responses.
#[derive(Clone)]
pub struct ResponsesServerToolPlan {
    option_kind: String,
    definitions: Vec<ResponsesServerToolDefinition>,
}

impl ResponsesServerToolPlan {
    /// Validate one durable option kind and its complete definition allowlist.
    ///
    /// # Errors
    /// Invalid/duplicate definitions or output discriminators fail before an
    /// adapter can publish.
    pub fn new(
        option_kind: impl Into<String>,
        definitions: Vec<ResponsesServerToolDefinition>,
    ) -> Result<Self, ResponsesServerToolFault> {
        let plan = Self {
            option_kind: option_kind.into(),
            definitions,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Mint the exact durable selection after the provider owner has applied
    /// its model/account capability gates.
    ///
    /// # Errors
    /// Empty, duplicate or non-allowlisted definitions fail without creating
    /// an option.
    pub fn provider_option(
        &self,
        provider: &str,
        selected: &[ResponsesServerToolDefinition],
    ) -> Result<heycode_core::ProviderRequestOption, ResponsesServerToolFault> {
        if selected.is_empty()
            || selected
                .iter()
                .any(|candidate| !self.has_definition(&candidate.request))
            || has_duplicate_values(selected.iter().map(|definition| &definition.request))
        {
            return Err(ResponsesServerToolFault::InvalidConfiguration);
        }
        heycode_core::ProviderRequestOption::new(
            provider,
            self.option_kind.clone(),
            serde_json::json!({
                "definitions":selected
                    .iter()
                    .map(|definition| definition.request.clone())
                    .collect::<Vec<_>>()
            }),
        )
        .map_err(|_| ResponsesServerToolFault::InvalidConfiguration)
    }

    fn validate(&self) -> Result<(), ResponsesServerToolFault> {
        if self.definitions.is_empty()
            || !safe_server_tool_identifier(&self.option_kind)
            || has_duplicate_values(
                self.definitions
                    .iter()
                    .map(|definition| &definition.request),
            )
        {
            return Err(ResponsesServerToolFault::InvalidConfiguration);
        }
        let mut output_types = BTreeSet::new();
        for definition in &self.definitions {
            definition.validate()?;
            for normalizer in &definition.normalizers {
                if !output_types.insert(normalizer.output_item_type()) {
                    return Err(ResponsesServerToolFault::InvalidConfiguration);
                }
            }
        }
        let probe = heycode_core::ProviderRequestOption::new(
            "responses-plan",
            self.option_kind.clone(),
            serde_json::json!({"definitions":[self.definitions[0].request.clone()]}),
        )
        .map_err(|_| ResponsesServerToolFault::InvalidConfiguration)?;
        drop(probe);
        Ok(())
    }

    fn has_definition(&self, request: &serde_json::Value) -> bool {
        self.definitions
            .iter()
            .any(|definition| definition.request == *request)
    }
}

impl std::fmt::Debug for ResponsesServerToolPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponsesServerToolPlan")
            .field("option_kind", &self.option_kind)
            .field("definition_count", &self.definitions.len())
            .finish()
    }
}

/// Which retained output items a route requires when a prior assistant turn is
/// replayed into a new stateless request.
///
/// This adapter always sends `store: false`, so nothing is retained
/// server-side and continuation depends entirely on what the caller replays.
/// The provider profile chooses the requirement; protocol compatibility never
/// does. Both levels above `None` are stated by the API itself:
///
/// - the `include` parameter documents `reasoning.encrypted_content` as what
///   "enables reasoning items to be used in multi-turn conversations when
///   using the Responses API statelessly (like when the `store` parameter is
///   set to `false` …)";
/// - `MessagePhase` documents that "For models like `gpt-5.3-codex` and
///   beyond, when sending follow-up requests, preserve and resend phase on all
///   assistant messages — **dropping it can degrade performance**."
///
/// That last sentence is why a miss is a pre-dispatch error rather than a
/// warning: a request that drops these items still succeeds, just worse, so
/// nothing downstream would ever notice the regression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsesContinuation {
    /// No retained-item requirement. Compatibility routes and models that
    /// return no reasoning items at all.
    None,
    /// A replayed assistant turn that called tools must carry that turn's
    /// completed reasoning item with non-empty `encrypted_content`.
    ReasoningItems,
    /// Additionally, every replayed assistant message item must carry `phase`.
    ReasoningAndPhase,
}

impl OpenAiResponsesConfig {
    /// Attempt requested function tools when a user-selected endpoint has no
    /// model capability evidence. Unknown remains Unknown; explicit Unsupported
    /// still fails locally and endpoint errors propagate without dropping tools.
    #[must_use]
    pub fn with_unknown_tool_attempts(mut self) -> Self {
        self.tool_admission = ToolAdmission::AttemptUnknown;
        self
    }

    /// Build one bearer-authenticated Responses route from a literal key
    /// captured for this adapter's lifetime.
    #[must_use]
    pub fn with_key(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_credential(provider, base_url, crate::RouteCredential::fixed(api_key))
    }

    /// Build one bearer-authenticated Responses route whose credential is
    /// resolved once per operation.
    #[must_use]
    pub fn with_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential,
            credential_header: ResponsesCredentialHeader::BearerAuthorization,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
            default_max_output_tokens: None,
            retry_spec: crate::RetrySpec::standard(),
            tool_admission: ToolAdmission::RequireEvidence,
            continuation: ResponsesContinuation::None,
            provider_request_options: Vec::new(),
            server_tool_plans: Vec::new(),
        }
    }

    /// Send the operation credential through an `api-key` header rather than
    /// the default bearer `Authorization` header.
    #[must_use]
    pub fn with_api_key_header(mut self) -> Self {
        self.credential_header = ResponsesCredentialHeader::ApiKey;
        self
    }

    /// Attach the retained-item requirement this route enforces on replay.
    #[must_use]
    pub fn with_continuation(mut self, continuation: ResponsesContinuation) -> Self {
        self.continuation = continuation;
        self
    }

    /// Attach exact-route reasoning choices/default.
    #[must_use]
    pub fn with_reasoning(
        mut self,
        efforts: Vec<ReasoningEffortId>,
        default: Option<ReasoningEffortId>,
    ) -> Self {
        self.reasoning_efforts = efforts;
        self.default_reasoning_effort = default;
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

    /// Bind every exact member of one provider-owned option object to a
    /// top-level Responses request field.
    ///
    /// Configuration validation rejects empty/duplicate/reserved mappings.
    /// Request validation requires the option object to contain exactly these
    /// members, so no durable sibling can be silently dropped.
    #[must_use]
    pub fn with_provider_request_option_members(
        mut self,
        kind: impl Into<String>,
        members: Vec<(String, String)>,
    ) -> Self {
        self.provider_request_options
            .push(ResponsesProviderOptionWire {
                kind: kind.into(),
                members,
            });
        self
    }

    /// Register one provider-owned exact hosted/server-tool plan.
    #[must_use]
    pub fn with_server_tool_plan(mut self, plan: ResponsesServerToolPlan) -> Self {
        self.server_tool_plans.push(plan);
        self
    }

    /// Secret-free exact-route resolution proposal.
    #[must_use]
    pub fn resolve_spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::OpenAiResponses,
            target: InferenceTarget::Http {
                base_url: self.base_url.clone(),
            },
            authentication: self.credential.binding(),
            default_max_output_tokens: self.default_max_output_tokens,
            reasoning_efforts: self.reasoning_efforts.clone(),
            default_reasoning_effort: self.default_reasoning_effort.clone(),
        }
    }
}

impl std::fmt::Debug for OpenAiResponsesConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("credential_header", &self.credential_header)
            .field("reasoning_efforts", &self.reasoning_efforts)
            .field("default_reasoning_effort", &self.default_reasoning_effort)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("retry_spec", &self.retry_spec)
            .field("provider_request_options", &self.provider_request_options)
            .field("server_tool_plan_count", &self.server_tool_plans.len())
            .finish()
    }
}

/// Reusable OpenAI Responses protocol adapter.
pub struct OpenAiResponsesAdapter {
    config: OpenAiResponsesConfig,
    http: heycode_http::HttpService,
}

impl OpenAiResponsesAdapter {
    /// Validate configuration and bind the shared HTTP service.
    ///
    /// # Errors
    /// Missing protocol declaration, unusable key/endpoint, duplicate or
    /// invalid reasoning defaults, or zero output default.
    pub fn new(
        config: OpenAiResponsesConfig,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        validate_config(&config)?;
        Ok(Self { config, http })
    }
}

impl InferenceAdapter for OpenAiResponsesAdapter {
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
        validate_provider_options(&self.config, &draft.provider_options)?;
        let selected = selected_responses_server_tools(
            &self.config,
            &draft.provider_options,
            &draft.native_features,
        )?;
        validate_responses_server_tool_replay(&draft.inputs, &selected)?;
        let retry_spec = if draft.native_features.is_empty() && selected.definitions.is_empty() {
            self.config.retry_spec.clone()
        } else {
            self.config.retry_spec.clone().disable_replay()
        };
        if draft.native_features.contains(&NativeFeature::Compaction) {
            return Err(ResolveError::InvalidRequest {
                field: "native_features",
                message: "Responses compaction uses a distinct buffered operation".to_owned(),
            });
        }
        if draft.native_features.contains(&NativeFeature::PromptCache)
            && draft.provider_options.is_empty()
        {
            return Err(ResolveError::InvalidRequest {
                field: "native_features",
                message: "Responses prompt cache requires one durable provider option".to_owned(),
            });
        }
        validate_continuation(self.config.continuation, &draft.inputs)?;
        resolve_request_with_tool_admission(
            &self.config.provider,
            draft,
            model,
            &self.config.resolve_spec(),
            self.config.tool_admission,
        )
        .map(|call| call.with_retry_spec(retry_spec))
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.stream_cancellable(call, tokio_util::sync::CancellationToken::new())
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        let selected = match selected_responses_server_tools(
            &self.config,
            call.provider_options(),
            call.native_features(),
        ) {
            Ok(selected) => selected,
            Err(_) => {
                return one_error(invalid("resolved Responses server-tool option is invalid"));
            }
        };
        let correlation = match responses_server_tool_replay_state(call.inputs(), &selected) {
            Ok(correlation) => correlation,
            Err(error) => return one_error(error),
        };
        let body = match responses_request_body(&self.config, &call, &selected) {
            Ok(body) => body,
            Err(error) => return one_error(error),
        };
        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let body = body.to_string().into_bytes();
        let provider = call.provider().to_owned();
        let model = call.model().to_owned();
        let expect_cache_usage = call.native_features().contains(&NativeFeature::PromptCache);
        let retry_spec = call.retry_spec().clone();
        let credential_header = self.config.credential_header;
        let http = self.http.clone();
        // Resolved once, before the first attempt: every retry of this one
        // operation reuses it, and the next operation resolves again.
        let credential = match self.config.credential.acquire() {
            Ok(credential) => credential,
            Err(error) => return one_error(LlmError::UnresolvedCredential(error)),
        };
        crate::retry::retrying_stream(retry_spec, cancellation, move |attempt_cancellation| {
            let request = heycode_http::HttpSseRequest::post(url.clone(), body.clone())
                .and_then(|request| {
                    credential_request_header(request, credential_header, credential.expose())
                })
                .and_then(|request| request.header("content-type", "application/json"));
            let request = match request {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            let events = http.sse(request, attempt_cancellation);
            Box::pin(
                futures::stream::unfold(
                    ResponsesPhase::Read(
                        events,
                        Box::new(ResponsesParser::new(
                            provider.clone(),
                            model.clone(),
                            expect_cache_usage,
                            selected.normalizers.clone(),
                            selected.all_normalizers.keys().copied().collect(),
                            correlation.clone(),
                        )),
                    ),
                    drive_responses,
                )
                .flat_map(futures::stream::iter),
            )
        })
    }
}

/// Enforce the route's retained-item requirement over the exact input
/// chronology, before anything reaches the wire.
///
/// A replayed assistant turn is bounded by the inputs that open a new model
/// turn — a user message or a tool result — so each tool-calling turn must
/// carry its own reasoning item rather than inheriting an earlier one.
fn validate_continuation(
    requirement: ResponsesContinuation,
    inputs: &[InferenceInput],
) -> Result<(), ResolveError> {
    if requirement == ResponsesContinuation::None {
        return Ok(());
    }
    let mut reasoning_preserved = false;
    for input in inputs {
        match input {
            // The neutral assistant vocabulary has no reasoning item, no item
            // id and no `phase` field, so this path cannot carry what the
            // route requires. Refusing it is the whole point: the request
            // would otherwise succeed in a silently degraded form.
            InferenceInput::Message(message)
                if message.role == Role::Assistant
                    && message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()) =>
            {
                return Err(continuation_error(
                    "replayed assistant tool calls must be preserved Responses output items, not neutral messages",
                ));
            }
            // A user message or a tool result opens the next model turn.
            InferenceInput::Message(message) if matches!(message.role, Role::User | Role::Tool) => {
                reasoning_preserved = false;
            }
            InferenceInput::Message(_) => {}
            InferenceInput::ProviderState(state) => {
                let data = state.data();
                // `ResponseOutputItem` state is validated to carry a non-empty
                // `type` when it is constructed, so this is a routing read
                // rather than a shape check.
                match data.get("type").and_then(serde_json::Value::as_str) {
                    Some("reasoning") => {
                        if data
                            .get("encrypted_content")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|content| !content.is_empty())
                        {
                            reasoning_preserved = true;
                        }
                    }
                    Some("function_call") => {
                        if !reasoning_preserved {
                            return Err(continuation_error(
                                "replayed tool call is missing its turn's encrypted reasoning item",
                            ));
                        }
                    }
                    Some("message")
                        if requirement == ResponsesContinuation::ReasoningAndPhase
                            && !preserved_phase(data) =>
                    {
                        return Err(continuation_error(
                            "replayed assistant message is missing its preserved phase",
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// `MessagePhase` is `commentary | final_answer`; any non-empty preserved
/// value counts, because the adapter replays it rather than interpreting it.
fn preserved_phase(data: &serde_json::Value) -> bool {
    data.get("phase")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|phase| !phase.is_empty())
}

fn continuation_error(message: &str) -> ResolveError {
    ResolveError::InvalidRequest {
        field: "provider_state",
        message: message.to_owned(),
    }
}

fn validate_config(config: &OpenAiResponsesConfig) -> Result<(), LlmError> {
    if !config
        .provider
        .protocols
        .contains(&ProviderProtocol::OpenAiResponses)
    {
        return Err(LlmError::InvalidResponse(
            "Responses adapter provider does not declare OpenAI Responses".to_owned(),
        ));
    }
    if config.credential.fixed_is_blank() {
        return Err(crate::retry::local_failure(
            crate::ProviderErrorClass::Authentication,
        ));
    }
    let url = format!("{}/responses", config.base_url.trim_end_matches('/'));
    heycode_http::HttpSseRequest::post(url, Vec::new())
        .and_then(|request| {
            credential_request_header(
                request,
                config.credential_header,
                config.credential.probe_value(),
            )
        })
        .map_err(map_transport_error)?;
    let mut seen = BTreeSet::new();
    for effort in &config.reasoning_efforts {
        if !seen.insert(effort.as_str()) {
            return Err(LlmError::InvalidResponse(
                "Responses adapter has duplicate reasoning effort ids".to_owned(),
            ));
        }
    }
    if config
        .default_reasoning_effort
        .as_ref()
        .is_some_and(|default| !seen.contains(default.as_str()))
    {
        return Err(LlmError::InvalidResponse(
            "Responses adapter reasoning default is not in its effort list".to_owned(),
        ));
    }
    if config.default_max_output_tokens == Some(0) {
        return Err(LlmError::InvalidResponse(
            "Responses adapter output default must be positive".to_owned(),
        ));
    }
    validate_provider_option_config(config)?;
    Ok(())
}

fn credential_request_header(
    request: heycode_http::HttpSseRequest,
    header: ResponsesCredentialHeader,
    credential: &str,
) -> Result<heycode_http::HttpSseRequest, heycode_http::TransportError> {
    match header {
        ResponsesCredentialHeader::BearerAuthorization => {
            request.header("authorization", &format!("Bearer {credential}"))
        }
        ResponsesCredentialHeader::ApiKey => request.header("api-key", credential),
    }
}

fn validate_provider_option_config(config: &OpenAiResponsesConfig) -> Result<(), LlmError> {
    const RESERVED: &[&str] = &[
        "model",
        "input",
        "stream",
        "store",
        "parallel_tool_calls",
        "include",
        "metadata",
        "instructions",
        "tools",
        "tool_choice",
        "reasoning",
        "text",
        "temperature",
        "max_output_tokens",
    ];
    let mut kinds = BTreeSet::new();
    let mut fields = BTreeSet::new();
    for wire in &config.provider_request_options {
        if wire.kind.is_empty()
            || wire.kind.trim() != wire.kind
            || wire.kind.len() > 128
            || !kinds.insert(wire.kind.as_str())
            || wire.members.is_empty()
        {
            return Err(LlmError::InvalidResponse(
                "Responses provider-option configuration is invalid".to_owned(),
            ));
        }
        let mut members = BTreeSet::new();
        for (member, field) in &wire.members {
            if member.is_empty()
                || member.trim() != member
                || field.is_empty()
                || field.trim() != field
                || !members.insert(member.as_str())
                || !fields.insert(field.as_str())
                || RESERVED.contains(&field.as_str())
            {
                return Err(LlmError::InvalidResponse(
                    "Responses provider-option configuration is invalid".to_owned(),
                ));
            }
        }
    }
    let mut output_types = BTreeSet::new();
    for plan in &config.server_tool_plans {
        plan.validate()
            .map_err(|_| invalid("Responses server-tool plan configuration is invalid"))?;
        if !kinds.insert(plan.option_kind.as_str()) {
            return Err(invalid(
                "Responses provider-option kinds must be unique across all dialects",
            ));
        }
        for definition in &plan.definitions {
            for normalizer in &definition.normalizers {
                if !output_types.insert(normalizer.output_item_type()) {
                    return Err(invalid(
                        "Responses server-tool output discriminators must be unique",
                    ));
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct SelectedResponsesServerTools {
    definitions: Vec<serde_json::Value>,
    normalizers: BTreeMap<&'static str, Arc<dyn ResponsesServerToolNormalizer>>,
    all_normalizers: BTreeMap<&'static str, Arc<dyn ResponsesServerToolNormalizer>>,
}

fn selected_responses_server_tools(
    config: &OpenAiResponsesConfig,
    options: &[heycode_core::ProviderRequestOption],
    native_features: &[NativeFeature],
) -> Result<SelectedResponsesServerTools, ResolveError> {
    let all_normalizers = config
        .server_tool_plans
        .iter()
        .flat_map(|plan| &plan.definitions)
        .flat_map(|definition| &definition.normalizers)
        .map(|normalizer| (normalizer.output_item_type(), normalizer.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut definitions = Vec::new();
    let mut normalizers = BTreeMap::new();
    let mut selected_values = Vec::new();
    let mut selected_required_features = BTreeSet::new();
    for option in options {
        let Some(plan) = config
            .server_tool_plans
            .iter()
            .find(|plan| plan.option_kind == option.kind())
        else {
            continue;
        };
        let data = option
            .data()
            .as_object()
            .filter(|data| data.len() == 1)
            .ok_or_else(server_tool_option_error)?;
        let selected = data
            .get("definitions")
            .and_then(serde_json::Value::as_array)
            .filter(|selected| !selected.is_empty())
            .ok_or_else(server_tool_option_error)?;
        for request in selected {
            let definition = plan
                .definitions
                .iter()
                .find(|definition| definition.request == *request)
                .ok_or_else(server_tool_option_error)?;
            if selected_values.contains(&request) {
                return Err(server_tool_option_error());
            }
            selected_values.push(request);
            definitions.push(request.clone());
            if let Some(feature) = definition.required_native_feature {
                if !native_features.contains(&feature) {
                    return Err(ResolveError::InvalidRequest {
                        field: "native_features",
                        message: "Responses server-tool definition is missing its required native capability"
                            .to_owned(),
                    });
                }
                selected_required_features.insert(feature);
            }
            for normalizer in &definition.normalizers {
                if normalizers
                    .insert(normalizer.output_item_type(), normalizer.clone())
                    .is_some()
                {
                    return Err(server_tool_option_error());
                }
            }
        }
    }
    if !config.server_tool_plans.is_empty()
        && native_features.iter().any(|feature| {
            matches!(feature, NativeFeature::Web) && !selected_required_features.contains(feature)
        })
    {
        return Err(ResolveError::InvalidRequest {
            field: "provider_options",
            message: "Responses native capability requires one exact server-tool definition"
                .to_owned(),
        });
    }
    Ok(SelectedResponsesServerTools {
        definitions,
        normalizers,
        all_normalizers,
    })
}

fn server_tool_option_error() -> ResolveError {
    ResolveError::InvalidRequest {
        field: "provider_options",
        message: "Responses server-tool option does not match its configured definition allowlist"
            .to_owned(),
    }
}

fn validate_provider_options(
    config: &OpenAiResponsesConfig,
    options: &[heycode_core::ProviderRequestOption],
) -> Result<(), ResolveError> {
    let mut kinds = BTreeSet::new();
    for option in options {
        if config
            .server_tool_plans
            .iter()
            .any(|plan| plan.option_kind == option.kind())
        {
            if !kinds.insert(option.kind()) {
                return Err(server_tool_option_error());
            }
            continue;
        }
        let wire = config
            .provider_request_options
            .iter()
            .find(|wire| wire.kind == option.kind())
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Responses route received an unsupported provider option kind".to_owned(),
            })?;
        if !kinds.insert(option.kind()) || provider_option_members(wire, option).is_err() {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Responses provider option object does not match its wire dialect"
                    .to_owned(),
            });
        }
    }
    Ok(())
}

fn provider_option_members<'wire, 'option>(
    wire: &'wire ResponsesProviderOptionWire,
    option: &'option heycode_core::ProviderRequestOption,
) -> Result<Vec<(&'option serde_json::Value, &'wire str)>, ()> {
    let object = option.data().as_object().ok_or(())?;
    if object.len() != wire.members.len()
        || wire
            .members
            .iter()
            .any(|(member, _)| !object.contains_key(member))
    {
        return Err(());
    }
    wire.members
        .iter()
        .map(|(member, field)| {
            object
                .get(member)
                .map(|value| (value, field.as_str()))
                .ok_or(())
        })
        .collect()
}

fn responses_request_body(
    config: &OpenAiResponsesConfig,
    call: &ResolvedCall,
    selected: &SelectedResponsesServerTools,
) -> Result<serde_json::Value, LlmError> {
    if call.protocol() != ProviderProtocol::OpenAiResponses {
        return Err(LlmError::InvalidResponse(
            "resolved call protocol is not OpenAI Responses".to_owned(),
        ));
    }
    let mut input = Vec::new();
    for item in call.inputs() {
        match item {
            InferenceInput::Message(message) => input.extend(response_input(message)?),
            InferenceInput::ProviderState(state) => input.push(state.data().clone()),
        }
    }
    let mut tools: Vec<serde_json::Value> = call
        .tools()
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
                "strict": false,
            })
        })
        .collect();
    if call.native_features().contains(&NativeFeature::Web) && config.server_tool_plans.is_empty() {
        tools.push(serde_json::json!({"type":"web_search"}));
    }
    tools.extend(selected.definitions.iter().cloned());
    let mut body = serde_json::json!({
        "model": call.model(),
        "input": input,
        "stream": true,
        "store": false,
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "metadata": {"heycode_purpose": purpose_name(call.purpose())},
    });
    if let Some(system) = call.system() {
        body["instructions"] = serde_json::json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!("auto");
    }
    if let Some(effort) = call.reasoning_effort() {
        body["reasoning"] = serde_json::json!({"effort":effort.as_str()});
    }
    if let Some(schema) = call.structured_output() {
        body["text"] = serde_json::json!({
            "format": {
                "type": "json_schema",
                "name": "heycode_response",
                "schema": schema,
                "strict": true,
            }
        });
    }
    if let Some(temperature) = call.temperature() {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(max_output_tokens) = call.max_output_tokens() {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }
    for option in call.provider_options() {
        if config
            .server_tool_plans
            .iter()
            .any(|plan| plan.option_kind == option.kind())
        {
            continue;
        }
        let wire = config
            .provider_request_options
            .iter()
            .find(|wire| wire.kind == option.kind())
            .ok_or_else(|| {
                LlmError::InvalidResponse(
                    "resolved Responses provider option has no wire dialect".to_owned(),
                )
            })?;
        for (value, field) in provider_option_members(wire, option).map_err(|_| {
            LlmError::InvalidResponse("resolved Responses provider option is invalid".to_owned())
        })? {
            body[field] = value.clone();
        }
    }
    Ok(body)
}

fn response_input(message: &ChatMessage) -> Result<Vec<serde_json::Value>, LlmError> {
    match message.role {
        Role::User => {
            let mut content =
                Vec::with_capacity(message.documents.len() + message.images.len() + 1);
            content.extend(message.documents.iter().map(|document| {
                serde_json::json!({
                    "type":"input_file",
                    "filename":document.filename(),
                    "file_data":crate::vocab::document_data_url(document),
                })
            }));
            if !message.content.is_empty() {
                content.push(serde_json::json!({
                    "type":"input_text",
                    "text":message.content,
                }));
            }
            content.extend(message.images.iter().map(|image| {
                serde_json::json!({
                    "type":"input_image",
                    "image_url":crate::vocab::image_data_url(image),
                    "detail":"auto",
                })
            }));
            if content.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "user input contains no text or images".to_owned(),
                ));
            }
            Ok(vec![serde_json::json!({
                "type":"message",
                "role":"user",
                "content":content,
            })])
        }
        Role::System => {
            if !message.images.is_empty() || !message.documents.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "developer input cannot contain images".to_owned(),
                ));
            }
            Ok(vec![response_message(
                "developer",
                "input_text",
                &message.content,
            )])
        }
        Role::Assistant => {
            if !message.images.is_empty() || !message.documents.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "assistant input cannot contain images".to_owned(),
                ));
            }
            let mut items = Vec::new();
            if !message.content.is_empty() {
                items.push(response_message(
                    "assistant",
                    "output_text",
                    &message.content,
                ));
            }
            if let Some(calls) = &message.tool_calls {
                items.extend(calls.iter().map(|call| {
                    serde_json::json!({
                        "type":"function_call",
                        "call_id":call.id,
                        "name":call.name,
                        "arguments":call.arguments,
                    })
                }));
            }
            Ok(items)
        }
        Role::Tool => {
            if !message.images.is_empty() || !message.documents.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "tool input cannot contain images".to_owned(),
                ));
            }
            let call_id = message.tool_call_id.as_ref().ok_or_else(|| {
                LlmError::InvalidResponse("tool message has no tool_call_id".to_owned())
            })?;
            Ok(vec![serde_json::json!({
                "type":"function_call_output",
                "call_id":call_id,
                "output":message.content,
            })])
        }
    }
}

fn response_message(role: &str, content_type: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "type":"message",
        "role":role,
        "content":[{"type":content_type,"text":text}],
    })
}

#[derive(Debug, Clone, Default)]
struct ResponsesServerToolCorrelation {
    calls: BTreeSet<String>,
    results: BTreeSet<String>,
    pending: BTreeMap<String, &'static str>,
}

impl ResponsesServerToolCorrelation {
    fn admit(
        &mut self,
        item_type: &'static str,
        normalization: &ResponsesServerToolNormalization,
    ) -> Result<(), LlmError> {
        if let Some(call) = normalization.call() {
            if !self.calls.insert(call.id().as_str().to_owned()) {
                return Err(invalid("Responses server-tool call id was used twice"));
            }
            self.pending
                .insert(call.id().as_str().to_owned(), item_type);
        }
        if let Some(result) = normalization.result() {
            let call_id = result.call_id().as_str();
            if !self.calls.contains(call_id) || !self.results.insert(call_id.to_owned()) {
                return Err(invalid(
                    "Responses server-tool result does not settle one preceding unique call",
                ));
            }
            self.pending.remove(call_id);
        }
        Ok(())
    }
}

fn validate_responses_server_tool_replay(
    inputs: &[InferenceInput],
    selected: &SelectedResponsesServerTools,
) -> Result<(), ResolveError> {
    responses_server_tool_replay_state(inputs, selected)
        .map(|_| ())
        .map_err(|_| ResolveError::InvalidRequest {
            field: "provider_state",
            message: "Responses server-tool replay state is invalid or lacks its exact definition"
                .to_owned(),
        })
}

fn responses_server_tool_replay_state(
    inputs: &[InferenceInput],
    selected: &SelectedResponsesServerTools,
) -> Result<ResponsesServerToolCorrelation, LlmError> {
    let mut correlation = ResponsesServerToolCorrelation::default();
    for state in inputs.iter().filter_map(|input| match input {
        InferenceInput::ProviderState(state)
            if state.kind() == ProviderStateKind::ResponseOutputItem =>
        {
            Some(state)
        }
        InferenceInput::Message(_) | InferenceInput::ProviderState(_) => None,
    }) {
        let item_type = state
            .data()
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("Responses provider state item has no type"))?;
        if let Some((registered_type, normalizer)) =
            selected.all_normalizers.get_key_value(item_type)
        {
            let normalization = normalizer
                .normalize(state)
                .map_err(|_| invalid("Responses server-tool replay item is invalid"))?;
            correlation.admit(registered_type, &normalization)?;
        }
    }
    if correlation
        .pending
        .values()
        .any(|item_type| !selected.normalizers.contains_key(item_type))
    {
        return Err(invalid(
            "Responses pending server-tool replay omitted its exact request definition",
        ));
    }
    Ok(correlation)
}

enum ResponsesPhase {
    Read(heycode_http::SseEventStream, Box<ResponsesParser>),
    Done,
}

async fn drive_responses(
    phase: ResponsesPhase,
) -> Option<(Vec<Result<InferenceEvent, LlmError>>, ResponsesPhase)> {
    match phase {
        ResponsesPhase::Read(mut events, mut parser) => match events.next().await {
            Some(Ok(event)) => {
                let output = parser.event(event);
                let next = if parser.terminal {
                    ResponsesPhase::Done
                } else {
                    ResponsesPhase::Read(events, parser)
                };
                Some((output, next))
            }
            Some(Err(error)) => Some((vec![Err(map_transport_error(error))], ResponsesPhase::Done)),
            None => Some((parser.finish(), ResponsesPhase::Done)),
        },
        ResponsesPhase::Done => None,
    }
}

struct ActiveItem {
    id: String,
    kind: StreamItemKind,
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    emitted_arguments: bool,
}

struct ResponsesParser {
    provider: String,
    model: String,
    last_sequence: Option<u64>,
    response_id: Option<String>,
    active: BTreeMap<u32, ActiveItem>,
    saw_tool_call: bool,
    expect_cache_usage: bool,
    server_tool_normalizers: BTreeMap<&'static str, Arc<dyn ResponsesServerToolNormalizer>>,
    registered_server_tool_types: BTreeSet<&'static str>,
    server_tool_correlation: ResponsesServerToolCorrelation,
    terminal: bool,
}

impl ResponsesParser {
    fn new(
        provider: String,
        model: String,
        expect_cache_usage: bool,
        server_tool_normalizers: BTreeMap<&'static str, Arc<dyn ResponsesServerToolNormalizer>>,
        registered_server_tool_types: BTreeSet<&'static str>,
        server_tool_correlation: ResponsesServerToolCorrelation,
    ) -> Self {
        Self {
            provider,
            model,
            last_sequence: None,
            response_id: None,
            active: BTreeMap::new(),
            saw_tool_call: false,
            expect_cache_usage,
            server_tool_normalizers,
            registered_server_tool_types,
            server_tool_correlation,
            terminal: false,
        }
    }

    fn event(&mut self, event: heycode_http::SseEvent) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        let result = self.parse_event(event);
        match result {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => {
                self.terminal = true;
                vec![Err(error)]
            }
        }
    }

    fn parse_event(
        &mut self,
        event: heycode_http::SseEvent,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let value: serde_json::Value = serde_json::from_str(&event.data)
            .map_err(|error| invalid(format!("Responses event is not JSON: {error}")))?;
        let object = as_object(&value, "Responses event")?;
        let event_type = required_str(object, "type")?;
        if event.event != "message" && event.event != event_type {
            return Err(invalid(format!(
                "SSE event `{}` disagrees with payload type `{event_type}`",
                event.event
            )));
        }
        let sequence = required_u64(object, "sequence_number")?;
        if self
            .last_sequence
            .is_some_and(|previous| sequence != previous.saturating_add(1))
        {
            return Err(invalid(format!(
                "Responses sequence is not contiguous after {:?}: {sequence}",
                self.last_sequence
            )));
        }
        self.last_sequence = Some(sequence);

        match event_type {
            "response.created" => self.response_started(object),
            "response.output_item.added" => self.item_started(object),
            "response.output_item.done" => self.item_finished(object),
            "response.output_text.delta" | "response.refusal.delta" => {
                Ok(vec![InferenceEvent::TextDelta(
                    required_str(object, "delta")?.to_owned(),
                )])
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                Ok(vec![InferenceEvent::ReasoningDelta(
                    required_str(object, "delta")?.to_owned(),
                )])
            }
            "response.function_call_arguments.delta" => self.function_delta(object),
            "response.completed" | "response.incomplete" => self.response_finished(object),
            "response.failed" => Err(response_failure(object)),
            "response.cancelled" => Err(crate::retry::cancelled_error()),
            _ => Ok(Vec::new()),
        }
    }

    fn response_started(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let response = required_object(event, "response")?;
        let id = required_str(response, "id")?.to_owned();
        if self.response_id.replace(id.clone()).is_some() {
            return Err(invalid("Responses stream created more than one response"));
        }
        Ok(vec![InferenceEvent::ResponseStarted { response_id: id }])
    }

    fn item_started(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let output_index = required_u32(event, "output_index")?;
        let item = required_object(event, "item")?;
        let id = required_str(item, "id")?.to_owned();
        let raw_kind = required_str(item, "type")?;
        let kind = item_kind(raw_kind);
        let (call_id, name) = if raw_kind == "function_call" {
            self.saw_tool_call = true;
            (
                Some(required_str(item, "call_id")?.to_owned()),
                Some(required_str(item, "name")?.to_owned()),
            )
        } else {
            (None, None)
        };
        let arguments = item
            .get("arguments")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        let active = ActiveItem {
            id: id.clone(),
            kind: kind.clone(),
            call_id,
            name,
            arguments,
            emitted_arguments: false,
        };
        if self.active.insert(output_index, active).is_some() {
            return Err(invalid(format!(
                "Responses output index {output_index} started twice"
            )));
        }
        Ok(vec![InferenceEvent::ItemStarted {
            output_index,
            item_id: id,
            kind,
        }])
    }

    fn function_delta(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let output_index = required_u32(event, "output_index")?;
        let item_id = required_str(event, "item_id")?;
        let delta = required_str(event, "delta")?.to_owned();
        let active = self.active.get_mut(&output_index).ok_or_else(|| {
            invalid(format!(
                "function delta references inactive output index {output_index}"
            ))
        })?;
        if active.id != item_id || active.kind != StreamItemKind::FunctionCall {
            return Err(invalid("function delta item identity/type mismatch"));
        }
        active.arguments.push_str(&delta);
        let first = !active.emitted_arguments;
        active.emitted_arguments = true;
        Ok(vec![InferenceEvent::ToolCallDelta {
            output_index,
            id: first
                .then(|| active.call_id.clone())
                .flatten()
                .map(heycode_core::CallId::from_raw),
            name: first.then(|| active.name.clone()).flatten(),
            arguments_delta: delta,
        }])
    }

    fn item_finished(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let output_index = required_u32(event, "output_index")?;
        let item = required_object(event, "item")?;
        let id = required_str(item, "id")?.to_owned();
        let raw_kind = required_str(item, "type")?;
        let kind = item_kind(raw_kind);
        let active = self.active.remove(&output_index).ok_or_else(|| {
            invalid(format!(
                "Responses output index {output_index} finished without start"
            ))
        })?;
        if active.id != id || active.kind != kind {
            return Err(invalid("Responses completed item identity/type mismatch"));
        }
        let mut output = Vec::new();
        if kind == StreamItemKind::FunctionCall {
            let final_arguments = required_str(item, "arguments")?;
            if active.emitted_arguments {
                if active.arguments != final_arguments {
                    return Err(invalid(
                        "Responses function argument deltas do not equal completed arguments",
                    ));
                }
            } else if !final_arguments.is_empty() {
                output.push(InferenceEvent::ToolCallDelta {
                    output_index,
                    id: active.call_id.map(heycode_core::CallId::from_raw),
                    name: active.name,
                    arguments_delta: final_arguments.to_owned(),
                });
            }
        }
        output.push(InferenceEvent::ItemFinished {
            output_index,
            item_id: id,
            kind,
        });
        let state = ProviderStateItem::new(
            self.provider.clone(),
            self.model.clone(),
            ProviderProtocol::OpenAiResponses,
            ProviderStateKind::ResponseOutputItem,
            serde_json::Value::Object(item.clone()),
        )
        .map_err(|error| invalid(error.to_string()))?;
        if let Some((registered_type, normalizer)) =
            self.server_tool_normalizers.get_key_value(raw_kind)
        {
            let normalization = normalizer
                .normalize(&state)
                .map_err(|_| invalid("Responses server-tool item failed normalization"))?;
            self.server_tool_correlation
                .admit(registered_type, &normalization)?;
            if let Some(call) = normalization.call {
                output.push(InferenceEvent::ServerToolCall { output_index, call });
            }
            if let Some(result) = normalization.result {
                output.push(InferenceEvent::ServerToolResult {
                    output_index,
                    result,
                });
            }
            output.extend(normalization.citations.into_iter().map(|citation| {
                InferenceEvent::Citation {
                    output_index,
                    citation,
                }
            }));
        } else if self.registered_server_tool_types.contains(raw_kind) {
            return Err(invalid(
                "Responses returned a server-tool item whose exact definition was not selected",
            ));
        }
        if raw_kind == "message" {
            output.extend(
                completed_message_citations(item)?
                    .into_iter()
                    .map(|citation| InferenceEvent::Citation {
                        output_index,
                        citation,
                    }),
            );
        }
        output.push(InferenceEvent::ProviderState(state));
        Ok(output)
    }

    fn response_finished(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if !self.active.is_empty() {
            return Err(invalid(
                "Responses terminal event arrived with unfinished output items",
            ));
        }
        if !self.server_tool_correlation.pending.is_empty() {
            return Err(invalid(
                "Responses terminal event left a provider-executed server tool unsettled",
            ));
        }
        let response = required_object(event, "response")?;
        let id = required_str(response, "id")?.to_owned();
        if self
            .response_id
            .as_ref()
            .is_some_and(|created| created != &id)
        {
            return Err(invalid("Responses terminal id differs from created id"));
        }
        if self.response_id.is_none() {
            self.response_id = Some(id.clone());
        }
        let status = required_str(response, "status")?.to_owned();
        let mut output = vec![InferenceEvent::ResponseFinished {
            response_id: id,
            status: status.clone(),
        }];
        if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
            let usage = as_object(usage, "response.usage")?;
            if let Some(metadata) = responses_metadata(usage, self.expect_cache_usage)? {
                output.push(InferenceEvent::ResponseMetadata(metadata));
            }
            output.push(InferenceEvent::Usage(TokenUsage {
                prompt_tokens: required_u64(usage, "input_tokens")?,
                completion_tokens: required_u64(usage, "output_tokens")?,
            }));
        }
        let finish = if self.saw_tool_call {
            FinishReason::ToolCalls
        } else if status == "incomplete"
            && response
                .get("incomplete_details")
                .and_then(serde_json::Value::as_object)
                .and_then(|details| details.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some("max_output_tokens")
        {
            FinishReason::Length
        } else {
            FinishReason::Stop
        };
        output.push(InferenceEvent::Finish(finish));
        self.terminal = true;
        Ok(output)
    }

    fn finish(mut self) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            Vec::new()
        } else {
            self.terminal = true;
            vec![Err(invalid(
                "Responses SSE ended before a terminal response event",
            ))]
        }
    }
}

// Detailed usage is an optional cost enrichment on an ordinary turn and a
// promise only on a negotiated prompt-cache call. `required` carries that
// promise: with it every counter is demanded and a malformed one is a protocol
// failure; without it the parse follows the evidence the payload actually
// carries and any residual inconsistency costs the `ResponseMetadata` event
// instead of the turn.
fn responses_metadata(
    usage: &serde_json::Map<String, serde_json::Value>,
    required: bool,
) -> Result<Option<heycode_core::ProviderResponseMetadata>, LlmError> {
    let detailed =
        usage.contains_key("input_tokens_details") || usage.contains_key("output_tokens_details");
    if !required && !detailed {
        return Ok(None);
    }
    match detailed_responses_metadata(usage, required) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if required => Err(error),
        Err(_) => Ok(None),
    }
}

fn detailed_responses_metadata(
    usage: &serde_json::Map<String, serde_json::Value>,
    required: bool,
) -> Result<heycode_core::ProviderResponseMetadata, LlmError> {
    let input_tokens = required_u64(usage, "input_tokens")?;
    let output_tokens = required_u64(usage, "output_tokens")?;
    if required || usage.contains_key("total_tokens") {
        let total = required_u64(usage, "total_tokens")?;
        if input_tokens.checked_add(output_tokens) != Some(total) {
            return Err(invalid("Responses detailed usage total is inconsistent"));
        }
    }
    let absent = serde_json::Map::new();
    let input_details = promised_object(usage, "input_tokens_details", required, &absent)?;
    let output_details = promised_object(usage, "output_tokens_details", required, &absent)?;
    let cache = heycode_core::ProviderCacheUsage::new(
        input_tokens,
        output_tokens,
        promised_u64(input_details, "cached_tokens", required)?,
        promised_u64(input_details, "cache_write_tokens", required)?,
    )
    .map_err(|_| invalid("Responses detailed usage is inconsistent"))?;
    let cache = match output_details.get("reasoning_tokens") {
        Some(_) => cache
            .with_reasoning_tokens(required_u64(output_details, "reasoning_tokens")?)
            .map_err(|_| invalid("Responses detailed usage is inconsistent"))?,
        None => cache,
    };
    heycode_core::ProviderResponseMetadata::new(Some(cache), Vec::new(), None)
        .map_err(|_| invalid("Responses detailed usage is inconsistent"))
}

// A nested usage object the payload did not send reads as empty unless the
// negotiated call promised it; a present one must still be an object.
fn promised_object<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    promised: bool,
    absent: &'a serde_json::Map<String, serde_json::Value>,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    match object.get(field) {
        Some(value) => as_object(value, field),
        None if promised => Err(invalid(format!("missing `{field}`"))),
        None => Ok(absent),
    }
}

// A counter the payload did not send is zero unless the negotiated call
// promised it; a present one must still be a u64.
fn promised_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    promised: bool,
) -> Result<u64, LlmError> {
    if promised || object.contains_key(field) {
        required_u64(object, field)
    } else {
        Ok(0)
    }
}

fn completed_message_citations(
    item: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<heycode_core::UrlCitation>, LlmError> {
    let content = item
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid("Responses completed message content must be an array"))?;
    let mut citations = Vec::new();
    for part in content {
        let part = part
            .as_object()
            .ok_or_else(|| invalid("Responses completed message content must contain objects"))?;
        if part.get("type").and_then(serde_json::Value::as_str) != Some("output_text") {
            continue;
        }
        let Some(annotations) = part.get("annotations") else {
            continue;
        };
        let annotations = annotations
            .as_array()
            .ok_or_else(|| invalid("Responses output-text annotations must be an array"))?;
        for annotation in annotations {
            let annotation = annotation
                .as_object()
                .ok_or_else(|| invalid("Responses output-text annotation must be an object"))?;
            if annotation.get("type").and_then(serde_json::Value::as_str) != Some("url_citation") {
                continue;
            }
            let url = required_str(annotation, "url")?;
            let title = required_str(annotation, "title")?;
            citations.push(
                heycode_core::UrlCitation::new(
                    url,
                    Some(title),
                    None,
                    Some(required_u32(annotation, "start_index")?),
                    Some(required_u32(annotation, "end_index")?),
                )
                .map_err(|_| invalid("Responses URL citation is invalid"))?,
            );
        }
    }
    Ok(citations)
}

fn item_kind(value: &str) -> StreamItemKind {
    match value {
        "message" => StreamItemKind::Message,
        "reasoning" => StreamItemKind::Reasoning,
        "function_call" => StreamItemKind::FunctionCall,
        other => StreamItemKind::Other(other.to_owned()),
    }
}

fn safe_server_tool_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn has_duplicate_values<'a>(values: impl IntoIterator<Item = &'a serde_json::Value>) -> bool {
    let mut seen = Vec::new();
    for value in values {
        if seen.contains(&value) {
            return true;
        }
        seen.push(value);
    }
    false
}

fn response_failure(event: &serde_json::Map<String, serde_json::Value>) -> LlmError {
    let response = event.get("response").and_then(serde_json::Value::as_object);
    let error = response
        .and_then(|response| response.get("error"))
        .and_then(serde_json::Value::as_object);
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            error
                .and_then(|error| error.get("type"))
                .and_then(serde_json::Value::as_str)
        });
    crate::retry::provider_event_error(crate::ProviderErrorClass::Server, code)
}

fn required_object<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    object
        .get(field)
        .ok_or_else(|| invalid(format!("missing `{field}`")))
        .and_then(|value| as_object(value, field))
}

fn as_object<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("`{field}` must be an object")))
}

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("`{field}` must be a string")))
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u64, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid(format!("`{field}` must be a u64")))
}

fn required_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u32, LlmError> {
    let value = required_u64(object, field)?;
    u32::try_from(value).map_err(|_| invalid(format!("`{field}` must be a u32")))
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

fn purpose_name(purpose: CallPurpose) -> &'static str {
    match purpose {
        CallPurpose::Conversation => "conversation",
        CallPurpose::SessionTitle => "session_title",
        CallPurpose::Compaction => "compaction",
        CallPurpose::Evaluation => "evaluation",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod image_tests {
    use super::*;

    #[test]
    fn responses_user_images_use_input_image_data_urls_after_text() {
        let image = crate::ChatImage::new(
            heycode_core::AttachmentMediaType::new("image/png").unwrap(),
            vec![1, 2, 3],
        )
        .unwrap();
        let items =
            response_input(&ChatMessage::user_with_images("describe", vec![image])).unwrap();
        assert_eq!(
            items[0]["content"][0],
            serde_json::json!({
                "type":"input_text","text":"describe"
            })
        );
        assert_eq!(
            items[0]["content"][1],
            serde_json::json!({
                "type":"input_image","image_url":"data:image/png;base64,AQID","detail":"auto"
            })
        );
    }

    #[test]
    fn responses_user_pdf_uses_input_file_before_text() {
        let document = crate::ChatDocument::new(
            heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
            "guide.pdf",
            b"%PDF-".to_vec(),
        )
        .unwrap();
        let items = response_input(&ChatMessage::user_with_media(
            "summarize",
            Vec::new(),
            vec![document],
        ))
        .unwrap();
        assert_eq!(
            items[0]["content"][0],
            serde_json::json!({
                "type":"input_file",
                "filename":"guide.pdf",
                "file_data":"data:application/pdf;base64,JVBERi0="
            })
        );
        assert_eq!(
            items[0]["content"][1],
            serde_json::json!({"type":"input_text","text":"summarize"})
        );
    }
}
