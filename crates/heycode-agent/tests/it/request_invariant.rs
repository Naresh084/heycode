//! Durable projection versus live resolved-call pre-transport invariant.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use heycode_agent::{RequestDesyncError, snapshots_from_resolved_call, verify_resolved_call};
use heycode_core::ProviderRequestOption;
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CallPurpose, CapabilitySupport, ChatMessage,
    InferenceAdapter, InferenceEvent, InferenceInput, InferenceStream, InferenceTarget,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, NativeFeature,
    ProviderDescriptor, ProviderProtocol, RequestDraft, ResolveError, ResolveSpec, ResolvedCall,
    RetryJitter, RetrySafety, RetrySpec, ToolSpec, resolve_request,
};
use heycode_session::{SessionEvent, SessionEventKind, project_requests};

struct Adapter {
    provider: ProviderDescriptor,
    dispatches: AtomicUsize,
}

impl Adapter {
    fn new(provider: &str) -> Self {
        Self {
            provider: ProviderDescriptor {
                id: provider.to_owned(),
                display_name: provider.to_owned(),
                protocols: vec![ProviderProtocol::OpenAiChatCompletions],
            },
            dispatches: AtomicUsize::new(0),
        }
    }

    fn spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::OpenAiChatCompletions,
            target: InferenceTarget::Http {
                base_url: format!("https://{}.test/v1", self.provider.id),
            },
            authentication: AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
            default_max_output_tokens: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
        }
    }
}

impl InferenceAdapter for Adapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.provider.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.spec().authentication
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        resolve_request(&self.provider, draft, model, &self.spec())
    }

    fn stream(&self, _call: ResolvedCall) -> InferenceStream {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([Ok(InferenceEvent::Finish(
            heycode_llm::FinishReason::Stop,
        ))]))
    }
}

fn model(provider: &str) -> ModelDescriptor {
    ModelDescriptor {
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        id: format!("{provider}/model"),
        display_name: "Model".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(128_000),
        max_output_tokens: Some(8_192),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        reasoning: None,
    }
}

fn tool(description: &str) -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: description.to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    }
}

fn draft(provider: &str, system: &str, user: &str, tool_description: &str) -> RequestDraft {
    RequestDraft {
        provider: provider.to_owned(),
        model: format!("{provider}/model"),
        catalog_revision: Some(7),
        catalog_fetched_at_ms: Some(10),
        effective_at_ms: 20,
        system: Some(system.to_owned()),
        inputs: vec![InferenceInput::Message(ChatMessage::user(user))],
        tools: vec![tool(tool_description)],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: vec![
            ProviderRequestOption::new(
                provider,
                "routing",
                serde_json::json!({"allow_fallbacks": false}),
            )
            .unwrap(),
        ],
        temperature: Some(0.2),
        max_output_tokens: Some(4_096),
        purpose: CallPurpose::Conversation,
    }
}

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: 2,
        seq,
        time_ms: i64::try_from(seq).unwrap() + 1,
        kind,
    }
}

fn durable_projection(adapter: &Adapter, call: &ResolvedCall) -> heycode_session::ProjectedRequest {
    let request_id = heycode_core::RequestId::from_raw("req_1");
    let (header, context) = snapshots_from_resolved_call(call).unwrap();
    let events = vec![
        event(
            0,
            SessionEventKind::UserMessage {
                text: "hello".to_owned(),
            },
        ),
        event(
            1,
            SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                header: Box::new(header),
            },
        ),
        event(
            2,
            SessionEventKind::RequestContext {
                request_id,
                context,
            },
        ),
    ];
    let projected = project_requests(&events).unwrap().pop().unwrap();
    assert_eq!(projected.header.provider, adapter.provider.id);
    projected
}

#[tokio::test]
async fn matching_projection_unlocks_exactly_one_dispatch() {
    let adapter = Adapter::new("provider");
    let call = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let projected = durable_projection(&adapter, &call);
    let verified = verify_resolved_call(&projected, call, &adapter, None).unwrap();
    let events = verified
        .dispatch(tokio_util::sync::CancellationToken::new())
        .collect::<Vec<_>>()
        .await;
    assert!(matches!(&events[..], [Ok(InferenceEvent::Finish(_))]));
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 1);
}

