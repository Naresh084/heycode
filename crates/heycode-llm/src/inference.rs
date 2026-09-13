//! Explicit provider-owned request resolution before transport dispatch.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::{
    CapabilitySupport, ChatMessage, FinishReason, LlmError, ModelDescriptor, ProviderDescriptor,
    ProviderProtocol, RetrySpec, Role, TokenUsage, ToolSpec,
};
use heycode_core::{ProviderRequestOption, ProviderStateItem};

/// Normalized native-inference stream.
pub type InferenceStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<InferenceEvent, LlmError>> + Send>>;

/// Opaque provider-owned reasoning effort id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReasoningEffortId(String);

impl ReasoningEffortId {
    /// Validate one non-blank bounded effort id without rewriting it.
    ///
    /// # Errors
    /// Blank, surrounding-whitespace, control-bearing or overlong ids are
    /// invalid request data.
    pub fn new(value: impl Into<String>) -> Result<Self, ResolveError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 128
            || value.chars().any(char::is_control)
        {
            return Err(ResolveError::InvalidRequest {
                field: "reasoning_effort",
                message: "reasoning effort must be 1..=128 bytes with no surrounding whitespace or control characters"
                    .to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// Borrow the provider-owned id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_built_in(value: &'static str) -> Self {
        Self(value.to_owned())
    }
}

/// Exact reasoning-effort vocabulary owned by one adapter route.
///
/// Choice order is presentation order and values are never normalized. `None`
/// from [`InferenceAdapter::reasoning_effort_options`] means the route does not
/// expose a selectable effort control; it is distinct from a guessed empty
/// vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortOptions {
    choices: Vec<ReasoningEffortId>,
    default: Option<ReasoningEffortId>,
}

impl ReasoningEffortOptions {
    /// Validate one non-empty, duplicate-free adapter-owned vocabulary.
    ///
    /// # Errors
    /// Empty or duplicate choices, or a default outside the choice list, are
    /// invalid adapter metadata.
    pub fn new(
        choices: Vec<ReasoningEffortId>,
        default: Option<ReasoningEffortId>,
    ) -> Result<Self, ResolveError> {
        if choices.is_empty() {
            return Err(ResolveError::InvalidAdapter {
                field: "reasoning_efforts",
                message: "reasoning effort options must contain at least one exact choice"
                    .to_owned(),
            });
        }
        let mut unique = BTreeSet::new();
        for choice in &choices {
            if !unique.insert(choice.as_str()) {
                return Err(ResolveError::InvalidAdapter {
                    field: "reasoning_efforts",
                    message: format!("duplicate reasoning effort `{}`", choice.as_str()),
                });
            }
        }
        if let Some(default) = &default
            && !unique.contains(default.as_str())
        {
            return Err(ResolveError::InvalidAdapter {
                field: "default_reasoning_effort",
                message: "default reasoning effort is not in the exact-route effort list"
                    .to_owned(),
            });
        }
        Ok(Self { choices, default })
    }

    /// Project adapter metadata only when the exact model proves reasoning
    /// support. Invalid adapter metadata still fails even for a non-reasoning
    /// model so callers cannot silently hide a broken route definition.
    ///
    /// # Errors
    /// Duplicate/default-invalid adapter metadata.
    pub fn for_model(
        model: &ModelDescriptor,
        choices: Vec<ReasoningEffortId>,
        default: Option<ReasoningEffortId>,
    ) -> Result<Option<Self>, ResolveError> {
        if choices.is_empty() && default.is_none() {
            return Ok(None);
        }
        let options = Self::new(choices, default)?;
        Ok((model.capabilities.reasoning == CapabilitySupport::Supported).then_some(options))
    }

    /// Exact choices in adapter display order.
    #[must_use]
    pub fn choices(&self) -> &[ReasoningEffortId] {
        &self.choices
    }

