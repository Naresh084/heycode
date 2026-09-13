//! P10 strict-provider request/response around-middleware.
//!
//! The request chain runs over a C02/C05-bound [`RequestDraft`] before adapter
//! resolution. Only fields represented by the durable request header are
//! mutable here; provider/model/catalog/input chronology remain read-only.
//! The response chain runs over normalized [`InferenceEvent`] values before
//! Agent publishes or commits them. Compatibility Chat dispatch and distinct
//! native-compaction operations do not pretend to cross this exact seam.

use heycode_core::{
    Context, Layer, PluginContributionSpec, ProviderProtocol, Waterfall, WaterfallCompletion,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    AuthenticationBinding, CallPurpose, InferenceEvent, NativeFeature, ProviderErrorClass,
    ProviderRequestOption, ReasoningEffortId, RequestDraft,
};

/// Exact request interception inventory id.
pub const PROVIDER_REQUEST_SEAM: &str = "provider/request";
/// Exact response interception inventory id.
pub const PROVIDER_RESPONSE_SEAM: &str = "provider/response";

const MAX_POLICY_CODE_BYTES: usize = 64;
const MAX_PROVIDER_ID_BYTES: usize = 64;
const MAX_MODEL_ID_BYTES: usize = 256;

/// Which strict provider boundary produced an interception outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInterceptionStage {
    /// Before adapter resolution and durable request commit.
    Request,
    /// After normalized decode and before durable/UI response publication.
    Response,
}

impl ProviderInterceptionStage {
    /// Stable safe stage name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
        }
    }
}

/// Validated body-free policy reason carried across the provider seam.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderInterceptionCode(String);

impl ProviderInterceptionCode {
    /// Validate one lowercase kebab-case policy code.
    ///
    /// # Errors
    /// Empty, oversized or non-kebab-case input is rejected without echoing
    /// the boundary value.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderInterceptionBoundaryError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_POLICY_CODE_BYTES
            && value.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(ProviderInterceptionBoundaryError::InvalidPolicyCode)
        }
    }

    /// Exact safe policy code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderInterceptionCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Boundary validation failures that never echo rejected values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProviderInterceptionBoundaryError {
    /// Policy code is not a bounded lowercase kebab id.
    #[error("provider interception policy code is invalid")]
    InvalidPolicyCode,
    /// Provider id is not a bounded safe identifier.
    #[error("provider interception provider id is invalid")]
    InvalidProvider,
    /// Model id is empty, oversized or contains whitespace/control bytes.
    #[error("provider interception model id is invalid")]
    InvalidModel,
    /// Unknown is not an executable provider protocol.
    #[error("provider interception protocol is invalid")]
    InvalidProtocol,
}

/// Safe terminal failures from a provider interception chain.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderInterceptionError {
    /// A layer deliberately rejected this operation with a validated code.
    #[error("provider {stage} rejected by policy `{code}`", stage = stage.as_str())]
    Rejected {
        /// Boundary that rejected the operation.
        stage: ProviderInterceptionStage,
        /// Safe policy code; no layer body is retained.
        code: ProviderInterceptionCode,
    },
    /// A layer returned an error. Its body is deliberately discarded.
    #[error("provider {stage} interception layer failed", stage = stage.as_str())]
    LayerFailed {
        /// Boundary whose layer failed.
        stage: ProviderInterceptionStage,
    },
    /// A layer returned without `next` and without a typed rejection.
    #[error("provider {stage} interception short-circuited without a policy decision", stage = stage.as_str())]
    UndeclaredShortCircuit {
        /// Boundary whose chain did not reach its terminal delegate.
        stage: ProviderInterceptionStage,
    },
    /// The one caller-owned operation token was cancelled.
    #[error("provider {stage} interception cancelled", stage = stage.as_str())]
    Cancelled {
        /// Boundary cancelled before publication.
        stage: ProviderInterceptionStage,
    },
    /// The supplied safe route context disagreed with the intercepted value.
    #[error("provider {stage} interception context is invalid", stage = stage.as_str())]
    InvalidContext {
        /// Boundary whose context did not match.
        stage: ProviderInterceptionStage,
    },
}

#[derive(Clone)]
enum InterceptionVerdict {
    Continue,
    Reject(ProviderInterceptionCode),
}

/// Mutable strict request decision handed to P10 layers.
///
/// Inputs and route identity are inspection-only. Mutators cover only fields
/// that adapter validation and the durable request header independently
/// preserve before transport.
pub struct ProviderRequestDecision {
    context: ProviderRequestContext,
    draft: RequestDraft,
    cancellation: CancellationToken,
    verdict: InterceptionVerdict,
}

