//! POR03 OpenRouter provider-routing policy reaches the exact wire body.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, OpenRouterDataCollection, OpenRouterProvider, OpenRouterRoutingPolicy,
    OpenRouterWebSearchEngine, OpenRouterWebSearchPolicy, Provider, ProviderProtocol,
    ProviderStateItem, ProviderStateKind, ReasoningEffortId, RequestDraft, ToolSpec,
};
use tokio_util::sync::CancellationToken;

struct CaptureTransport {
    body: Arc<Mutex<Option<serde_json::Value>>>,
}

impl HttpTransport for CaptureTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        let body = serde_json::from_slice(request.body().unwrap()).unwrap();
        *self.body.lock().unwrap() = Some(body);
        Box::pin(futures::stream::iter([
            Ok(SseEvent {
                event: "message".to_owned(),
                data: r#"{"id":"route_1","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#.to_owned(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: r#"{"id":"route_1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#.to_owned(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: "[DONE]".to_owned(),
                id: None,
                retry_ms: None,
            }),
        ]))
    }
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        id: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        display_name: "GLM 5.3 Flash".to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_048_576),
        max_output_tokens: Some(131_072),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        reasoning: None,
    }
}

/// One non-GLM OpenRouter row carrying exactly what its catalog published.
fn published_model(id: &str, efforts: &[&str], default_effort: Option<&str>) -> ModelDescriptor {
    ModelDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        reasoning: Some(
            heycode_llm::ModelReasoningMetadata::published(
                efforts.iter().map(|effort| (*effort).to_owned()).collect(),
                default_effort.map(str::to_owned),
                Some(true),
                Some(false),
            )
            .unwrap(),
        ),
        ..model()
    }
}

fn sse(data: serde_json::Value) -> SseEvent {
    SseEvent {
        event: "message".to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    }
}

fn done() -> SseEvent {
    SseEvent {
        event: "message".to_owned(),
        data: "[DONE]".to_owned(),
        id: None,
        retry_ms: None,
    }
}

struct ScriptCaptureTransport {
    scripts: Mutex<Vec<Vec<SseEvent>>>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl HttpTransport for ScriptCaptureTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap()).unwrap());
        let script = self.scripts.lock().unwrap().remove(0);
        Box::pin(futures::stream::iter(script.into_iter().map(Ok)))
    }
}

fn tool() -> ToolSpec {
    ToolSpec {
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({"type":"object"}),
    }
}

fn provider_draft(provider: &OpenRouterProvider, inputs: Vec<InferenceInput>) -> RequestDraft {
    RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs,
        tools: vec![tool()],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Provider::request_options(provider),
        temperature: None,
        max_output_tokens: Some(1024),
        purpose: CallPurpose::Evaluation,
    }
}

#[tokio::test]
async fn validated_routing_policy_is_durable_input_and_reaches_provider_wire() {
    let policy = OpenRouterRoutingPolicy::new(
        vec!["z-ai".to_owned(), "novita".to_owned()],
        false,
        true,
        OpenRouterDataCollection::Deny,
        Some(true),
    )
    .unwrap();
    let captured = Arc::new(Mutex::new(None));
    let http = heycode_http::HttpService::new(Arc::new(CaptureTransport {
        body: captured.clone(),
    }));
    let provider = OpenRouterProvider::from_key_with_transport_and_routing(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        http,
        policy,
        super::openrouter_transform_options(),
    )
    .unwrap();
    let provider_options = Provider::request_options(&provider);
    assert_eq!(provider_options.len(), 3);
    assert_eq!(provider_options[0].provider(), "openrouter");
    assert_eq!(provider_options[0].kind(), "routing");
    assert_eq!(provider_options[1].provider(), "openrouter");
    assert_eq!(provider_options[1].kind(), "caching");
    assert_eq!(provider_options[2].kind(), "transforms");

    let draft = RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options,
        temperature: None,
        max_output_tokens: Some(1024),
        purpose: CallPurpose::Evaluation,
    };
    let call = InferenceAdapter::resolve(&provider, draft, &model()).unwrap();
    assert_eq!(
        call.provider_options()
            .iter()
            .map(heycode_core::ProviderRequestOption::kind)
            .collect::<Vec<_>>(),
        ["routing", "caching", "transforms"]
    );
    let output = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    assert!(output.iter().all(Result::is_ok));

    let body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        body["provider"],
        serde_json::json!({
            "order": ["z-ai", "novita"],
            "allow_fallbacks": false,
            "require_parameters": true,
            "data_collection": "deny",
            "zdr": true
        })
    );
    assert_eq!(
        body["plugins"],
        super::openrouter_transform_options()[0].data()["plugins"]
    );
}

