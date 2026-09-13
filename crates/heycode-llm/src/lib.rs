//! heycode-llm — the LLM capability: provider-neutral vocabulary, the
//! [`Provider`] seam, reusable Responses/Chat/Messages protocol adapters, the
//! DeepSeek and OpenRouter routes, provider registry, and the effect-owned
//! model catalog registry with TTL/single-flight last-good refreshes. The
//! native [`InferenceAdapter`] draft-to-[`ResolvedCall`] boundary validates
//! every represented choice before transport; P08 adds body-free failure
//! classes and cancellation-aware pre-output retry policy.
//!
//! Stream contract (AGENTS §5): a well-formed chunk stream carries exactly
//! one `Usage`, immediately before its `Finish`, and nothing after
//! `Finish`. Tool-call deltas accumulate client-side via [`accumulate`].

mod accumulate;
mod activation;
mod anthropic;
mod audio;
mod bedrock;
mod catalog;
mod chat;
mod connection;
mod context_budget;
mod context_projection;
mod credential;
pub use connection::{
    ConnectionFamily, ConnectionModelSelection, ConnectionParameter, ConnectionProfile,
};
mod deepseek;
mod error;
mod gemini;
mod inference;
mod interception;
mod model_catalog;
mod model_filter;
mod model_pricing;
mod model_selection;
mod native_compaction;
mod openrouter;
mod provider;
mod registry;
mod request_transform;
mod responses;
mod retry;
mod sse;
pub mod testing;
mod token_count;
mod token_meter;
mod usage_facts;
mod vocab;
mod wire;

/// Registered inference providers.
pub const SERVICE_PROVIDERS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("providers");
/// Active provider/model selection.
pub const SERVICE_LLM: heycode_core::ServiceKey = heycode_core::ServiceKey::new("llm");
/// Strict-provider request/response around-middleware.
pub const SERVICE_PROVIDER_INTERCEPTION: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("provider-interception");
/// Provider-owned live model catalogs and last-good generations.
pub const SERVICE_MODELS: heycode_core::ServiceKey = heycode_core::ServiceKey::new("models");
/// Exact and estimated token counters plus their deterministic selection.
pub const SERVICE_TOKEN_COUNTERS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("token-counters");
/// Effect-owned provider request-transform policies.
pub const SERVICE_REQUEST_TRANSFORMS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("request-transforms");