impl ProviderRequestDecision {
    fn new(
        context: ProviderRequestContext,
        draft: RequestDraft,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            context,
            draft,
            cancellation,
            verdict: InterceptionVerdict::Continue,
        }
    }

    /// Safe route/authentication context proposed by the strict adapter.
    #[must_use]
    pub const fn context(&self) -> &ProviderRequestContext {
        &self.context
    }

    /// Read the complete proposed request without gaining mutable access to
    /// route identity or durable input chronology.
    #[must_use]
    pub const fn draft(&self) -> &RequestDraft {
        &self.draft
    }

    /// Caller-owned operation cancellation token.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Current system slot.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        self.draft.system.as_deref()
    }

    /// Replace the system slot. Adapter resolution and C02 logging still
    /// validate and persist the result before dispatch.
    pub fn set_system(&mut self, system: Option<String>) {
        self.draft.system = system;
    }

    /// Mutable client tool schemas represented in the durable header.
    #[must_use]
    pub fn tools_mut(&mut self) -> &mut Vec<heycode_core::ToolSpec> {
        &mut self.draft.tools
    }

    /// Effective reasoning-effort proposal.
    #[must_use]
    pub const fn reasoning_effort(&self) -> Option<&ReasoningEffortId> {
        self.draft.reasoning_effort.as_ref()
    }

    /// Replace the reasoning-effort proposal.
    pub fn set_reasoning_effort(&mut self, value: Option<ReasoningEffortId>) {
        self.draft.reasoning_effort = value;
    }

    /// Mutable structured-output schema.
    #[must_use]
    pub fn structured_output_mut(&mut self) -> &mut Option<serde_json::Value> {
        &mut self.draft.structured_output
    }

    /// Mutable provider-native feature list.
    #[must_use]
    pub fn native_features_mut(&mut self) -> &mut Vec<NativeFeature> {
        &mut self.draft.native_features
    }

    /// Mutable logical native-tool selections.
    #[must_use]
    pub fn native_tool_routes_mut(&mut self) -> &mut Vec<heycode_core::NativeToolRoute> {
        &mut self.draft.native_tool_routes
    }

    /// Mutable provider-owned request option objects.
    #[must_use]
    pub fn provider_options_mut(&mut self) -> &mut Vec<ProviderRequestOption> {
        &mut self.draft.provider_options
    }

    /// Sampling temperature proposal.
    #[must_use]
    pub const fn temperature(&self) -> Option<f32> {
        self.draft.temperature
    }

    /// Replace the sampling temperature proposal.
    pub fn set_temperature(&mut self, value: Option<f32>) {
        self.draft.temperature = value;
    }

    /// Output-token cap proposal.
    #[must_use]
    pub const fn max_output_tokens(&self) -> Option<u64> {
        self.draft.max_output_tokens
    }

    /// Replace the output-token cap proposal.
    pub fn set_max_output_tokens(&mut self, value: Option<u64>) {
        self.draft.max_output_tokens = value;
    }

    /// Deliberately refuse dispatch with a validated body-free policy code.
    pub fn reject(&mut self, code: ProviderInterceptionCode) {
        self.verdict = InterceptionVerdict::Reject(code);
    }

    fn finish(
        self,
        completion: WaterfallCompletion,
    ) -> Result<RequestDraft, ProviderInterceptionError> {
        finish(
            ProviderInterceptionStage::Request,
            self.verdict,
            completion,
            self.cancellation.is_cancelled(),
            self.draft,
        )
    }
}

/// Secret-free strict request facts available to authentication/policy layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestContext {
    provider: String,
    model: String,
    purpose: CallPurpose,
    authentication: AuthenticationBinding,
}

impl ProviderRequestContext {
    /// Validate one request context without inspecting secret material.
    ///
    /// # Errors
    /// Invalid provider/model identifiers fail without echoing the value.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        purpose: CallPurpose,
        authentication: AuthenticationBinding,
    ) -> Result<Self, ProviderInterceptionBoundaryError> {
        let provider = provider.into();
        let model = model.into();
        if !valid_provider(&provider) {
            return Err(ProviderInterceptionBoundaryError::InvalidProvider);
        }
        if !valid_model(&model) {
            return Err(ProviderInterceptionBoundaryError::InvalidModel);
        }
        Ok(Self {
            provider,
            model,
            purpose,
            authentication,
        })
    }

    /// Provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Request purpose.
    #[must_use]
    pub const fn purpose(&self) -> CallPurpose {
        self.purpose
    }

    /// Secret-free authentication binding preview.
    #[must_use]
    pub const fn authentication(&self) -> &AuthenticationBinding {
        &self.authentication
    }
}

