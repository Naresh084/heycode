//! C11 production construction from the actual legacy and strict request planes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_core::{Context, ProviderStateItem, ProviderStateKind, ToolSpec};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatImage,
    ChatMessage, ChatRequest, ContributorTokens, CountableContent, EnvelopeContributor,
    EnvelopeMeasurementError, HeuristicTokenEstimator, InferenceInput, InferenceTarget,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, ProviderDescriptor, ProviderProtocol, RequestDraft, ResolveSpec,
    TokenCountFailure, TokenCountRequest, TokenCounter, TokenCounterDescriptor, TokenCounterId,
    TokenCounterRegistry, TokenCounterScope, TokenEvidence, UncountedReason,
    measure_chat_request_envelope, measure_resolved_call_envelope, resolve_request,
};
use tokio_util::sync::CancellationToken;

struct ExactMessageCounter {
    calls: Arc<AtomicUsize>,
    descriptor: TokenCounterDescriptor,
}

struct RefusingExactCounter {
    descriptor: TokenCounterDescriptor,
}

#[async_trait]
impl TokenCounter for RefusingExactCounter {
    fn descriptor(&self) -> TokenCounterDescriptor {
        self.descriptor.clone()
    }

    async fn count(
        &self,
        _request: &TokenCountRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure> {
        Err(TokenCountFailure::unsupported(
            "exact counter cannot represent this transcript",
        ))
    }
}

#[async_trait]
impl TokenCounter for ExactMessageCounter {
    fn descriptor(&self) -> TokenCounterDescriptor {
        self.descriptor.clone()
    }

    async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            request
                .content()
                .iter()
                .all(|part| matches!(part, CountableContent::Message(message) if message.images.is_empty() && message.documents.is_empty())),
            "the exact counter receives transcript messages only; other contributors are isolated"
        );
        Ok(17)
    }
}

fn registry() -> (Context, TokenCounterRegistry, Arc<AtomicUsize>) {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    let calls = Arc::new(AtomicUsize::new(0));
    registry
        .register(
            &context,
            Arc::new(ExactMessageCounter {
                calls: calls.clone(),
                descriptor: TokenCounterDescriptor::new(
                    TokenCounterId::new("provider-exact").unwrap(),
                    TokenEvidence::Exact,
                    TokenCounterScope::provider("provider").unwrap(),
                )
                .unwrap(),
            }),
        )
        .unwrap();
    registry
        .register(&context, Arc::new(HeuristicTokenEstimator::new()))
        .unwrap();
    (context, registry, calls)
}

fn entry_tokens(
    envelope: &heycode_llm::TokenEnvelope,
    contributor: EnvelopeContributor,
) -> &ContributorTokens {
    envelope
        .entries()
        .iter()
        .find(|entry| entry.contributor() == contributor)
        .expect("every contributor is represented")
        .tokens()
}

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

fn image() -> ChatImage {
    ChatImage::new(
        heycode_core::AttachmentMediaType::new("image/png").unwrap(),
        vec![1, 2, 3],
    )
    .unwrap()
}