#[test]
fn routing_policy_rejects_duplicate_or_unsafe_provider_slugs() {
    assert!(
        OpenRouterRoutingPolicy::new(
            vec!["z-ai".to_owned(), "z-ai".to_owned()],
            true,
            false,
            OpenRouterDataCollection::Allow,
            None,
        )
        .is_err()
    );
    assert!(
        OpenRouterRoutingPolicy::new(
            vec!["Z.AI".to_owned()],
            true,
            false,
            OpenRouterDataCollection::Allow,
            None,
        )
        .is_err()
    );
}

#[test]
fn openrouter_route_cannot_construct_without_an_explicit_transform_decision() {
    let result = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(Arc::new(CaptureTransport {
            body: Arc::new(Mutex::new(None)),
        })),
        Vec::new(),
    );
    let Err(error) = result else {
        panic!("OpenRouter omission must not inherit API or account transform defaults")
    };
    assert!(error.to_string().contains("transform request option"));
}

#[test]
fn glm_route_defaults_mandatory_reasoning_to_max_before_strict_activation() {
    let captured = Arc::new(Mutex::new(None));
    let http = heycode_http::HttpService::new(Arc::new(CaptureTransport { body: captured }));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        http,
        super::openrouter_transform_options(),
    )
    .unwrap();
    assert!(Provider::inference_adapter(&provider).is_some());

    let draft = RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: OpenRouterProvider::DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Provider::request_options(&provider),
        temperature: None,
        max_output_tokens: Some(1024),
        purpose: CallPurpose::Evaluation,
    };
    let call = InferenceAdapter::resolve(&provider, draft.clone(), &model()).unwrap();
    assert_eq!(call.reasoning_effort().unwrap().as_str(), "max");
    assert!(call.defaults().reasoning_effort);

    let mut unsupported = draft;
    unsupported.reasoning_effort = Some(ReasoningEffortId::new("medium").unwrap());
    assert!(
        InferenceAdapter::resolve(&provider, unsupported, &model()).is_err(),
        "GLM catalog evidence accepts only max/high/low"
    );
}

#[test]
fn every_model_exposes_its_own_published_effort_vocabulary() {
    let http = heycode_http::HttpService::new(Arc::new(CaptureTransport {
        body: Arc::new(Mutex::new(None)),
    }));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        http,
        super::openrouter_transform_options(),
    )
    .unwrap();

    // A published vocabulary reaches the control exactly, in published order.
    let graded = published_model("vendor/graded", &["low", "medium", "high"], Some("medium"));
    let options = InferenceAdapter::reasoning_effort_options(&provider, &graded)
        .unwrap()
        .expect("a published vocabulary is selectable");
    assert_eq!(
        options
            .choices()
            .iter()
            .map(ReasoningEffortId::as_str)
            .collect::<Vec<_>>(),
        ["low", "medium", "high"]
    );
    assert_eq!(
        options.default().map(ReasoningEffortId::as_str),
        Some("medium")
    );

    // The verified GLM route keeps the list this adapter proved for it.
    let glm = InferenceAdapter::reasoning_effort_options(&provider, &model())
        .unwrap()
        .expect("the verified GLM route stays selectable");
    assert_eq!(
        glm.choices()
            .iter()
            .map(ReasoningEffortId::as_str)
            .collect::<Vec<_>>(),
        ["max", "high", "low"]
    );

    // A reasoning-capable model whose row named no vocabulary exposes no
    // control at all rather than borrowing the GLM list.
    for silent in [
        ModelDescriptor {
            id: "vendor/silent".to_owned(),
            reasoning: None,
            ..model()
        },
        published_model("vendor/flags-only", &[], None),
    ] {
        assert_eq!(
            InferenceAdapter::reasoning_effort_options(&provider, &silent).unwrap(),
            None,
            "an unpublished vocabulary is unknown, never the GLM list"
        );
    }

    // A non-reasoning model stays without an effort control.
    let mut text_only = published_model("vendor/text", &["low", "high"], Some("low"));
    text_only.capabilities.reasoning = CapabilitySupport::Unsupported;
    assert_eq!(
        InferenceAdapter::reasoning_effort_options(&provider, &text_only).unwrap(),
        None
    );
}