    /// Explicit adapter default. `None` means omission leaves the choice to the
    /// provider rather than naming a local default.
    #[must_use]
    pub const fn default(&self) -> Option<&ReasoningEffortId> {
        self.default.as_ref()
    }
}

/// Opaque non-secret credential reference captured for later operation-time
/// resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialHandle(String);

impl CredentialHandle {
    /// Validate a non-blank reference without resolving a secret.
    ///
    /// # Errors
    /// Blank or surrounding-whitespace references are invalid adapter data.
    pub fn new(value: impl Into<String>) -> Result<Self, ResolveError> {
        let value = value.into();
        if value.is_empty() || value.trim() != value {
            return Err(ResolveError::InvalidAdapter {
                field: "authentication",
                message: "credential reference must be non-blank with no surrounding whitespace"
                    .to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// A validated credential reference is always a valid handle: it is
    /// non-blank and carries no whitespace by construction, so this
    /// conversion cannot fail.
    #[must_use]
    pub fn from_reference(reference: &heycode_credentials::CredentialReference) -> Self {
        Self(reference.as_str().to_owned())
    }

    /// Borrow the non-secret reference.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Marker that an adapter instance already owns its authentication binding.
/// It carries no secret material.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdapterOwnedAuth(());

impl AdapterOwnedAuth {
    /// Construct the secret-free marker.
    #[must_use]
    pub const fn new() -> Self {
        Self(())
    }
}

/// Authentication binding retained by a resolved call without secret bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationBinding {
    /// Endpoint requires no authentication.
    None,
    /// Current adapter instance owns an already-bound credential.
    AdapterOwned(AdapterOwnedAuth),
    /// Resolve this non-secret reference at operation time.
    Credential(CredentialHandle),
    /// Provider SDK/host ambient identity chain.
    Ambient,
}

/// Transport target chosen by an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceTarget {
    /// HTTP API rooted at this version/base URL.
    Http {
        /// Absolute `http` or `https` base URL without embedded credentials.
        base_url: String,
    },
    /// SDK-managed service route such as Bedrock or Vertex.
    ManagedService {
        /// Stable service name.
        service: String,
        /// Optional region/location.
        location: Option<String>,
    },
}

/// Purpose of one model request for policy, logging and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPurpose {
    /// Normal user conversation/agent step.
    Conversation,
    /// Auxiliary session-title generation.
    SessionTitle,
    /// Portable compaction summary generation.
    Compaction,
    /// Provider conformance or product evaluation call.
    Evaluation,
}

/// Input modalities materially present in a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputModality {
    /// Text messages/system prompt.
    Text,
    /// Image attachment/input part.
    Image,
    /// Native document/file attachment part.
    Document,
}

/// Provider-native features requested for this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeFeature {
    /// Provider-hosted web search/fetch.
    Web,
    /// Provider-native compaction/context editing.
    Compaction,
    /// Provider-managed prompt-prefix cache controls.
    PromptCache,
}

/// One ordered provider-visible input entry.
#[derive(Debug, Clone, PartialEq)]
pub enum InferenceInput {
    /// Provider-neutral conversation message.
    Message(ChatMessage),
    /// Lossless provider-owned item replayed in chronological position.
    ProviderState(ProviderStateItem),
}

/// Normalized output item phase kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamItemKind {
    /// Assistant message item.
    Message,
    /// Reasoning/state item.
    Reasoning,
    /// Client function call item.
    FunctionCall,
    /// Other provider item retained losslessly.
    Other(String),
}

/// Normalized event stream emitted by native inference adapters.
#[derive(Debug, Clone, PartialEq)]
pub enum InferenceEvent {
    /// Provider response lifecycle began.
    ResponseStarted {
        /// Provider response id.
        response_id: String,
    },
    /// One ordered provider output item began.
    ItemStarted {
        /// Output array index.
        output_index: u32,
        /// Provider item id.
        item_id: String,
        /// Normalized item kind.
        kind: StreamItemKind,
    },
    /// Visible assistant text delta.
    TextDelta(String),
    /// Safe reasoning summary/thinking delta. Empty text may signal observed
    /// reasoning activity when the provider supplies only opaque state.
    ReasoningDelta(String),
    /// Client function call argument delta.
    ToolCallDelta {
        /// Output array index.
        output_index: u32,
        /// Call id on the first delta.
        id: Option<heycode_core::CallId>,
        /// Function name on the first delta.
        name: Option<String>,
        /// Raw JSON argument fragment.
        arguments_delta: String,
    },
    /// Completed provider-executed tool call; never dispatched as a client tool.
    ServerToolCall {
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized call metadata.
        call: heycode_core::ServerToolCall,
    },
    /// Completed provider-executed tool result without raw response data.
    ServerToolResult {
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized result metadata.
        result: heycode_core::ServerToolResult,
    },
    /// Provider-reported aggregate server-tool usage without synthetic calls.
    ServerToolUsage(heycode_core::ServerToolUsage),
    /// Public URL citation attached to assistant output.
    Citation {
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized citation metadata.
        citation: heycode_core::UrlCitation,
    },
    /// One ordered provider output item completed.
    ItemFinished {
        /// Output array index.
        output_index: u32,
        /// Provider item id.
        item_id: String,
        /// Normalized item kind.
        kind: StreamItemKind,
    },
    /// Lossless provider-owned continuation state.
    ProviderState(ProviderStateItem),
    /// Provider response lifecycle completed before terminal usage/finish.
    ResponseFinished {
        /// Provider response id.
        response_id: String,
        /// Provider status string.
        status: String,
    },
    /// Validated detailed cache/context-edit facts for this successful response.
    ResponseMetadata(heycode_core::ProviderResponseMetadata),
    /// Token usage, always immediately before [`Self::Finish`] when present.
    Usage(TokenUsage),
    /// Terminal successful/incomplete finish.
    Finish(FinishReason),
}