fn resolved_call() -> heycode_llm::ResolvedCall {
    let provider = ProviderDescriptor {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    };
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.image_input = CapabilitySupport::Supported;
    let model = ModelDescriptor {
        id: "provider/model".to_owned(),
        display_name: "Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(32_000),
        max_output_tokens: Some(4_096),
        lifecycle: ModelLifecycle::stable(),
        capabilities,
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    };
    let state = ProviderStateItem::new(
        "provider",
        "provider/model",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role": "assistant",
            "content": "prior answer",
            "reasoning_content": "retained reasoning"
        }),
    )
    .unwrap();
    resolve_request(
        &provider,
        RequestDraft {
            provider: "provider".to_owned(),
            model: "provider/model".to_owned(),
            catalog_revision: Some(1),
            catalog_fetched_at_ms: Some(1),
            effective_at_ms: 2,
            system: Some("standing system instructions".to_owned()),
            inputs: vec![
                InferenceInput::ProviderState(state),
                InferenceInput::Message(ChatMessage::user_with_images("question", vec![image()])),
            ],
            tools: vec![tool()],
            input_modalities: vec![InputModality::Text, InputModality::Image],
            reasoning_effort: None,
            structured_output: None,
            native_features: Vec::new(),
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            purpose: CallPurpose::Conversation,
        },
        &model,
        &ResolveSpec {
            protocol: ProviderProtocol::OpenAiChatCompletions,
            target: InferenceTarget::Http {
                base_url: "https://example.test/v1".to_owned(),
            },
            authentication: AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
            default_max_output_tokens: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn strict_request_measurement_populates_all_real_contributors() {
    let (_context, registry, calls) = registry();
    let envelope =
        measure_resolved_call_envelope(&registry, &resolved_call(), &CancellationToken::new())
            .await
            .unwrap();

    assert_eq!(envelope.entries().len(), 6);
    assert!(matches!(
        entry_tokens(&envelope, EnvelopeContributor::System),
        ContributorTokens::Estimated(tokens, _) if *tokens > 0
    ));
    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::Messages),
        &ContributorTokens::Exact(17)
    );
    assert!(matches!(
        entry_tokens(&envelope, EnvelopeContributor::Tools),
        ContributorTokens::Estimated(tokens, _) if *tokens > 0
    ));
    assert!(matches!(
        entry_tokens(&envelope, EnvelopeContributor::ProviderState),
        ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    ));
    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::Attachments),
        &ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn legacy_request_measurement_separates_media_from_countable_message_text() {
    let (_context, registry, calls) = registry();
    let request = ChatRequest {
        model: "provider/model".to_owned(),
        messages: vec![
            ChatMessage::system("system"),
            ChatMessage::user_with_images("visible text", vec![image()]),
        ],
        tools: Some(vec![tool()]),
        temperature: None,
        max_tokens: None,
    };
    let envelope =
        measure_chat_request_envelope(&registry, "provider", &request, &CancellationToken::new())
            .await
            .unwrap();

    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::Messages),
        &ContributorTokens::Exact(17)
    );
    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::ProviderState),
        &ContributorTokens::Exact(0)
    );
    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::Attachments),
        &ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_produces_no_partial_envelope() {
    let (_context, registry, _calls) = registry();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        measure_resolved_call_envelope(&registry, &resolved_call(), &cancellation).await,
        Err(EnvelopeMeasurementError::Cancelled)
    );
}

#[tokio::test]
async fn a_counter_fallback_remains_visible_on_the_message_contributor() {
    let context = Context::new();
    let registry = TokenCounterRegistry::new();
    registry
        .register(
            &context,
            Arc::new(RefusingExactCounter {
                descriptor: TokenCounterDescriptor::new(
                    TokenCounterId::new("provider-exact").unwrap(),
                    TokenEvidence::Exact,
                    TokenCounterScope::provider("provider").unwrap(),
                )
                .unwrap(),
            }),
        )
        .unwrap();
    registry
        .register(&context, Arc::new(HeuristicTokenEstimator::new()))
        .unwrap();
    let request = ChatRequest {
        model: "provider/model".to_owned(),
        messages: vec![ChatMessage::user("hello")],
        tools: None,
        temperature: None,
        max_tokens: None,
    };

    let envelope =
        measure_chat_request_envelope(&registry, "provider", &request, &CancellationToken::new())
            .await
            .unwrap();
    let messages = envelope
        .entries()
        .iter()
        .find(|entry| entry.contributor() == EnvelopeContributor::Messages)
        .unwrap();
    assert!(matches!(
        messages.tokens(),
        ContributorTokens::Estimated(_, _)
    ));
    assert_eq!(messages.refusals().len(), 1);
    assert_eq!(messages.refusals()[0].counter().as_str(), "provider-exact");
}

#[tokio::test]
async fn tool_results_are_counted_once_and_do_not_enter_the_conversation_counter() {
    let (_context, registry, calls) = registry();
    let request = ChatRequest {
        model: "provider/model".to_owned(),
        messages: vec![ChatMessage::tool("read-call", "x".repeat(40_000))],
        tools: None,
        temperature: None,
        max_tokens: None,
    };
    let envelope =
        measure_chat_request_envelope(&registry, "provider", &request, &CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(
        entry_tokens(&envelope, EnvelopeContributor::Messages),
        &ContributorTokens::Exact(0)
    );
    let result_tokens = entry_tokens(&envelope, EnvelopeContributor::ToolResults);
    assert!(matches!(result_tokens, ContributorTokens::Estimated(tokens, _) if *tokens >= 10_000));
    assert_eq!(envelope.total().counted(), result_tokens.counted().unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