/// The durable header records the only replay evidence resolution owns: the
/// native features and provider-executed tool routes this request carries. A
/// route that hands the agent a call claiming the request is replayable while
/// that evidence says the provider is executing work of its own has shipped
/// the wrong replay policy, and a retried turn would double-execute it. That
/// must be loud before any transport starts, not discovered on a retry.
#[test]
fn a_replay_policy_wider_than_the_durable_evidence_fails_before_transport() {
    let adapter = Adapter::new("provider");
    let mut model = model("provider");
    model.capabilities.native_web = CapabilitySupport::Supported;
    let mut draft = draft("provider", "system", "hello", "Read");
    draft.native_features = vec![NativeFeature::Web];
    let resolve = || adapter.resolve(draft.clone(), &model).unwrap();
    let resolved = resolve();

    // Resolution proved this request is NOT safe to send twice.
    assert_eq!(resolved.retry_spec().safety(), RetrySafety::Never);
    let projected = durable_projection(&adapter, &resolved);
    assert_eq!(projected.header.options.native_features, ["web"]);
    // And the durable header says so, so a reader looking at a duplicate
    // dispatch can tell policy from a bug.
    assert_eq!(
        projected.header.options.retry,
        Some(heycode_session::RequestRetrySnapshot {
            max_attempts: 3,
            safety: heycode_session::RequestRetrySafetySnapshot::Never,
        }),
        "narrowing forbids replay without pretending the attempt budget changed"
    );
    let replayable =
        durable_projection(&adapter, &resolve().with_retry_spec(RetrySpec::standard()));
    assert_eq!(
        replayable.header.options.retry.unwrap().safety,
        heycode_session::RequestRetrySafetySnapshot::StatelessPreOutput,
        "a replayable request records the wider policy it actually ran under"
    );

    // The narrowed policy verifies exactly as before.
    verify_resolved_call(&projected, resolved, &adapter, None).unwrap();

    // A route that re-resolved and lost the narrowing must not dispatch.
    let widened = resolve().with_retry_spec(RetrySpec::standard());
    assert_eq!(
        widened.retry_spec().safety(),
        RetrySafety::StatelessPreOutput
    );
    let error = verify_resolved_call(&projected, widened, &adapter, None)
        .err()
        .expect("a replay policy wider than the durable evidence must fail the compare");

    assert!(
        matches!(
            error,
            RequestDesyncError::Mismatch {
                field: "retry_replay_safety"
            }
        ),
        "unexpected error: {error}"
    );
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verified_dispatch_threads_pre_cancellation_before_adapter_streaming() {
    let adapter = Adapter::new("provider");
    let call = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let projected = durable_projection(&adapter, &call);
    let verified = verify_resolved_call(&projected, call, &adapter, None).unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();

    let events = verified.dispatch(cancellation).collect::<Vec<_>>().await;
    assert!(matches!(&events[..], [Err(_)]));
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn mutated_live_prompt_tool_route_or_inputs_fail_before_transport() {
    let durable_adapter = Adapter::new("provider");
    let baseline = durable_adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let projected = durable_projection(&durable_adapter, &baseline);

    let prompt = durable_adapter
        .resolve(
            draft("provider", "mutated", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, prompt, &durable_adapter, None),
        Err(RequestDesyncError::Mismatch { field: "system" })
    ));

    let tools = durable_adapter
        .resolve(
            draft("provider", "system", "hello", "Changed"),
            &model("provider"),
        )
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, tools, &durable_adapter, None),
        Err(RequestDesyncError::Mismatch { field: "tools" })
    ));

    let inputs = durable_adapter
        .resolve(
            draft("provider", "system", "other", "Read"),
            &model("provider"),
        )
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, inputs, &durable_adapter, None),
        Err(RequestDesyncError::Mismatch { field: "inputs" })
    ));

    let mut option_draft = draft("provider", "system", "hello", "Read");
    option_draft.provider_options = vec![
        ProviderRequestOption::new(
            "provider",
            "routing",
            serde_json::json!({"allow_fallbacks": true}),
        )
        .unwrap(),
    ];
    let options = durable_adapter
        .resolve(option_draft, &model("provider"))
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, options, &durable_adapter, None),
        Err(RequestDesyncError::Mismatch {
            field: "provider_options"
        })
    ));

    let other_adapter = Adapter::new("other");
    let route = other_adapter
        .resolve(draft("other", "system", "hello", "Read"), &model("other"))
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, route, &other_adapter, None),
        Err(RequestDesyncError::Mismatch { field: "provider" })
    ));

    assert_eq!(durable_adapter.dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(other_adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn context_or_adapter_default_provenance_drift_is_detected() {
    let adapter = Adapter::new("provider");
    let baseline = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let mut projected = durable_projection(&adapter, &baseline);
    projected.context.catalog_revision = Some(8);
    let call = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, call, &adapter, None),
        Err(RequestDesyncError::Mismatch {
            field: "catalog_revision"
        })
    ));

    projected.context.catalog_revision = Some(7);
    projected.header.options.defaulted_max_output_tokens = true;
    let call = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    assert!(matches!(
        verify_resolved_call(&projected, call, &adapter, None),
        Err(RequestDesyncError::Mismatch {
            field: "defaulted_max_output_tokens"
        })
    ));
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn legacy_header_without_a_retry_row_still_verifies() {
    let adapter = Adapter::new("provider");
    let call = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let mut projected = durable_projection(&adapter, &call);
    projected.header.options.retry = None;
    verify_resolved_call(&projected, call, &adapter, None).unwrap();
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}

#[test]
fn retry_attempt_budget_drift_fails_before_transport() {
    let adapter = Adapter::new("provider");
    let baseline = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap();
    let projected = durable_projection(&adapter, &baseline);
    assert_eq!(
        projected.header.options.retry.unwrap().max_attempts,
        3,
        "the durable header records the resolved attempt budget"
    );

    // Same replay safety, different attempt budget: no footprint in
    // native_features or routes, so the rank bound alone cannot see it.
    let drifted = adapter
        .resolve(
            draft("provider", "system", "hello", "Read"),
            &model("provider"),
        )
        .unwrap()
        .with_retry_spec(
            RetrySpec::new(
                1,
                Duration::from_millis(500),
                Duration::from_secs(8),
                Duration::from_secs(60),
                RetryJitter::Full,
                RetrySafety::StatelessPreOutput,
            )
            .unwrap(),
        );
    let error = verify_resolved_call(&projected, drifted, &adapter, None)
        .err()
        .expect("an attempt budget the durable header never recorded must fail the compare");
    assert!(
        matches!(
            error,
            RequestDesyncError::Mismatch {
                field: "retry_max_attempts"
            }
        ),
        "unexpected error: {error}"
    );
    assert_eq!(adapter.dispatches.load(Ordering::SeqCst), 0);
}