/// Exact normalized capability whose request could not be proven valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedCapability {
    /// Client tool/function calling.
    Tools,
    /// Image input.
    ImageInput,
    /// Native document/file input.
    DocumentInput,
    /// Reasoning effort/state.
    Reasoning,
    /// JSON-schema structured output.
    StructuredOutput,
    /// Provider-hosted web capability.
    NativeWeb,
    /// Provider-native compaction/context editing.
    NativeCompaction,
    /// Prompt-prefix caching.
    PromptCache,
}

/// Durable-projection request proposal before adapter validation/defaulting.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestDraft {
    /// Selected provider route.
    pub provider: String,
    /// Selected model id or provider alias.
    pub model: String,
    /// Catalog generation consulted, when any.
    pub catalog_revision: Option<u64>,
    /// Commit timestamp of the consulted catalog generation.
    pub catalog_fetched_at_ms: Option<u64>,
    /// Explicit lifecycle comparison instant in Unix milliseconds.
    pub effective_at_ms: u64,
    /// Rendered system prompt slot.
    pub system: Option<String>,
    /// Ordered messages and compatible provider items in exact chronology.
    pub inputs: Vec<InferenceInput>,
    /// Client tool schemas offered to the model.
    pub tools: Vec<ToolSpec>,
    /// Modalities materially present in the request.
    pub input_modalities: Vec<InputModality>,
    /// Explicit reasoning effort, or none for adapter/provider default.
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// JSON Schema object for structured output.
    pub structured_output: Option<serde_json::Value>,
    /// Provider-native features requested by policy.
    pub native_features: Vec<NativeFeature>,
    /// Resolved logical native-tool implementation selections.
    pub native_tool_routes: Vec<heycode_core::NativeToolRoute>,
    /// Provider-owned, schema-tagged request options proposed by the selected
    /// provider implementation.
    pub provider_options: Vec<ProviderRequestOption>,
    /// Sampling temperature override.
    pub temperature: Option<f32>,
    /// Explicit output token cap.
    pub max_output_tokens: Option<u64>,
    /// Request purpose.
    pub purpose: CallPurpose,
}

/// Adapter-owned exact-route choices and defaults used during resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveSpec {
    /// Wire protocol chosen for this exact route.
    pub protocol: ProviderProtocol,
    /// Validated transport target proposal.
    pub target: InferenceTarget,
    /// Secret-free authentication binding.
    pub authentication: AuthenticationBinding,
    /// Adapter-configured output default, or provider-owned default when none.
    pub default_max_output_tokens: Option<u64>,
    /// Exact accepted reasoning effort ids in adapter display order.
    pub reasoning_efforts: Vec<ReasoningEffortId>,
    /// Adapter-configured reasoning default, or provider-owned when none.
    pub default_reasoning_effort: Option<ReasoningEffortId>,
}

/// Fields materialized by adapter resolution instead of explicitly requested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolvedDefaults {
    /// Reasoning effort came from the adapter default.
    pub reasoning_effort: bool,
    /// Output cap came from the adapter default.
    pub max_output_tokens: bool,
}

/// One validated call. Fields are private so callers cannot bypass
/// [`InferenceAdapter::resolve`]; consuming dispatch makes the handle one-shot
/// by ownership.
#[derive(Debug, PartialEq)]
pub struct ResolvedCall {
    provider: String,
    model: String,
    catalog_revision: Option<u64>,
    catalog_fetched_at_ms: Option<u64>,
    context_window: Option<u64>,
    model_max_output_tokens: Option<u64>,
    effective_at_ms: u64,
    protocol: ProviderProtocol,
    target: InferenceTarget,
    authentication: AuthenticationBinding,
    system: Option<String>,
    inputs: Vec<InferenceInput>,
    tools: Vec<ToolSpec>,
    input_modalities: Vec<InputModality>,
    reasoning_effort: Option<ReasoningEffortId>,
    structured_output: Option<serde_json::Value>,
    native_features: Vec<NativeFeature>,
    native_tool_routes: Vec<heycode_core::NativeToolRoute>,
    provider_options: Vec<ProviderRequestOption>,
    temperature: Option<f32>,
    max_output_tokens: Option<u64>,
    purpose: CallPurpose,
    defaults: ResolvedDefaults,
    retry_spec: RetrySpec,
}

impl ResolvedCall {
    /// Provider route.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Catalog generation consulted, when any.
    #[must_use]
    pub const fn catalog_revision(&self) -> Option<u64> {
        self.catalog_revision
    }

    /// Catalog generation commit timestamp.
    #[must_use]
    pub const fn catalog_fetched_at_ms(&self) -> Option<u64> {
        self.catalog_fetched_at_ms
    }

    /// Exact-model context capacity when known.
    #[must_use]
    pub const fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    /// Exact-model output maximum when known.
    #[must_use]
    pub const fn model_max_output_tokens(&self) -> Option<u64> {
        self.model_max_output_tokens
    }

    /// Explicit lifecycle comparison instant.
    #[must_use]
    pub const fn effective_at_ms(&self) -> u64 {
        self.effective_at_ms
    }