#[test]
fn resolution_accepts_only_the_effort_values_that_model_published() {
    let http = heycode_http::HttpService::new(Arc::new(CaptureTransport {
        body: Arc::new(Mutex::new(None)),
    }));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        http,
        super::openrouter_transform_options(),
    )
    .unwrap();
    let graded = published_model("vendor/graded", &["low", "medium", "high"], Some("medium"));
    let draft = |effort: Option<&str>| RequestDraft {
        provider: OpenRouterProvider::NAME.to_owned(),
        model: graded.id.clone(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(2),
        effective_at_ms: 3,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("hello"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: effort.map(|effort| ReasoningEffortId::new(effort).unwrap()),
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Provider::request_options(&provider),
        temperature: None,
        max_output_tokens: Some(1024),
        purpose: CallPurpose::Evaluation,
    };

    let call = InferenceAdapter::resolve(&provider, draft(Some("high")), &graded).unwrap();
    assert_eq!(call.reasoning_effort().unwrap().as_str(), "high");

    // `max` belongs to the GLM route alone and is not a value this model
    // published, so it never reaches the wire.
    assert!(InferenceAdapter::resolve(&provider, draft(Some("max")), &graded).is_err());

    // The published default applies when the caller names no effort.
    let defaulted = InferenceAdapter::resolve(&provider, draft(None), &graded).unwrap();
    assert_eq!(defaulted.reasoning_effort().unwrap().as_str(), "medium");

    // A model that published no vocabulary sends no reasoning field and
    // refuses a guessed one.
    let silent = ModelDescriptor {
        id: "vendor/silent".to_owned(),
        reasoning: None,
        ..model()
    };
    let mut silent_draft = draft(None);
    silent_draft.model = silent.id.clone();
    assert!(
        InferenceAdapter::resolve(&provider, silent_draft.clone(), &silent)
            .unwrap()
            .reasoning_effort()
            .is_none()
    );
    silent_draft.reasoning_effort = Some(ReasoningEffortId::new("max").unwrap());
    assert!(InferenceAdapter::resolve(&provider, silent_draft, &silent).is_err());
}

#[tokio::test]
async fn glm_tool_turn_preserves_and_replays_complete_reasoning_details() {
    let scripts = vec![
        vec![
            sse(serde_json::json!({
                "id":"glm_tool_1",
                "choices":[{"index":0,"delta":{"reasoning_details":[{
                    "type":"reasoning.text","text":"inspect first","signature":null,
                    "id":"reasoning-1","format":"unknown","index":0
                }]},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"glm_tool_1",
                "choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"call_1","type":"function",
                    "function":{"name":"read","arguments":"{\"path\":\"README.md\"}"}
                }]},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"glm_tool_1",
                "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
            })),
            done(),
        ],
        vec![
            sse(serde_json::json!({
                "id":"glm_tool_2",
                "choices":[{"index":0,"delta":{"content":"done"},"finish_reason":null}]
            })),
            sse(serde_json::json!({
                "id":"glm_tool_2",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
            })),
            done(),
        ],
    ];
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(Arc::new(ScriptCaptureTransport {
            scripts: Mutex::new(scripts),
            bodies: bodies.clone(),
        })),
        super::openrouter_transform_options(),
    )
    .unwrap();

    let first = InferenceAdapter::resolve(
        &provider,
        provider_draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("read it"))],
        ),
        &model(),
    )
    .unwrap();
    assert_eq!(first.reasoning_effort().unwrap().as_str(), "max");
    let first_events = InferenceAdapter::stream(&provider, first)
        .collect::<Vec<_>>()
        .await;
    assert!(first_events.iter().any(|event| matches!(
        event, Ok(InferenceEvent::ReasoningDelta(text)) if text == "inspect first"
    )));
    let state = first_events
        .iter()
        .find_map(|event| match event {
            Ok(InferenceEvent::ProviderState(state)) => Some(state.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.data()["reasoning_details"][0]["id"], "reasoning-1");
    assert_eq!(state.data()["tool_calls"][0]["id"], "call_1");

    let second = InferenceAdapter::resolve(
        &provider,
        provider_draft(
            &provider,
            vec![
                InferenceInput::ProviderState(state),
                InferenceInput::Message(ChatMessage::tool_result("call_1", "contents", false)),
            ],
        ),
        &model(),
    )
    .unwrap();
    let second_events = InferenceAdapter::stream(&provider, second)
        .collect::<Vec<_>>()
        .await;
    assert!(second_events.iter().all(Result::is_ok));

    let captured = bodies.lock().unwrap();
    assert_eq!(captured[0]["reasoning"]["effort"], "max");
    assert_eq!(
        captured[1]["messages"][0]["reasoning_details"][0]["id"],
        "reasoning-1"
    );
}

#[test]
fn glm_tool_history_without_reasoning_state_fails_before_transport() {
    let captured = Arc::new(Mutex::new(None));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(Arc::new(CaptureTransport { body: captured })),
        super::openrouter_transform_options(),
    )
    .unwrap();
    let state = ProviderStateItem::new(
        OpenRouterProvider::NAME,
        OpenRouterProvider::DEFAULT_MODEL,
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({
            "role":"assistant",
            "content":null,
            "tool_calls":[{
                "id":"call_1","type":"function",
                "function":{"name":"read","arguments":"{}"}
            }]
        }),
    )
    .unwrap();
    assert!(
        InferenceAdapter::resolve(
            &provider,
            provider_draft(
                &provider,
                vec![
                    InferenceInput::ProviderState(state),
                    InferenceInput::Message(ChatMessage::tool_result("call_1", "contents", false,)),
                ],
            ),
            &model(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn structured_reasoning_shows_summary_but_never_encrypted_payload() {
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(Arc::new(ScriptCaptureTransport {
            scripts: Mutex::new(vec![vec![
                sse(serde_json::json!({"id":"thinking", "choices":[{"index":0,
                    "delta":{"reasoning_details":[{"type":"reasoning.encrypted", "data":"opaque-secret"}]},
                    "finish_reason":null}]})),
                sse(serde_json::json!({"id":"thinking", "choices":[{"index":0,
                    "delta":{"reasoning_details":[{"type":"reasoning.summary", "summary":"Checking dependencies"}]},
                    "finish_reason":null}]})),
                sse(serde_json::json!({"id":"thinking", "choices":[{"index":0,
                    "delta":{"content":"Done"}, "finish_reason":"stop"}]})),
                done(),
            ]]),
            bodies: Arc::new(Mutex::new(Vec::new())),
        })),
        super::openrouter_transform_options(),
    ).unwrap();
    let call = InferenceAdapter::resolve(
        &provider,
        provider_draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("inspect"))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let visible = events
        .iter()
        .filter_map(|event| match event {
            Ok(InferenceEvent::ReasoningDelta(text)) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["", "Checking dependencies"]);
    assert!(events.iter().any(|event| matches!(event,
        Ok(InferenceEvent::ProviderState(state)) if state.data()["reasoning_details"][0]["data"] == "opaque-secret"
    )));
}

#[tokio::test]
async fn glm_tool_response_without_reasoning_state_publishes_no_finish_or_state() {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let provider = OpenRouterProvider::from_key_with_transport(
        "test-key",
        Some(OpenRouterProvider::DEFAULT_MODEL.to_owned()),
        heycode_http::HttpService::new(Arc::new(ScriptCaptureTransport {
            scripts: Mutex::new(vec![vec![
                sse(serde_json::json!({
                    "id":"glm_bad_tool",
                    "choices":[{"index":0,"delta":{"tool_calls":[{
                        "index":0,"id":"call_1","type":"function",
                        "function":{"name":"read","arguments":"{}"}
                    }]},"finish_reason":null}]
                })),
                sse(serde_json::json!({
                    "id":"glm_bad_tool",
                    "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
                })),
                done(),
            ]]),
            bodies,
        })),
        super::openrouter_transform_options(),
    )
    .unwrap();
    let call = InferenceAdapter::resolve(
        &provider,
        provider_draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("read it"))],
        ),
        &model(),
    )
    .unwrap();
    let events = InferenceAdapter::stream(&provider, call)
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().any(Result::is_err));
    assert!(!events.iter().any(|event| matches!(
        event,
        Ok(InferenceEvent::ProviderState(_) | InferenceEvent::Finish(_))
    )));
}