/// Safe exact route facts supplied to response layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponseContext {
    provider: String,
    model: String,
    protocol: ProviderProtocol,
    purpose: CallPurpose,
}

impl ProviderResponseContext {
    /// Validate exact response route context.
    ///
    /// # Errors
    /// Invalid provider/model identifiers or an Unknown protocol fail without
    /// echoing the rejected value.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        protocol: ProviderProtocol,
        purpose: CallPurpose,
    ) -> Result<Self, ProviderInterceptionBoundaryError> {
        let provider = provider.into();
        let model = model.into();
        if !valid_provider(&provider) {
            return Err(ProviderInterceptionBoundaryError::InvalidProvider);
        }
        if !valid_model(&model) {
            return Err(ProviderInterceptionBoundaryError::InvalidModel);
        }
        if protocol == ProviderProtocol::Unknown {
            return Err(ProviderInterceptionBoundaryError::InvalidProtocol);
        }
        Ok(Self {
            provider,
            model,
            protocol,
            purpose,
        })
    }

    /// Provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Exact wire protocol.
    #[must_use]
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// Request purpose whose response is being processed.
    #[must_use]
    pub const fn purpose(&self) -> CallPurpose {
        self.purpose
    }
}

/// One response item exposed to P10 without provider body/error text.
#[derive(Clone, PartialEq)]
pub enum ProviderResponseItem {
    /// One normalized successful provider event.
    Event(InferenceEvent),
    /// One body-free terminal provider failure class.
    Failure(ProviderErrorClass),
}

impl std::fmt::Debug for ProviderResponseItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(event) => formatter
                .debug_tuple("Event")
                .field(&inference_event_name(event))
                .finish(),
            Self::Failure(class) => formatter.debug_tuple("Failure").field(class).finish(),
        }
    }
}

/// Mutable normalized response decision handed to P10 layers.
pub struct ProviderResponseDecision {
    context: ProviderResponseContext,
    item: ProviderResponseItem,
    cancellation: CancellationToken,
    verdict: InterceptionVerdict,
}

impl ProviderResponseDecision {
    fn new(
        context: ProviderResponseContext,
        item: ProviderResponseItem,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            context,
            item,
            cancellation,
            verdict: InterceptionVerdict::Continue,
        }
    }

    /// Exact safe route context.
    #[must_use]
    pub const fn context(&self) -> &ProviderResponseContext {
        &self.context
    }

    /// Current body-free response item.
    #[must_use]
    pub const fn item(&self) -> &ProviderResponseItem {
        &self.item
    }

    /// Current normalized event, when this is a successful event item.
    #[must_use]
    pub const fn event(&self) -> Option<&InferenceEvent> {
        match &self.item {
            ProviderResponseItem::Event(event) => Some(event),
            ProviderResponseItem::Failure(_) => None,
        }
    }

    /// Mutate a normalized event before invariant checks and publication.
    #[must_use]
    pub fn event_mut(&mut self) -> Option<&mut InferenceEvent> {
        match &mut self.item {
            ProviderResponseItem::Event(event) => Some(event),
            ProviderResponseItem::Failure(_) => None,
        }
    }

    /// Caller-owned operation cancellation token.
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Deliberately refuse this response with a body-free policy code.
    pub fn reject(&mut self, code: ProviderInterceptionCode) {
        self.verdict = InterceptionVerdict::Reject(code);
    }

    fn finish(
        self,
        completion: WaterfallCompletion,
    ) -> Result<ProviderResponseItem, ProviderInterceptionError> {
        finish(
            ProviderInterceptionStage::Response,
            self.verdict,
            completion,
            self.cancellation.is_cancelled(),
            self.item,
        )
    }
}

/// Effect-owned strict-provider request and response chains.
#[derive(Default)]
pub struct ProviderInterception {
    request: Waterfall<ProviderRequestDecision>,
    response: Waterfall<ProviderResponseDecision>,
}

impl ProviderInterception {
    /// Register one plugin-owned request layer. Context rollback/shutdown
    /// removes this exact layer.
    pub fn register_request(
        &self,
        context: &Context,
        layer: impl Layer<ProviderRequestDecision> + 'static,
    ) {
        self.request.push_effect(context, layer);
    }

    /// Register one plugin-owned response layer. Context rollback/shutdown
    /// removes this exact layer.
    pub fn register_response(
        &self,
        context: &Context,
        layer: impl Layer<ProviderResponseDecision> + 'static,
    ) {
        self.response.push_effect(context, layer);
    }