    /// Resolved wire protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// Resolved transport target.
    #[must_use]
    pub const fn target(&self) -> &InferenceTarget {
        &self.target
    }

    /// Secret-free authentication binding.
    #[must_use]
    pub const fn authentication(&self) -> &AuthenticationBinding {
        &self.authentication
    }

    /// Rendered system prompt.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    /// Ordered provider-visible messages/state.
    #[must_use]
    pub fn inputs(&self) -> &[InferenceInput] {
        &self.inputs
    }

    /// Validated client tool schemas.
    #[must_use]
    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Normalized input modalities.
    #[must_use]
    pub fn input_modalities(&self) -> &[InputModality] {
        &self.input_modalities
    }

    /// Effective reasoning effort.
    #[must_use]
    pub const fn reasoning_effort(&self) -> Option<&ReasoningEffortId> {
        self.reasoning_effort.as_ref()
    }

    /// Structured-output JSON Schema.
    #[must_use]
    pub const fn structured_output(&self) -> Option<&serde_json::Value> {
        self.structured_output.as_ref()
    }

    /// Normalized provider-native features.
    #[must_use]
    pub fn native_features(&self) -> &[NativeFeature] {
        &self.native_features
    }

    /// Selected logical native-tool implementations.
    #[must_use]
    pub fn native_tool_routes(&self) -> &[heycode_core::NativeToolRoute] {
        &self.native_tool_routes
    }

    /// Validated provider-owned request options.
    #[must_use]
    pub fn provider_options(&self) -> &[ProviderRequestOption] {
        &self.provider_options
    }

    /// Sampling temperature override.
    #[must_use]
    pub const fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    /// Effective output cap.
    #[must_use]
    pub const fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// Request purpose.
    #[must_use]
    pub const fn purpose(&self) -> CallPurpose {
        self.purpose
    }

    /// Which fields the adapter defaulted.
    #[must_use]
    pub const fn defaults(&self) -> ResolvedDefaults {
        self.defaults
    }

    /// Explicit retry policy resolved before dispatch.
    #[must_use]
    pub const fn retry_spec(&self) -> &RetrySpec {
        &self.retry_spec
    }

    /// Replace the retry policy resolution derived from the draft.
    ///
    /// Resolution hands every call a bounded policy already, so this exists for
    /// the adapter that owns evidence resolution cannot see: a configured
    /// policy of its own, or protocol-specific proof that this exact request is
    /// not replayable. Out-of-crate adapters composing through
    /// [`resolve_request`] use it for the same reason the in-crate ones do.
    #[must_use]
    pub fn with_retry_spec(mut self, retry_spec: RetrySpec) -> Self {
        self.retry_spec = retry_spec;
        self
    }
}

/// Request resolution failures. Every variant occurs before transport.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// Draft route does not belong to this adapter.
    #[error("provider `{requested}` does not match adapter `{adapter}`")]
    ProviderMismatch {
        /// Draft provider.
        requested: String,
        /// Adapter provider.
        adapter: String,
    },
    /// Descriptor does not represent the requested model id/alias.
    #[error("model `{requested}` does not match resolved descriptor `{resolved}`")]
    ModelMismatch {
        /// Draft model id/alias.
        requested: String,
        /// Canonical descriptor id.
        resolved: String,
    },
    /// Chosen protocol is not declared by this provider.
    #[error("provider `{provider}` does not declare protocol {protocol:?}")]
    ProtocolUnsupported {
        /// Provider id.
        provider: String,
        /// Requested protocol.
        protocol: ProviderProtocol,
    },
    /// Model is explicitly or effectively retired.
    #[error("model `{model}` for provider `{provider}` is retired")]
    RetiredModel {
        /// Provider id.
        provider: String,
        /// Canonical model id.
        model: String,
        /// Retirement instant when known.
        retirement_at_ms: Option<u64>,
    },
    /// Descriptor explicitly denies a requested capability.
    #[error("provider `{provider}` model `{model}` does not support {capability:?}")]
    Unsupported {
        /// Provider id.
        provider: String,
        /// Canonical model id.
        model: String,
        /// Denied capability.
        capability: RequestedCapability,
    },
    /// Descriptor has no trustworthy evidence for a requested capability.
    #[error("provider `{provider}` model `{model}` has unproven {capability:?} support")]
    Unproven {
        /// Provider id.
        provider: String,
        /// Canonical model id.
        model: String,
        /// Unproven capability.
        capability: RequestedCapability,
    },
    /// Requested reasoning id is not in exact adapter-owned choices.
    #[error("reasoning effort `{requested:?}` is unsupported; available: {available:?}")]
    UnsupportedReasoningEffort {
        /// Exact requested id.
        requested: ReasoningEffortId,
        /// Exact available ids in adapter order.
        available: Vec<ReasoningEffortId>,
    },
    /// Explicit output cap exceeds the model maximum.
    #[error("requested output cap {requested} exceeds model maximum {maximum}")]
    OutputLimitExceeded {
        /// Explicit requested cap.
        requested: u64,
        /// Descriptor maximum.
        maximum: u64,
    },
    /// Caller proposal is structurally invalid.
    #[error("invalid request field `{field}`: {message}")]
    InvalidRequest {
        /// Stable field name.
        field: &'static str,
        /// Safe actionable detail.
        message: String,
    },
    /// Adapter resolution metadata is structurally invalid.
    #[error("invalid adapter field `{field}`: {message}")]
    InvalidAdapter {
        /// Stable field name.
        field: &'static str,
        /// Safe actionable detail.
        message: String,
    },
}

