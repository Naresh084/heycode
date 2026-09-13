//! Provider-owned OpenAI inference wiring.
//!
//! POA01 shipped identity, auth and discovery but nothing that could dispatch.
//! This module advertises the reusable `OpenAiResponsesAdapter` that P03 owns
//! and chooses the retained-item requirement that adapter enforces on replay.

use std::sync::Arc;

use heycode_core::ProviderRequestOption;
use heycode_http::HttpService;
use heycode_llm::{
    CallPurpose, ChatMessage, ChatRequest, ChunkStream, InferenceAdapter, InferenceInput,
    InferenceStream, LlmError, NativeCompactionAdapter, NativeFeature, OpenAiResponsesAdapter,
    OpenAiResponsesConfig, Provider, ProviderInfo, ProviderOptionContext, RequestDraft,
    RequestedCapability, ResolveError, ResolvedCall, ResponsesContinuation, RetrySafety, Role,
    RouteCredential, resolve_request,
};

use crate::catalog::{DEFAULT_BASE_URL, OPENAI_PROVIDER, provider_descriptor};
use crate::compaction::{OpenAiCompactionClient, openai_compaction_support};
use crate::hosted_tools::{
    OpenAiHostedToolDefinition, OpenAiHostedToolFault, OpenAiHostedToolKind, OpenAiHostedTools,
};
use crate::prompt_cache::OpenAiPromptCacheControl;
use crate::{OPENAI_API_KEY_REFERENCE, OPENAI_GPT_5_6_SOL};

/// The official OpenAI route: a Responses adapter plus provider identity.
pub struct OpenAiProvider {
    adapter: OpenAiResponsesAdapter,
    config: OpenAiResponsesConfig,
    http: HttpService,
    compaction: OpenAiCompactionClient,
    default_model: String,
    request_options: Vec<ProviderRequestOption>,
    hosted_tools: Option<OpenAiHostedTools>,
}

impl OpenAiProvider {
    /// Retained-item requirement this route enforces when a prior assistant
    /// turn is replayed.
    ///
    /// The adapter always sends `store: false`, so continuation depends
    /// entirely on what the caller replays, and the provider default is a
    /// GPT-5.6 reasoning model. The current stateless `all_turns` example for
    /// that family explicitly preserves every output item, including encrypted
    /// reasoning and assistant phase, so both requirements apply to this route.
    pub const CONTINUATION: ResponsesContinuation = ResponsesContinuation::ReasoningAndPhase;

    /// Build the official OpenAI Responses route.
    ///
    /// # Errors
    /// An unusable endpoint or an empty key fails before the route is
    /// published.
    pub fn new(
        http: HttpService,
        api_key: impl Into<String>,
        default_model: Option<String>,
    ) -> Result<Self, LlmError> {
        Self::with_base_url(http, DEFAULT_BASE_URL, api_key, default_model)
    }

    /// Build against an explicit OpenAI-compatible API base URL.
    ///
    /// # Errors
    /// An unusable endpoint or an empty key fails before the route is
    /// published.
    pub fn with_base_url(
        http: HttpService,
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
        default_model: Option<String>,
    ) -> Result<Self, LlmError> {
        Self::with_base_url_and_credential(
            http,
            base_url,
            RouteCredential::fixed(api_key),
            default_model,
        )
    }

    /// Build the official route with a credential resolved once per operation.
    ///
    /// # Errors
    /// An unusable endpoint or credential binding fails before publication.
    pub fn with_credential(
        http: HttpService,
        credential: RouteCredential,
        default_model: Option<String>,
    ) -> Result<Self, LlmError> {
        Self::with_base_url_and_credential(http, DEFAULT_BASE_URL, credential, default_model)
    }

