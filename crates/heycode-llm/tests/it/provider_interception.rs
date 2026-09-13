//! P10 provider request/response around-middleware contracts.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Context, Layer, Next, ProviderProtocol};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, InferenceEvent, InputModality,
    ProviderErrorClass, ProviderInterception, ProviderInterceptionCode, ProviderInterceptionError,
    ProviderRequestContext, ProviderRequestDecision, ProviderResponseContext,
    ProviderResponseDecision, ProviderResponseItem, RequestDraft,
};
use tokio_util::sync::CancellationToken;

fn draft() -> RequestDraft {
    RequestDraft {
        provider: "provider-test".to_owned(),
        model: "provider-test/model".to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: Some("base".to_owned()),
        inputs: vec![heycode_llm::InferenceInput::Message(
            heycode_llm::ChatMessage::user("hello"),
        )],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    }
}

fn response_context() -> ProviderResponseContext {
    ProviderResponseContext::new(
        "provider-test",
        "provider-test/model",
        ProviderProtocol::OpenAiResponses,
        CallPurpose::Conversation,
    )
    .unwrap()
}

fn request_context() -> ProviderRequestContext {
    ProviderRequestContext::new(
        "provider-test",
        "provider-test/model",
        CallPurpose::Conversation,
        AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
    )
    .unwrap()
}

struct RequestEdit {
    name: &'static str,
    order: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Layer<ProviderRequestDecision> for RequestEdit {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        self.order.lock().unwrap().push(self.name);
        assert!(matches!(
            input.context().authentication(),
            AuthenticationBinding::AdapterOwned(_)
        ));
        input.set_max_output_tokens(Some(input.max_output_tokens().unwrap_or(0) + 100));
        next.run(input).await
    }
}

#[tokio::test]
async fn request_layers_run_in_registration_order_and_edits_survive() {
    let mut context = Context::new();
    let interception = ProviderInterception::default();
    let order = Arc::new(Mutex::new(Vec::new()));
    interception.register_request(
        &context,
        RequestEdit {
            name: "first",
            order: order.clone(),
        },
    );
    interception.register_request(
        &context,
        RequestEdit {
            name: "second",
            order: order.clone(),
        },
    );

    let request = interception
        .intercept_request(request_context(), draft(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(*order.lock().unwrap(), ["first", "second"]);
    assert_eq!(request.max_output_tokens, Some(200));
    context.shutdown();
}

struct RejectRequest;

#[async_trait]
impl Layer<ProviderRequestDecision> for RejectRequest {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        _next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        input.reject(ProviderInterceptionCode::new("managed-policy").unwrap());
        Ok(())
    }
}

struct ForgetNext;

#[async_trait]
impl Layer<ProviderRequestDecision> for ForgetNext {
    async fn handle(
        &self,
        _input: &mut ProviderRequestDecision,
        _next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

struct LeakError;

#[async_trait]
impl Layer<ProviderRequestDecision> for LeakError {
    async fn handle(
        &self,
        _input: &mut ProviderRequestDecision,
        _next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("credential-canary-must-not-escape")
    }
}

#[tokio::test]
async fn rejection_is_explicit_and_an_undeclared_short_circuit_fails_closed() {
    let reject = ProviderInterception::default();
    let reject_context = Context::new();
    reject.register_request(&reject_context, RejectRequest);
    assert!(matches!(
        reject
            .intercept_request(request_context(), draft(), CancellationToken::new())
            .await,
        Err(ProviderInterceptionError::Rejected { code, .. })
            if code.as_str() == "managed-policy"
    ));

    let missing_next = ProviderInterception::default();
    let missing_context = Context::new();
    missing_next.register_request(&missing_context, ForgetNext);
    assert!(matches!(
        missing_next
            .intercept_request(request_context(), draft(), CancellationToken::new())
            .await,
        Err(ProviderInterceptionError::UndeclaredShortCircuit { .. })
    ));
}

#[tokio::test]
async fn layer_errors_discard_the_body_and_effect_disposal_removes_exact_layers() {
    let mut context = Context::new();
    let interception = ProviderInterception::default();
    interception.register_request(&context, LeakError);
    assert_eq!(interception.request_layer_count(), 1);

    let error = interception
        .intercept_request(request_context(), draft(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderInterceptionError::LayerFailed { .. }
    ));
    assert!(!error.to_string().contains("credential-canary"));

    context.shutdown();
    assert_eq!(interception.request_layer_count(), 0);
}

struct ReplaceText;

#[async_trait]
impl Layer<ProviderResponseDecision> for ReplaceText {
    async fn handle(
        &self,
        input: &mut ProviderResponseDecision,
        mut next: Next<'_, ProviderResponseDecision>,
    ) -> anyhow::Result<()> {
        if let Some(InferenceEvent::TextDelta(text)) = input.event_mut() {
            *text = "filtered".to_owned();
        }
        next.run(input).await
    }
}

#[tokio::test]
async fn response_edits_survive_only_after_the_chain_delegates() {
    let context = Context::new();
    let interception = ProviderInterception::default();
    interception.register_response(&context, ReplaceText);

    let event = interception
        .intercept_response(
            response_context(),
            ProviderResponseItem::Event(InferenceEvent::TextDelta("raw".to_owned())),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        event,
        ProviderResponseItem::Event(InferenceEvent::TextDelta("filtered".to_owned()))
    );
}

struct ObserveFailure(Arc<Mutex<Vec<ProviderErrorClass>>>);

#[async_trait]
impl Layer<ProviderResponseDecision> for ObserveFailure {
    async fn handle(
        &self,
        input: &mut ProviderResponseDecision,
        mut next: Next<'_, ProviderResponseDecision>,
    ) -> anyhow::Result<()> {
        if let ProviderResponseItem::Failure(class) = input.item() {
            self.0.lock().unwrap().push(*class);
        }
        next.run(input).await
    }
}

#[tokio::test]
async fn response_layers_observe_body_free_failure_classes() {
    let context = Context::new();
    let interception = ProviderInterception::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    interception.register_response(&context, ObserveFailure(seen.clone()));

    let item = interception
        .intercept_response(
            response_context(),
            ProviderResponseItem::Failure(ProviderErrorClass::Authentication),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        item,
        ProviderResponseItem::Failure(ProviderErrorClass::Authentication)
    );
    assert_eq!(*seen.lock().unwrap(), [ProviderErrorClass::Authentication]);
}

#[tokio::test]
async fn request_context_must_match_the_durable_draft_route() {
    let context = ProviderRequestContext::new(
        "other-provider",
        "provider-test/model",
        CallPurpose::Conversation,
        AuthenticationBinding::None,
    )
    .unwrap();

    assert!(matches!(
        ProviderInterception::default()
            .intercept_request(context, draft(), CancellationToken::new())
            .await,
        Err(ProviderInterceptionError::InvalidContext { .. })
    ));
}

#[test]
fn interception_codes_and_response_context_reject_boundary_data() {
    assert!(ProviderInterceptionCode::new("").is_err());
    assert!(ProviderInterceptionCode::new("Not Kebab").is_err());
    assert!(ProviderInterceptionCode::new("a".repeat(65)).is_err());
    assert!(
        ProviderResponseContext::new(
            "bad provider",
            "model",
            ProviderProtocol::OpenAiResponses,
            CallPurpose::Conversation,
        )
        .is_err()
    );
    assert!(
        ProviderResponseContext::new(
            "provider",
            "model",
            ProviderProtocol::Unknown,
            CallPurpose::Conversation,
        )
        .is_err()
    );
}