/// Native inference adapter. Delegated coding-agent runtimes use a different
/// top-level contract.
pub trait InferenceAdapter: Send + Sync {
    /// Provider identity/protocol families owned by this adapter.
    fn descriptor(&self) -> ProviderDescriptor;

    /// Secret-free authentication binding proposed for every call on this
    /// adapter instance. Resolution must return the same binding.
    fn authentication_binding(&self) -> AuthenticationBinding;

    /// Exact selectable effort values for this resolved model.
    ///
    /// The default is unsupported. Implementations must return only values
    /// accepted by the same route configuration used by [`Self::resolve`].
    ///
    /// # Errors
    /// Structurally invalid adapter metadata fails before UI publication.
    fn reasoning_effort_options(
        &self,
        _model: &ModelDescriptor,
    ) -> Result<Option<ReasoningEffortOptions>, ResolveError> {
        Ok(None)
    }

    /// Validate/default one durable request draft for an exact model.
    ///
    /// # Errors
    /// Any unsupported, unproven, retired or malformed choice fails before
    /// transport construction.
    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError>;

    /// Consume one resolved call and begin normalized streaming.
    fn stream(&self, call: ResolvedCall) -> InferenceStream;

    /// Consume one resolved call with caller-owned cancellation. Current
    /// protocol adapters override this so the token owns HTTP attempts and
    /// retry waits; compatibility adapters retain pre-cancel behavior.
    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        if cancellation.is_cancelled() {
            Box::pin(futures::stream::once(async {
                Err(crate::retry::cancelled_error())
            }))
        } else {
            self.stream(call)
        }
    }

    /// Optional provider-native compaction operation for this exact adapter.
    ///
    /// Capability evidence in a model descriptor and operation availability
    /// are independent: both must exist before a Consumer may dispatch.
    fn native_compaction(&self) -> Option<&dyn crate::NativeCompactionAdapter> {
        None
    }
}

/// Apply provider-neutral invariants for an adapter-owned exact-route spec.
/// Adapters must perform any additional protocol/model-specific validation
/// before returning the result.
///
/// # Errors
/// Provider/model/protocol mismatch, retirement, unsupported or unproven
/// capabilities, invalid schemas/limits/target/defaults.
pub fn resolve_request(
    provider: &ProviderDescriptor,
    draft: RequestDraft,
    model: &ModelDescriptor,
    spec: &ResolveSpec,
) -> Result<ResolvedCall, ResolveError> {
    resolve_request_with_tool_admission(
        provider,
        draft,
        model,
        spec,
        ToolAdmission::RequireEvidence,
    )
}

/// Route-owned admission policy; it never changes reported model evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ToolAdmission {
    /// Require proven tool capability before dispatch.
    #[default]
    RequireEvidence,
    /// Permit an unknown capability without admitting known lack of support.
    AttemptUnknown,
}