    /// Build an explicit-base route with a credential resolved once per
    /// operation.
    ///
    /// # Errors
    /// An unusable endpoint or credential binding fails before publication.
    pub fn with_base_url_and_credential(
        http: HttpService,
        base_url: impl AsRef<str>,
        credential: RouteCredential,
        default_model: Option<String>,
    ) -> Result<Self, LlmError> {
        let origin = base_url.as_ref().trim_end_matches('/');
        let responses_base_url = format!("{origin}/v1");
        let config = OpenAiResponsesConfig::with_credential(
            provider_descriptor(),
            responses_base_url,
            credential.clone(),
        )
        .with_continuation(Self::CONTINUATION)
        .with_provider_request_option_members(
            "prompt-cache",
            vec![
                ("prompt_cache_key".to_owned(), "prompt_cache_key".to_owned()),
                (
                    "prompt_cache_options".to_owned(),
                    "prompt_cache_options".to_owned(),
                ),
            ],
        );
        let adapter = OpenAiResponsesAdapter::new(config.clone(), http.clone())?;
        let compaction =
            OpenAiCompactionClient::with_route_credential(http.clone(), origin, credential)
                .map_err(|_| {
                    LlmError::InvalidResponse("OpenAI compaction route is invalid".to_owned())
                })?;
        Ok(Self {
            adapter,
            config,
            http,
            compaction,
            default_model: default_model.unwrap_or_else(|| OPENAI_GPT_5_6_SOL.to_owned()),
            request_options: Vec::new(),
            hosted_tools: None,
        })
    }

    /// Attach one exact model-gated prompt-cache policy to normal Responses
    /// calls. Native compaction remains a separate operation and receives no
    /// cache option.
    ///
    /// # Errors
    /// Unproven model support or invalid provider-option construction.
    pub fn with_prompt_caching(
        mut self,
        control: OpenAiPromptCacheControl,
    ) -> Result<Self, LlmError> {
        let option = control
            .provider_option(&self.default_model)
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;
        upsert_request_option(&mut self.request_options, option);
        Ok(self)
    }

    /// Attach one provider-owned hosted-tool selection to the reusable
    /// Responses serializer/parser.
    ///
    /// The current generic bridge admits web search, file search, code
    /// interpreter and hosted shell. Computer use still needs a client action
    /// loop, image generation needs attachment admission, and this crate's MCP
    /// definition needs an approval-response owner.
    ///
    /// # Errors
    /// Duplicate configuration, unproven model support, a missing upper bridge
    /// or invalid shared adapter configuration fails before publication.
    pub fn with_hosted_tools(
        mut self,
        definitions: Vec<OpenAiHostedToolDefinition>,
    ) -> Result<Self, LlmError> {
        if self.hosted_tools.is_some() {
            return Err(LlmError::InvalidResponse(
                "OpenAI hosted tools were configured more than once".to_owned(),
            ));
        }
        let hosted_tools = OpenAiHostedTools::new(&self.default_model, definitions)
            .map_err(hosted_tool_configuration_error)?;
        let config = hosted_tools.configure(self.config.clone());
        let adapter = OpenAiResponsesAdapter::new(config.clone(), self.http.clone())?;
        self.config = config;
        self.adapter = adapter;
        self.hosted_tools = Some(hosted_tools);
        Ok(self)
    }

    fn expected_request_options(
        &self,
        model: &heycode_llm::ModelDescriptor,
        routes: &[heycode_core::NativeToolRoute],
    ) -> Result<Vec<ProviderRequestOption>, ResolveError> {
        let mut options = self.request_options.clone();
        match &self.hosted_tools {
            Some(hosted_tools) => {
                if let Some(option) = hosted_tools
                    .provider_option_for_routes(&model.id, routes)
                    .map_err(|fault| hosted_tool_resolve_error(fault, &model.id))?
                {
                    upsert_request_option(&mut options, option);
                }
            }
            None if routes.iter().any(is_openai_hosted_route) => {
                return Err(hosted_tool_resolve_error(
                    OpenAiHostedToolFault::InvalidConfiguration,
                    &model.id,
                ));
            }
            None => {}
        }
        Ok(options)
    }
}