#[test]
fn perplexity_web_search_uses_its_documented_twenty_result_ceiling() {
    assert!(
        OpenRouterWebSearchPolicy::new(OpenRouterWebSearchEngine::Perplexity, 20, 1, 20, 4_000, 1,)
            .is_ok()
    );
    assert!(
        OpenRouterWebSearchPolicy::new(OpenRouterWebSearchEngine::Perplexity, 21, 1, 21, 4_000, 1,)
            .is_err()
    );
    assert!(
        OpenRouterWebSearchPolicy::new(OpenRouterWebSearchEngine::Exa, 25, 1, 25, 4_000, 1,)
            .is_ok()
    );
}

#[tokio::test]
async fn cache_policy_is_durable_but_only_anthropic_models_receive_message_markers() {
    for id in [
        "anthropic/claude-sonnet-4",
        OpenRouterProvider::DEFAULT_MODEL,
    ] {
        let captured = Arc::new(Mutex::new(None));
        let http = heycode_http::HttpService::new(Arc::new(CaptureTransport {
            body: captured.clone(),
        }));
        let provider = OpenRouterProvider::from_key_with_transport_and_routing(
            "test-key",
            Some(id.to_owned()),
            http,
            OpenRouterRoutingPolicy::official_defaults(),
            super::openrouter_transform_options(),
        )
        .unwrap();
        let mut model = model();
        model.id = id.to_owned();
        let mut draft = provider_draft(
            &provider,
            vec![InferenceInput::Message(ChatMessage::user("exact question"))],
        );
        draft.model = id.to_owned();
        draft.system = Some("exact system".into());
        let call = InferenceAdapter::resolve(&provider, draft.clone(), &model).unwrap();
        assert!(
            call.provider_options()
                .iter()
                .any(|option| option.kind() == "caching"
                    && option.data() == &serde_json::json!({"type":"ephemeral"}))
        );
        assert!(
            InferenceAdapter::stream(&provider, call)
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(Result::is_ok)
        );
        let body = captured.lock().unwrap().clone().unwrap();
        assert!(
            body.get("cache_control").is_none(),
            "top-level control would restrict upstream routing"
        );
        assert!(
            body.get("caching").is_none(),
            "durable policy is not an API field"
        );
        if id.starts_with("anthropic/") {
            assert_eq!(body["messages"][0]["content"][0]["text"], "exact system");
            assert_eq!(body["messages"][1]["content"][0]["text"], "exact question");
            assert_eq!(
                body["messages"][1]["content"][0]["cache_control"],
                serde_json::json!({"type":"ephemeral"})
            );
        } else {
            assert_eq!(body["messages"][0]["content"], "exact system");
            assert_eq!(body["messages"][1]["content"], "exact question");
        }
        draft
            .provider_options
            .retain(|option| option.kind() != "caching");
        draft.provider_options.push(
            heycode_core::ProviderRequestOption::new(
                "openrouter",
                "caching",
                serde_json::json!({"type":"ephemeral", "ttl":"1h"}),
            )
            .unwrap(),
        );
        assert!(InferenceAdapter::resolve(&provider, draft, &model).is_err());
    }
}