pub use accumulate::{Accumulated, AccumulatedToolCall, accumulate};
pub use activation::{ProviderActivator, ProviderActivatorRegistration};
pub use anthropic::{
    AnthropicAuthWire, AnthropicMessagesAdapter, AnthropicMessagesConfig, AnthropicMessagesDialect,
    AnthropicServerToolNormalizationFault, AnthropicServerToolPlan,
    AnthropicServerToolResultNormalizer, AnthropicServerToolRoute, AnthropicThinkingDisplay,
    AnthropicThinkingMode,
};
pub use audio::{
    ExperimentalAudioAdapter, ExperimentalAudioDescriptor, ExperimentalAudioError,
    ExperimentalAudioEvent, ExperimentalAudioInput, ExperimentalAudioOutput,
    ExperimentalAudioRequest, ExperimentalAudioStream, ExperimentalFeatureVisibility,
    ResolvedExperimentalAudioCall, resolve_experimental_audio,
};
pub use bedrock::{BedrockConverseAdapter, BedrockConverseConfig};
pub use catalog::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelLifecycleStatus,
    ModelReasoningError, ModelReasoningMetadata, ProviderDescriptor,
};
pub use chat::{
    ChatReasoningContinuation, ChatReasoningWire, ChatThinkingConfig, OpenAiChatCompletionsAdapter,
    OpenAiChatCompletionsConfig,
};
pub use context_budget::context_budget;
pub use context_projection::{BudgetUse, ContextProjection, ContributorLine};
pub use credential::{CredentialResolver, OperationCredential, RouteCredential};
pub use deepseek::DeepSeekProvider;
pub use error::{
    LlmError, ProviderErrorClass, ProviderErrorCode, ProviderFactError, ProviderFailure,
    ProviderFailureOrigin,
};
pub use gemini::{
    GeminiAdapter, GeminiConfig, GeminiExtensionFault, GeminiProviderOptionPlan,
    GeminiProviderOptionWire, GeminiStreamNormalizer, GeminiStreamNormalizerFactory,
};
pub use heycode_core::{ContextActivity, ContextBudget, ContextConfidence, ContextLimitSource};
pub use heycode_core::{
    ProviderProtocol, ProviderRequestOption, ProviderStateError, ProviderStateItem,
    ProviderStateKind,
};
pub use heycode_credentials::CredentialResolutionError;
pub use inference::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CredentialHandle, InferenceAdapter,
    InferenceEvent, InferenceInput, InferenceStream, InferenceTarget, InputModality, NativeFeature,
    ReasoningEffortId, ReasoningEffortOptions, RequestDraft, RequestedCapability, ResolveError,
    ResolveSpec, ResolvedCall, ResolvedDefaults, StreamItemKind, resolve_request,
};
pub use interception::{
    PROVIDER_REQUEST_SEAM, PROVIDER_RESPONSE_SEAM, ProviderInterception,
    ProviderInterceptionBoundaryError, ProviderInterceptionCode, ProviderInterceptionError,
    ProviderInterceptionStage, ProviderRequestContext, ProviderRequestDecision,
    ProviderResponseContext, ProviderResponseDecision, ProviderResponseItem,
    provider_interception_inventory,
};
pub use model_catalog::{
    CatalogError, CatalogFailureKind, CatalogFetchError, CatalogFreshness, CatalogPersistence,
    CatalogPersistenceError, CatalogRefreshMode, CatalogRegistry, CatalogSnapshot, CatalogView,
    ModelCatalog, model_catalog_plugin, token_counters_plugin,
};
pub use model_filter::{CapabilityFilter, ModelFilter, ModelLifecycleFilter};
pub use model_pricing::{
    ModelMetadataProvenance, ModelPerformance, ModelPricing, ModelProvenanceError,
    PerformanceError, PriceComponent, PriceCurrency, PricingError, TokenPrice, TokenPriceUnit,
};
pub use model_selection::{ModelSelectionError, ModelSelectionWarning, ResolvedModelSelection};
pub use native_compaction::{
    NativeCompactionAdapter, NativeCompactionCheckpoint, NativeCompactionError,
    NativeCompactionFuture,
};
pub use openrouter::{
    OpenRouterDataCollection, OpenRouterProvider, OpenRouterRoutingPolicy,
    OpenRouterRoutingPolicyError, OpenRouterWebSearchEngine, OpenRouterWebSearchPolicy,
    OpenRouterWebSearchPolicyError,
};
pub use provider::{ChunkStream, Provider, ProviderInfo, ProviderOptionContext};
pub use registry::{
    LlmSelection, ProviderProfile, ProviderRegistration, ProviderRegistry, llm_plugin,
};
pub use request_transform::{
    RequestTransformCost, RequestTransformDescriptor, RequestTransformEffect,
    RequestTransformError, RequestTransformId, RequestTransformPagePrice, RequestTransformProvider,
    RequestTransformRegistry, RequestTransformRequest, request_transforms_plugin,
};
pub use responses::{
    OpenAiResponsesAdapter, OpenAiResponsesConfig, ResponsesContinuation,
    ResponsesServerToolDefinition, ResponsesServerToolFault, ResponsesServerToolNormalization,
    ResponsesServerToolNormalizer, ResponsesServerToolPlan,
};
pub use retry::{
    RetryAttempt, RetryDecision, RetryDelaySource, RetryJitter, RetrySafety, RetrySpec,
    RetrySpecError, RetryStopReason, classify_transport_error,
};
pub use sse::SseParser;
pub use token_count::{
    CountableContent, EstimatedTokenCount, EstimationMethod, ExactTokenCount,
    HeuristicTokenEstimator, TokenCount, TokenCountError, TokenCountFailure, TokenCountFailureKind,
    TokenCountOutcome, TokenCountRefusal, TokenCountRequest, TokenCounter, TokenCounterDescriptor,
    TokenCounterId, TokenCounterRegistry, TokenCounterScope, TokenEvidence,
};
pub use token_meter::{
    ContributorTokens, EnvelopeContributor, EnvelopeCost, EnvelopeEntry, EnvelopeMeasurementError,
    EnvelopeTotal, TokenEnvelope, UncountedReason, measure_chat_request_envelope,
    measure_experimental_audio_envelope, measure_resolved_call_envelope,
};
pub use usage_facts::{
    RateLimitHeaderNames, RateLimitHeaders, RateLimitScope, RateLimitSnapshot, RateLimitWindow,
    RequestCost,
};
pub use vocab::{
    ChatDocument, ChatImage, ChatMessage, ChatRequest, ChatToolCall, FinishReason, Role,
    StreamChunk, TokenUsage, ToolSpec,
};
pub use wire::{OpenAiCompatClient, OpenAiCompatConfig};