fn is_openai_hosted_route(route: &heycode_core::NativeToolRoute) -> bool {
    route.kind() == heycode_core::NativeToolImplementationKind::Provider
        && route.provider() == Some(OPENAI_PROVIDER)
        && OpenAiHostedToolKind::ALL
            .iter()
            .any(|kind| kind.as_str() == route.logical())
}

impl Provider for OpenAiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: OPENAI_PROVIDER.to_owned(),
            default_model: self.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        Some(OPENAI_API_KEY_REFERENCE)
    }

    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        provider_descriptor()
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    fn request_options(&self) -> Vec<ProviderRequestOption> {
        self.request_options.clone()
    }

    fn request_options_for(
        &self,
        context: ProviderOptionContext<'_>,
    ) -> Result<Vec<ProviderRequestOption>, ResolveError> {
        self.expected_request_options(context.model(), context.native_tool_routes())
    }

    // No `rate_limit_headers` override: the official OpenAPI specification
    // declares only `Retry-After`, which the shared layer already reads under
    // RFC 9110. Declaring an unverified `x-ratelimit-*` spelling would make
    // `/usage` display a guess.

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        // This route advertises an inference adapter, so the legacy path must
        // fail loud rather than dispatch a request that bypasses every replay
        // contract the adapter enforces (AGENTS §5).
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "OpenAI route dispatches through its inference adapter; the legacy chat path is not available"
                    .to_owned(),
            ))
        }))
    }
}

impl InferenceAdapter for OpenAiProvider {
    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        responses_route_descriptor()
    }

    fn authentication_binding(&self) -> heycode_llm::AuthenticationBinding {
        self.adapter.authentication_binding()
    }

    fn resolve(
        &self,
        mut draft: RequestDraft,
        model: &heycode_llm::ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        let native_compaction = draft.native_features.contains(&NativeFeature::Compaction);
        let compacted_state = contains_compaction_state(&draft.inputs);
        if native_compaction {
            validate_compaction_request(&draft, model)?;
            // `/responses/compact` has no output-limit parameter. Resolution
            // removes the generic Agent proposal so the durable call matches
            // the bytes this provider operation actually sends.
            draft.max_output_tokens = None;
        } else {
            let expected = self.expected_request_options(model, &draft.native_tool_routes)?;
            if draft.provider_options != expected {
                return Err(ResolveError::InvalidRequest {
                    field: "provider_options",
                    message: "OpenAI provider options differ from the selected model/routes"
                        .to_owned(),
                });
            }
            if self
                .request_options
                .iter()
                .any(|option| option.kind() == "prompt-cache")
                && !draft.native_features.contains(&NativeFeature::PromptCache)
            {
                draft.native_features.push(NativeFeature::PromptCache);
            }
        }
        if compacted_state {
            require_compaction_support(model)?;
        }
        validate_exact_inputs(&draft.inputs)?;
        let call = if native_compaction || compacted_state {
            validate_with_shared_adapter(&self.adapter, &draft, model)?;
            let resolved = resolve_request(
                &provider_descriptor(),
                draft,
                model,
                &self.config.resolve_spec(),
            )?;
            // `resolve_request` proves replay safety from native features and
            // provider-executed routes alone. Both arms of this branch send
            // provider-held compacted state: the compaction operation commits
            // a checkpoint, and a continuation replays server-side state a
            // second send would re-enter. Neither is proven safe to send twice,
            // and a continuation carries no native feature to say so, so the
            // route withholds replay explicitly.
            let retry_spec = resolved
                .retry_spec()
                .clone()
                .with_safety(RetrySafety::Never);
            resolved.with_retry_spec(retry_spec)
        } else {
            self.adapter.resolve(draft, model)?
        };
        Ok(call)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        if call.native_features().contains(&NativeFeature::Compaction) {
            return compaction_stream_error();
        }
        self.adapter.stream(call)
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        if call.native_features().contains(&NativeFeature::Compaction) {
            return compaction_stream_error();
        }
        InferenceAdapter::stream_cancellable(&self.adapter, call, cancellation)
    }

    fn native_compaction(&self) -> Option<&dyn NativeCompactionAdapter> {
        Some(&self.compaction)
    }
}