/// Resolve the same durable request under an explicit route tool policy.
///
/// # Errors
/// Invalid route, request, schema, limits, or unsupported/unproven capabilities.
pub(crate) fn resolve_request_with_tool_admission(
    provider: &ProviderDescriptor,
    draft: RequestDraft,
    model: &ModelDescriptor,
    spec: &ResolveSpec,
    tool_admission: ToolAdmission,
) -> Result<ResolvedCall, ResolveError> {
    validate_route(provider, &draft, model, spec)?;
    if draft.catalog_revision.is_some() != draft.catalog_fetched_at_ms.is_some()
        || draft.catalog_revision == Some(0)
        || draft.catalog_fetched_at_ms == Some(0)
    {
        return Err(ResolveError::InvalidRequest {
            field: "catalog_revision",
            message: "catalog revision and positive fetched timestamp must be paired".to_owned(),
        });
    }
    validate_provider_state(provider, model, spec, &draft.inputs)?;
    validate_target(&spec.target)?;
    validate_tools(&draft.tools)?;
    validate_temperature(draft.temperature)?;
    validate_provider_options(provider, &draft.provider_options)?;
    validate_native_tool_routes(provider, &draft.native_tool_routes)?;
    let mut image_count = 0_usize;
    let mut document_count = 0_usize;
    let mut document_bytes = 0_usize;
    for input in &draft.inputs {
        if let InferenceInput::Message(message) = input {
            if (!message.images.is_empty() || !message.documents.is_empty())
                && message.role != Role::User
            {
                return Err(ResolveError::InvalidRequest {
                    field: "inputs",
                    message: "media input is permitted only on user messages".to_owned(),
                });
            }
            image_count = image_count
                .checked_add(message.images.len())
                .ok_or_else(|| ResolveError::InvalidRequest {
                    field: "inputs",
                    message: "image input count exceeds the supported limit".to_owned(),
                })?;
            document_count = document_count
                .checked_add(message.documents.len())
                .ok_or_else(|| ResolveError::InvalidRequest {
                    field: "inputs",
                    message: "document input count exceeds the supported limit".to_owned(),
                })?;
            for document in &message.documents {
                document_bytes = document_bytes
                    .checked_add(document.bytes().len())
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "inputs",
                        message: "document input bytes exceed the supported limit".to_owned(),
                    })?;
            }
        }
    }
    if image_count > 16
        || draft.input_modalities.contains(&InputModality::Image) != (image_count != 0)
    {
        return Err(ResolveError::InvalidRequest {
            field: "input_modalities",
            message: "image modality must exactly match one to sixteen user images".to_owned(),
        });
    }
    if document_count > 4
        || document_bytes > 32 * 1024 * 1024
        || draft.input_modalities.contains(&InputModality::Document) != (document_count != 0)
    {
        return Err(ResolveError::InvalidRequest {
            field: "input_modalities",
            message: "document modality must exactly match one to four bounded user documents"
                .to_owned(),
        });
    }
    if draft.input_modalities.is_empty() {
        return Err(ResolveError::InvalidRequest {
            field: "input_modalities",
            message: "at least one material input modality is required".to_owned(),
        });
    }
    if let Some(schema) = &draft.structured_output
        && !schema.is_object()
    {
        return Err(ResolveError::InvalidRequest {
            field: "structured_output",
            message: "JSON Schema must be an object".to_owned(),
        });
    }

    if !draft.tools.is_empty()
        && !(tool_admission == ToolAdmission::AttemptUnknown
            && model.capabilities.tools == crate::CapabilitySupport::Unknown)
    {
        require_capability(provider, model, RequestedCapability::Tools)?;
    }
    for modality in &draft.input_modalities {
        match modality {
            InputModality::Text => {}
            InputModality::Image => {
                require_capability(provider, model, RequestedCapability::ImageInput)?;
            }
            InputModality::Document => {
                require_capability(provider, model, RequestedCapability::DocumentInput)?;
            }
        }
    }
    if draft.structured_output.is_some() {
        require_capability(provider, model, RequestedCapability::StructuredOutput)?;
    }
    for feature in &draft.native_features {
        let capability = match feature {
            NativeFeature::Web => RequestedCapability::NativeWeb,
            NativeFeature::Compaction => RequestedCapability::NativeCompaction,
            NativeFeature::PromptCache => RequestedCapability::PromptCache,
        };
        require_capability(provider, model, capability)?;
    }

    validate_reasoning_spec(spec)?;
    let (reasoning_effort, defaulted_reasoning) = resolve_reasoning(provider, model, &draft, spec)?;
    let (max_output_tokens, defaulted_output) = resolve_output_limit(model, &draft, spec)?;
    let input_modalities = unique_sorted(draft.input_modalities);
    let native_features = unique_sorted(draft.native_features);
    let retry_spec = resolved_retry_spec(&native_features, &draft.native_tool_routes);

    Ok(ResolvedCall {
        provider: provider.id.clone(),
        model: model.id.clone(),
        catalog_revision: draft.catalog_revision,
        catalog_fetched_at_ms: draft.catalog_fetched_at_ms,
        context_window: model.context_window,
        model_max_output_tokens: model.max_output_tokens,
        effective_at_ms: draft.effective_at_ms,
        protocol: spec.protocol,
        target: spec.target.clone(),
        authentication: spec.authentication.clone(),
        system: draft.system,
        inputs: draft.inputs,
        tools: draft.tools,
        input_modalities,
        reasoning_effort,
        structured_output: draft.structured_output,
        native_features,
        native_tool_routes: draft.native_tool_routes,
        provider_options: draft.provider_options,
        temperature: draft.temperature,
        max_output_tokens,
        purpose: draft.purpose,
        defaults: ResolvedDefaults {
            reasoning_effort: defaulted_reasoning,
            max_output_tokens: defaulted_output,
        },
        retry_spec,
    })
}

/// Bounded retry policy every resolved call carries, derived from the only
/// replay evidence resolution owns.
///
/// A request that asks the provider to execute work of its own — a native
/// feature, or a provider-executed native tool route — is not proven safe to
/// send twice, so its policy keeps the standard bounds with replay withheld.
/// Anything an adapter knows beyond this it applies with
/// [`ResolvedCall::with_retry_spec`].
fn resolved_retry_spec(
    native_features: &[NativeFeature],
    native_tool_routes: &[heycode_core::NativeToolRoute],
) -> RetrySpec {
    let provider_executed = !native_features.is_empty()
        || native_tool_routes
            .iter()
            .any(|route| route.kind() == heycode_core::NativeToolImplementationKind::Provider);
    if provider_executed {
        RetrySpec::standard().disable_replay()
    } else {
        RetrySpec::standard()
    }
}