    /// Run request middleware and return the admitted draft.
    ///
    /// # Errors
    /// Cancellation, typed policy rejection, a body-free layer failure, or an
    /// undeclared short-circuit.
    pub async fn intercept_request(
        &self,
        context: ProviderRequestContext,
        draft: RequestDraft,
        cancellation: CancellationToken,
    ) -> Result<RequestDraft, ProviderInterceptionError> {
        if cancellation.is_cancelled() {
            return Err(ProviderInterceptionError::Cancelled {
                stage: ProviderInterceptionStage::Request,
            });
        }
        if context.provider != draft.provider
            || context.model != draft.model
            || context.purpose != draft.purpose
        {
            return Err(ProviderInterceptionError::InvalidContext {
                stage: ProviderInterceptionStage::Request,
            });
        }
        let mut decision = ProviderRequestDecision::new(context, draft, cancellation);
        let completion = self.request.run_checked(&mut decision).await.map_err(|_| {
            ProviderInterceptionError::LayerFailed {
                stage: ProviderInterceptionStage::Request,
            }
        })?;
        decision.finish(completion)
    }

    /// Run response middleware and return the admitted normalized event.
    ///
    /// # Errors
    /// Cancellation, typed policy rejection, a body-free layer failure, or an
    /// undeclared short-circuit.
    pub async fn intercept_response(
        &self,
        context: ProviderResponseContext,
        item: ProviderResponseItem,
        cancellation: CancellationToken,
    ) -> Result<ProviderResponseItem, ProviderInterceptionError> {
        if cancellation.is_cancelled() {
            return Err(ProviderInterceptionError::Cancelled {
                stage: ProviderInterceptionStage::Response,
            });
        }
        let mut decision = ProviderResponseDecision::new(context, item, cancellation);
        let completion = self
            .response
            .run_checked(&mut decision)
            .await
            .map_err(|_| ProviderInterceptionError::LayerFailed {
                stage: ProviderInterceptionStage::Response,
            })?;
        decision.finish(completion)
    }

    /// Current effect-owned request-layer count.
    #[must_use]
    pub fn request_layer_count(&self) -> usize {
        self.request.shared_layer_count()
    }

    /// Current effect-owned response-layer count.
    #[must_use]
    pub fn response_layer_count(&self) -> usize {
        self.response.shared_layer_count()
    }
}

/// Exact inventory rows every implementation of the `llm` plugin owns.
#[must_use]
pub fn provider_interception_inventory() -> Vec<PluginContributionSpec> {
    [PROVIDER_REQUEST_SEAM, PROVIDER_RESPONSE_SEAM]
        .into_iter()
        .map(|name| {
            PluginContributionSpec::new(heycode_core::ContributionKind::InterceptionSeam, name)
        })
        .collect()
}

fn finish<T>(
    stage: ProviderInterceptionStage,
    verdict: InterceptionVerdict,
    completion: WaterfallCompletion,
    cancelled: bool,
    value: T,
) -> Result<T, ProviderInterceptionError> {
    if cancelled {
        return Err(ProviderInterceptionError::Cancelled { stage });
    }
    match verdict {
        InterceptionVerdict::Reject(code) => {
            Err(ProviderInterceptionError::Rejected { stage, code })
        }
        InterceptionVerdict::Continue if completion == WaterfallCompletion::Completed => Ok(value),
        InterceptionVerdict::Continue => {
            Err(ProviderInterceptionError::UndeclaredShortCircuit { stage })
        }
    }
}

fn valid_provider(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_ID_BYTES
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_ID_BYTES
        && value
            .chars()
            .all(|character| !character.is_control() && !character.is_whitespace())
}

fn inference_event_name(event: &InferenceEvent) -> &'static str {
    match event {
        InferenceEvent::ResponseStarted { .. } => "response_started",
        InferenceEvent::ItemStarted { .. } => "item_started",
        InferenceEvent::TextDelta(_) => "text_delta",
        InferenceEvent::ReasoningDelta(_) => "reasoning_delta",
        InferenceEvent::ToolCallDelta { .. } => "tool_call_delta",
        InferenceEvent::ServerToolCall { .. } => "server_tool_call",
        InferenceEvent::ServerToolResult { .. } => "server_tool_result",
        InferenceEvent::ServerToolUsage(_) => "server_tool_usage",
        InferenceEvent::Citation { .. } => "citation",
        InferenceEvent::ItemFinished { .. } => "item_finished",
        InferenceEvent::ProviderState(_) => "provider_state",
        InferenceEvent::ResponseFinished { .. } => "response_finished",
        InferenceEvent::ResponseMetadata(_) => "response_metadata",
        InferenceEvent::Usage(_) => "usage",
        InferenceEvent::Finish(_) => "finish",
    }
}