fn responses_route_descriptor() -> heycode_llm::ProviderDescriptor {
    let mut descriptor = provider_descriptor();
    descriptor.protocols = vec![heycode_core::ProviderProtocol::OpenAiResponses];
    descriptor
}

fn upsert_request_option(options: &mut Vec<ProviderRequestOption>, option: ProviderRequestOption) {
    options.retain(|existing| existing.kind() != option.kind());
    options.push(option);
    options.sort_by(|left, right| left.kind().cmp(right.kind()));
}

fn hosted_tool_configuration_error(fault: OpenAiHostedToolFault) -> LlmError {
    let message = match fault {
        OpenAiHostedToolFault::UnprovenCapability => "OpenAI hosted-tool capability is unproven",
        OpenAiHostedToolFault::MissingSharedBridge => {
            "OpenAI hosted-tool shared bridge is unavailable"
        }
        OpenAiHostedToolFault::InvalidConfiguration
        | OpenAiHostedToolFault::WrongRoute
        | OpenAiHostedToolFault::WrongItemType
        | OpenAiHostedToolFault::InvalidItem => "OpenAI hosted-tool configuration is invalid",
    };
    LlmError::InvalidResponse(message.to_owned())
}

fn hosted_tool_resolve_error(fault: OpenAiHostedToolFault, model: &str) -> ResolveError {
    let message = match fault {
        OpenAiHostedToolFault::UnprovenCapability => {
            "OpenAI hosted-tool capability is unproven for the selected model"
        }
        OpenAiHostedToolFault::MissingSharedBridge => {
            "OpenAI hosted-tool shared bridge is unavailable"
        }
        OpenAiHostedToolFault::InvalidConfiguration
        | OpenAiHostedToolFault::WrongRoute
        | OpenAiHostedToolFault::WrongItemType
        | OpenAiHostedToolFault::InvalidItem => "OpenAI hosted-tool route selection is invalid",
    };
    ResolveError::InvalidRequest {
        field: "provider_options",
        message: format!("{message}: {model}"),
    }
}

/// The shared adapter enforces the cross-item reasoning/phase relationship.
/// This route additionally refuses the two lossy shapes that still satisfy
/// that generic check: a neutral assistant message and a mutated non-empty
/// phase value.
fn validate_exact_inputs(inputs: &[InferenceInput]) -> Result<(), ResolveError> {
    for input in inputs {
        match input {
            InferenceInput::Message(message) if message.role == Role::Assistant => {
                return Err(invalid_state(
                    "phase-preserving Responses continuation requires assistant turns as exact Responses output items",
                ));
            }
            InferenceInput::ProviderState(state) => {
                match state.data().get("type").and_then(serde_json::Value::as_str) {
                    Some("reasoning")
                        if state
                            .data()
                            .get("encrypted_content")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(str::is_empty) =>
                    {
                        return Err(invalid_state(
                            "stateless Responses reasoning items require non-empty encrypted_content",
                        ));
                    }
                    Some("message") => {
                        match state.data().get("role").and_then(serde_json::Value::as_str) {
                            Some("user") if valid_compacted_user_message(state.data()) => {}
                            Some("assistant")
                                if state
                                    .data()
                                    .get("phase")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|phase| {
                                        matches!(phase, "commentary" | "final_answer")
                                    }) => {}
                            Some("assistant") => {
                                return Err(invalid_state(
                                    "preserved Responses message phase must be commentary or final_answer",
                                ));
                            }
                            Some("user") => {
                                return Err(invalid_state(
                                    "compacted Responses user message is invalid",
                                ));
                            }
                            Some(_) | None => {
                                return Err(invalid_state(
                                    "preserved Responses message role is invalid",
                                ));
                            }
                        }
                    }
                    Some("compaction")
                        if state
                            .data()
                            .get("encrypted_content")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(str::is_empty) =>
                    {
                        return Err(invalid_state(
                            "Responses compaction items require non-empty encrypted_content",
                        ));
                    }
                    Some(_) | None => {}
                }
            }
            InferenceInput::Message(_) => {}
        }
    }
    Ok(())
}