fn validate_provider_options(
    provider: &ProviderDescriptor,
    options: &[ProviderRequestOption],
) -> Result<(), ResolveError> {
    let mut kinds = BTreeSet::new();
    for option in options {
        option
            .validate()
            .map_err(|error| ResolveError::InvalidRequest {
                field: "provider_options",
                message: error.to_string(),
            })?;
        if option.provider() != provider.id {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "provider option owner does not match the selected provider".to_owned(),
            });
        }
        if !kinds.insert(option.kind()) {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "provider option kinds must be unique".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_native_tool_routes(
    provider: &ProviderDescriptor,
    routes: &[heycode_core::NativeToolRoute],
) -> Result<(), ResolveError> {
    let mut logical = BTreeSet::new();
    let mut prior: Option<&str> = None;
    for route in routes {
        route
            .validate()
            .map_err(|error| ResolveError::InvalidRequest {
                field: "native_tool_routes",
                message: error.to_string(),
            })?;
        if route.kind() == heycode_core::NativeToolImplementationKind::Provider
            && route.provider() != Some(provider.id.as_str())
        {
            return Err(ResolveError::InvalidRequest {
                field: "native_tool_routes",
                message: "provider-native tool route owner does not match request provider"
                    .to_owned(),
            });
        }
        if !logical.insert(route.logical()) || prior.is_some_and(|prior| prior >= route.logical()) {
            return Err(ResolveError::InvalidRequest {
                field: "native_tool_routes",
                message: "logical native-tool routes must be unique and sorted".to_owned(),
            });
        }
        prior = Some(route.logical());
    }
    Ok(())
}

fn validate_provider_state(
    provider: &ProviderDescriptor,
    model: &ModelDescriptor,
    spec: &ResolveSpec,
    inputs: &[InferenceInput],
) -> Result<(), ResolveError> {
    for item in inputs.iter().filter_map(|input| match input {
        InferenceInput::Message(_) => None,
        InferenceInput::ProviderState(item) => Some(item),
    }) {
        if item.provider() != provider.id
            || item.model() != model.id
            || item.protocol() != spec.protocol
            || item.schema_version() != 1
        {
            return Err(ResolveError::InvalidRequest {
                field: "provider_state",
                message: "provider state route/protocol/schema does not match the resolved call"
                    .to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_route(
    provider: &ProviderDescriptor,
    draft: &RequestDraft,
    model: &ModelDescriptor,
    spec: &ResolveSpec,
) -> Result<(), ResolveError> {
    if draft.provider != provider.id {
        return Err(ResolveError::ProviderMismatch {
            requested: draft.provider.clone(),
            adapter: provider.id.clone(),
        });
    }
    if draft.model != model.id && !model.aliases.iter().any(|alias| alias == &draft.model) {
        return Err(ResolveError::ModelMismatch {
            requested: draft.model.clone(),
            resolved: model.id.clone(),
        });
    }
    if !provider.protocols.contains(&spec.protocol) {
        return Err(ResolveError::ProtocolUnsupported {
            provider: provider.id.clone(),
            protocol: spec.protocol,
        });
    }
    if !model.lifecycle.is_selectable(draft.effective_at_ms) {
        return Err(ResolveError::RetiredModel {
            provider: provider.id.clone(),
            model: model.id.clone(),
            retirement_at_ms: model.lifecycle.retirement_at_ms,
        });
    }
    Ok(())
}

fn validate_target(target: &InferenceTarget) -> Result<(), ResolveError> {
    match target {
        InferenceTarget::Http { base_url } => {
            let url = reqwest::Url::parse(base_url).map_err(|_| ResolveError::InvalidAdapter {
                field: "target",
                message: "HTTP target must be an absolute URL".to_owned(),
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(ResolveError::InvalidAdapter {
                    field: "target",
                    message: "HTTP target must be http(s), host-qualified, credential-free, and have no query/fragment"
                        .to_owned(),
                });
            }
        }
        InferenceTarget::ManagedService { service, location } => {
            if service.is_empty()
                || service.trim() != service
                || location
                    .as_ref()
                    .is_some_and(|value| value.is_empty() || value.trim() != value)
            {
                return Err(ResolveError::InvalidAdapter {
                    field: "target",
                    message:
                        "managed service/location must be non-blank with no surrounding whitespace"
                            .to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn validate_tools(tools: &[ToolSpec]) -> Result<(), ResolveError> {
    let mut names = BTreeSet::new();
    for tool in tools {
        if tool.name.is_empty() || tool.name.trim() != tool.name || !tool.parameters.is_object() {
            return Err(ResolveError::InvalidRequest {
                field: "tools",
                message: "tool names must be non-blank and parameters must be JSON Schema objects"
                    .to_owned(),
            });
        }
        if !names.insert(tool.name.as_str()) {
            return Err(ResolveError::InvalidRequest {
                field: "tools",
                message: format!("duplicate tool name `{}`", tool.name),
            });
        }
    }
    Ok(())
}

fn validate_temperature(temperature: Option<f32>) -> Result<(), ResolveError> {
    if temperature.is_some_and(|value| !value.is_finite()) {
        return Err(ResolveError::InvalidRequest {
            field: "temperature",
            message: "temperature must be finite".to_owned(),
        });
    }
    Ok(())
}

fn validate_reasoning_spec(spec: &ResolveSpec) -> Result<(), ResolveError> {
    let mut efforts = BTreeSet::new();
    for effort in &spec.reasoning_efforts {
        if !efforts.insert(effort.as_str()) {
            return Err(ResolveError::InvalidAdapter {
                field: "reasoning_efforts",
                message: format!("duplicate reasoning effort `{}`", effort.as_str()),
            });
        }
    }
    if let Some(default) = &spec.default_reasoning_effort
        && !efforts.contains(default.as_str())
    {
        return Err(ResolveError::InvalidAdapter {
            field: "default_reasoning_effort",
            message: "default reasoning effort is not in the exact-route effort list".to_owned(),
        });
    }
    Ok(())
}

fn resolve_reasoning(
    provider: &ProviderDescriptor,
    model: &ModelDescriptor,
    draft: &RequestDraft,
    spec: &ResolveSpec,
) -> Result<(Option<ReasoningEffortId>, bool), ResolveError> {
    if let Some(requested) = &draft.reasoning_effort {
        require_capability(provider, model, RequestedCapability::Reasoning)?;
        if !spec.reasoning_efforts.contains(requested) {
            return Err(ResolveError::UnsupportedReasoningEffort {
                requested: requested.clone(),
                available: spec.reasoning_efforts.clone(),
            });
        }
        return Ok((Some(requested.clone()), false));
    }
    if model.capabilities.reasoning == CapabilitySupport::Supported
        && let Some(default) = &spec.default_reasoning_effort
    {
        return Ok((Some(default.clone()), true));
    }
    Ok((None, false))
}

fn resolve_output_limit(
    model: &ModelDescriptor,
    draft: &RequestDraft,
    spec: &ResolveSpec,
) -> Result<(Option<u64>, bool), ResolveError> {
    if draft.max_output_tokens == Some(0) {
        return Err(ResolveError::InvalidRequest {
            field: "max_output_tokens",
            message: "output cap must be positive".to_owned(),
        });
    }
    if spec.default_max_output_tokens == Some(0) {
        return Err(ResolveError::InvalidAdapter {
            field: "default_max_output_tokens",
            message: "adapter output default must be positive".to_owned(),
        });
    }
    let (resolved, defaulted) = match draft.max_output_tokens {
        Some(value) => (Some(value), false),
        None => (
            spec.default_max_output_tokens,
            spec.default_max_output_tokens.is_some(),
        ),
    };
    if let (Some(requested), Some(maximum)) = (resolved, model.max_output_tokens)
        && requested > maximum
    {
        return if defaulted {
            Err(ResolveError::InvalidAdapter {
                field: "default_max_output_tokens",
                message: format!(
                    "adapter output default {requested} exceeds model maximum {maximum}"
                ),
            })
        } else {
            Err(ResolveError::OutputLimitExceeded { requested, maximum })
        };
    }
    Ok((resolved, defaulted))
}

fn require_capability(
    provider: &ProviderDescriptor,
    model: &ModelDescriptor,
    capability: RequestedCapability,
) -> Result<(), ResolveError> {
    let support = match capability {
        RequestedCapability::Tools => model.capabilities.tools,
        RequestedCapability::ImageInput => model.capabilities.image_input,
        RequestedCapability::DocumentInput => model.capabilities.document_input,
        RequestedCapability::Reasoning => model.capabilities.reasoning,
        RequestedCapability::StructuredOutput => model.capabilities.structured_output,
        RequestedCapability::NativeWeb => model.capabilities.native_web,
        RequestedCapability::NativeCompaction => model.capabilities.native_compaction,
        RequestedCapability::PromptCache => model.capabilities.prompt_cache,
    };
    match support {
        CapabilitySupport::Supported => Ok(()),
        CapabilitySupport::Unsupported => Err(ResolveError::Unsupported {
            provider: provider.id.clone(),
            model: model.id.clone(),
            capability,
        }),
        CapabilitySupport::Unknown => Err(ResolveError::Unproven {
            provider: provider.id.clone(),
            model: model.id.clone(),
            capability,
        }),
    }
}

fn unique_sorted<T: Ord>(values: Vec<T>) -> Vec<T> {
    values
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