#[tokio::test]
async fn anthropic_tool_turn_without_new_reasoning_replays_exact_native_state() {
    // Sonnet 4 can emit a follow-up tool call without another thinking block.
    // Observed with OpenRouter Bedrock -> Vertex tool continuations on 2026-09-11.
    for details in [
        None,
        Some(
            serde_json::json!([{"type":"reasoning.encrypted","data":"opaque-continuation","id":"r1","index":0}]),
        ),
    ] {
        let mut delta = serde_json::json!({"tool_calls":[{
            "index":0,"id":"call_read","type":"function",
            "function":{"name":"read","arguments":"{\"path\":\"fixture.txt\"}"}
        }]});
        if let Some(details) = &details {
            delta["reasoning_details"] = details.clone();
        }
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let provider = OpenRouterProvider::from_key_with_transport(
            "test-key", Some("anthropic/claude-sonnet-4".to_owned()),
            heycode_http::HttpService::new(Arc::new(ScriptCaptureTransport {
                scripts: Mutex::new(vec![vec![
                    sse(serde_json::json!({"id":"anthropic_tool","choices":[{"index":0,"delta":delta,"finish_reason":null}]})),
                    sse(serde_json::json!({"id":"anthropic_tool","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})), done(),
                ], vec![
                    sse(serde_json::json!({"id":"anthropic_done","choices":[{"index":0,"delta":{"content":"verified"},"finish_reason":null}]})),
                    sse(serde_json::json!({"id":"anthropic_done","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})), done(),
                ]]), bodies: bodies.clone(),
            })), super::openrouter_transform_options(),
        ).unwrap();
        let mut descriptor = model();
        descriptor.id = "anthropic/claude-sonnet-4".to_owned();
        let draft = |inputs| {
            let mut draft = provider_draft(&provider, inputs);
            draft.model = descriptor.id.clone();
            draft
        };
        let first = InferenceAdapter::resolve(
            &provider,
            draft(vec![InferenceInput::Message(ChatMessage::user("read"))]),
            &descriptor,
        )
        .unwrap();
        let events = InferenceAdapter::stream(&provider, first)
            .collect::<Vec<_>>()
            .await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        let state = events
            .iter()
            .find_map(|event| match event {
                Ok(InferenceEvent::ProviderState(state)) => Some(state.clone()),
                _ => None,
            })
            .expect("a tool continuation must retain its authoritative native state");
        assert_eq!(state.data().get("reasoning_details"), details.as_ref());
        assert!(state.data().get("reasoning").is_none());
        let expected = state.data().clone();
        let second = InferenceAdapter::resolve(
            &provider,
            draft(vec![
                InferenceInput::ProviderState(state),
                InferenceInput::Message(ChatMessage::tool_result(
                    "call_read",
                    "fixture contents",
                    false,
                )),
            ]),
            &descriptor,
        )
        .unwrap();
        let results = InferenceAdapter::stream(&provider, second)
            .collect::<Vec<_>>()
            .await;
        assert!(results.iter().all(Result::is_ok));
        let body = &bodies.lock().unwrap()[1];
        // The cache marker belongs to the tool result; the native assistant remains exact.
        assert_eq!(body["messages"][0], expected);
        assert_eq!(body["messages"][1]["tool_call_id"], "call_read");
    }
}