fn validate_compaction_request(
    draft: &RequestDraft,
    model: &heycode_llm::ModelDescriptor,
) -> Result<(), ResolveError> {
    if draft.purpose != CallPurpose::Compaction
        || draft.native_features != [NativeFeature::Compaction]
        || !draft.tools.is_empty()
        || !draft.native_tool_routes.is_empty()
        || !draft.provider_options.is_empty()
        || draft.reasoning_effort.is_some()
        || draft.structured_output.is_some()
        || draft.temperature.is_some()
    {
        return Err(ResolveError::InvalidRequest {
            field: "native_features",
            message: "OpenAI native compaction requires one dedicated compaction call".to_owned(),
        });
    }
    require_compaction_support(model)
}

fn require_compaction_support(model: &heycode_llm::ModelDescriptor) -> Result<(), ResolveError> {
    match openai_compaction_support(&model.id) {
        heycode_llm::CapabilitySupport::Supported => Ok(()),
        heycode_llm::CapabilitySupport::Unsupported => Err(ResolveError::Unsupported {
            provider: OPENAI_PROVIDER.to_owned(),
            model: model.id.clone(),
            capability: RequestedCapability::NativeCompaction,
        }),
        heycode_llm::CapabilitySupport::Unknown => Err(ResolveError::Unproven {
            provider: OPENAI_PROVIDER.to_owned(),
            model: model.id.clone(),
            capability: RequestedCapability::NativeCompaction,
        }),
    }
}

fn validate_with_shared_adapter(
    adapter: &OpenAiResponsesAdapter,
    draft: &RequestDraft,
    model: &heycode_llm::ModelDescriptor,
) -> Result<(), ResolveError> {
    let mut validation = draft.clone();
    validation.native_features.clear();
    validation.inputs = validation
        .inputs
        .into_iter()
        .map(|input| match input {
            InferenceInput::ProviderState(state)
                if state.data().get("type").and_then(serde_json::Value::as_str)
                    == Some("message")
                    && state.data().get("role").and_then(serde_json::Value::as_str)
                        == Some("user") =>
            {
                InferenceInput::Message(ChatMessage::user("compacted user boundary"))
            }
            other => other,
        })
        .collect();
    adapter.resolve(validation, model).map(|_| ())
}

fn contains_compaction_state(inputs: &[InferenceInput]) -> bool {
    inputs.iter().any(|input| {
        matches!(
            input,
            InferenceInput::ProviderState(state)
                if state.data().get("type").and_then(serde_json::Value::as_str)
                    == Some("compaction")
        )
    })
}

fn valid_compacted_user_message(data: &serde_json::Value) -> bool {
    data.get("status").and_then(serde_json::Value::as_str) == Some("completed")
        && data
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|content| !content.is_empty())
}

fn compaction_stream_error() -> InferenceStream {
    Box::pin(futures::stream::once(async {
        Err(LlmError::InvalidResponse(
            "OpenAI native compaction must use the buffered compaction operation".to_owned(),
        ))
    }))
}

fn invalid_state(message: &str) -> ResolveError {
    ResolveError::InvalidRequest {
        field: "provider_state",
        message: message.to_owned(),
    }
}

/// Build the official route behind an `Arc` for registry contribution.
///
/// # Errors
/// Same contract as [`OpenAiProvider::new`].
pub fn openai_provider(
    http: HttpService,
    api_key: impl Into<String>,
    default_model: Option<String>,
) -> Result<Arc<dyn Provider>, LlmError> {
    Ok(Arc::new(OpenAiProvider::new(http, api_key, default_model)?))
}
