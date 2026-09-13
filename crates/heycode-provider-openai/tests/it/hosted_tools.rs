//! POA03 provider-owned hosted-tool definitions and completed-item facts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ProviderStateItem,
    ProviderStateKind, ServerToolOutcome,
};
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{
    HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream, TransportError,
};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceAdapter, InferenceEvent, InferenceInput,
    InputModality, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, NativeFeature, OpenAiResponsesAdapter, OpenAiResponsesConfig,
    ProviderOptionContext, RequestDraft,
};
use heycode_provider_openai::{
    OPENAI_GPT_5_6_SOL, OpenAiHostedToolDefinition, OpenAiHostedToolFault,
    OpenAiHostedToolItemRole, OpenAiHostedToolKind, OpenAiHostedToolOutcome, OpenAiHostedTools,
    OpenAiPromptCacheControl, OpenAiPromptCacheMode, OpenAiProvider,
    classify_hosted_tool_citations, classify_hosted_tool_item, hosted_tool_support, openai_profile,
};
use tokio_util::sync::CancellationToken;

struct DeadTransport;

impl HttpTransport for DeadTransport {
    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct ScriptTransport {
    events: Mutex<Option<Vec<Result<SseEvent, TransportError>>>>,
    body: Mutex<Option<serde_json::Value>>,
}

impl ScriptTransport {
    fn new(events: Vec<Result<SseEvent, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Some(events)),
            body: Mutex::new(None),
        })
    }
}

impl HttpTransport for ScriptTransport {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.body.lock().unwrap() = Some(
            serde_json::from_slice(request.body().unwrap_or_default())
                .expect("request body must be JSON"),
        );
        let events = self
            .events
            .lock()
            .unwrap()
            .take()
            .expect("only one request is expected");
        Box::pin(futures::stream::iter(events))
    }
}

fn sse(
    name: &str,
    sequence_number: u64,
    data: serde_json::Value,
) -> Result<SseEvent, TransportError> {
    let mut data = data;
    data["type"] = serde_json::json!(name);
    data["sequence_number"] = serde_json::json!(sequence_number);
    Ok(SseEvent {
        event: name.to_owned(),
        data: data.to_string(),
        id: None,
        retry_ms: None,
    })
}

fn model() -> ModelDescriptor {
    ModelDescriptor {
        id: OPENAI_GPT_5_6_SOL.to_owned(),
        display_name: OPENAI_GPT_5_6_SOL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: None,
        max_output_tokens: None,
        lifecycle: ModelLifecycle::unknown(),
        capabilities: ModelCapabilities {
            native_web: CapabilitySupport::Supported,
            prompt_cache: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

fn state(model: &str, data: serde_json::Value) -> ProviderStateItem {
    ProviderStateItem::new(
        "openai",
        model,
        ProviderProtocol::OpenAiResponses,
        ProviderStateKind::ResponseOutputItem,
        data,
    )
    .unwrap()
}

fn provider_route(kind: OpenAiHostedToolKind) -> NativeToolRoute {
    NativeToolRoute::new(
        kind.as_str(),
        format!("openai:{}", kind.as_str()),
        NativeToolImplementationKind::Provider,
        Some("openai".to_owned()),
    )
    .unwrap()
}

fn provider_routes(kinds: impl IntoIterator<Item = OpenAiHostedToolKind>) -> Vec<NativeToolRoute> {
    let mut routes = kinds.into_iter().map(provider_route).collect::<Vec<_>>();
    routes.sort_by(|left, right| left.logical().cmp(right.logical()));
    routes
}

#[test]
fn default_model_supports_the_exact_official_hosted_tool_set_only() {
    assert_eq!(OpenAiHostedToolKind::ALL.len(), 7);
    for kind in OpenAiHostedToolKind::ALL {
        assert_eq!(
            hosted_tool_support(OPENAI_GPT_5_6_SOL, kind),
            CapabilitySupport::Supported,
            "{kind:?}"
        );
        assert_eq!(
            hosted_tool_support("unlisted-model", kind),
            CapabilitySupport::Unknown,
            "unknown model capability must not become supported"
        );
    }
}

#[test]
fn exact_request_definitions_are_capability_gated_and_secret_free() {
    let definitions = [
        OpenAiHostedToolDefinition::web_search(),
        OpenAiHostedToolDefinition::file_search(["vs_docs"]).unwrap(),
        OpenAiHostedToolDefinition::code_interpreter(),
        OpenAiHostedToolDefinition::hosted_shell(),
        OpenAiHostedToolDefinition::computer_use(),
        OpenAiHostedToolDefinition::image_generation(),
        OpenAiHostedToolDefinition::remote_mcp("docs", "https://mcp.example.test/v1").unwrap(),
    ];
    for definition in definitions {
        let wire = definition.wire_for(OPENAI_GPT_5_6_SOL).unwrap();
        assert_eq!(wire["type"], definition.kind().request_type());
        assert_eq!(
            format!("{definition:?}"),
            format!("{:?}", definition.kind())
        );
        assert_eq!(
            definition.wire_for("unlisted-model").unwrap_err(),
            OpenAiHostedToolFault::UnprovenCapability
        );
    }
}

#[test]
fn provider_plan_configures_the_generic_responses_path_with_one_exact_option() {
    let definitions = vec![
        OpenAiHostedToolDefinition::web_search(),
        OpenAiHostedToolDefinition::file_search(["vs_docs"]).unwrap(),
        OpenAiHostedToolDefinition::code_interpreter(),
        OpenAiHostedToolDefinition::hosted_shell(),
    ];
    let hosted = OpenAiHostedTools::new(OPENAI_GPT_5_6_SOL, definitions).unwrap();
    assert_eq!(hosted.definitions().len(), 4);
    assert_eq!(hosted.provider_option().provider(), "openai");
    assert_eq!(hosted.provider_option().kind(), "hosted-tools");
    assert_eq!(
        hosted.provider_option().data()["definitions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    let config = OpenAiResponsesConfig::with_key(
        openai_profile().descriptor,
        "https://api.openai.test/v1",
        "test-key",
    );
    OpenAiResponsesAdapter::new(
        hosted.configure(config),
        HttpService::new(Arc::new(DeadTransport)),
    )
    .unwrap();

    assert_eq!(
        OpenAiHostedTools::new(
            "unlisted-model",
            vec![OpenAiHostedToolDefinition::web_search()]
        )
        .unwrap_err(),
        OpenAiHostedToolFault::UnprovenCapability
    );

    for definition in [
        OpenAiHostedToolDefinition::computer_use(),
        OpenAiHostedToolDefinition::image_generation(),
        OpenAiHostedToolDefinition::remote_mcp("docs", "https://mcp.example.test/v1").unwrap(),
    ] {
        assert_eq!(
            OpenAiHostedTools::new(OPENAI_GPT_5_6_SOL, vec![definition]).unwrap_err(),
            OpenAiHostedToolFault::MissingSharedBridge
        );
    }
}

#[test]
fn provider_hook_rechecks_the_actual_model_and_exposes_the_exact_option() {
    let provider = OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
    .with_hosted_tools(vec![
        OpenAiHostedToolDefinition::web_search(),
        OpenAiHostedToolDefinition::file_search(["vs_docs"]).unwrap(),
    ])
    .unwrap()
    .with_prompt_caching(
        OpenAiPromptCacheControl::new("hosted-tools-test", OpenAiPromptCacheMode::Implicit)
            .unwrap(),
    )
    .unwrap();
    let static_options = heycode_llm::Provider::request_options(&provider);
    assert_eq!(static_options.len(), 1);
    assert_eq!(static_options[0].kind(), "prompt-cache");
    let native_tool_routes = provider_routes([
        OpenAiHostedToolKind::WebSearch,
        OpenAiHostedToolKind::FileSearch,
    ]);
    let options = heycode_llm::Provider::request_options_for(
        &provider,
        ProviderOptionContext::new(&model(), &native_tool_routes),
    )
    .unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].kind(), "hosted-tools");
    assert_eq!(options[1].kind(), "prompt-cache");

    let draft = RequestDraft {
        provider: "openai".to_owned(),
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("search"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![NativeFeature::Web],
        native_tool_routes,
        provider_options: options.clone(),
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    };
    assert!(provider.resolve(draft.clone(), &model()).is_ok());

    let mut unknown = model();
    unknown.id = "unlisted-model".to_owned();
    unknown.display_name = "unlisted-model".to_owned();
    let mut wrong_model = draft;
    wrong_model.model = unknown.id.clone();
    let error = provider.resolve(wrong_model, &unknown).unwrap_err();
    assert!(format!("{error:?}").contains("hosted-tool capability is unproven"));
}

#[test]
fn request_specific_routes_never_enable_an_unselected_hosted_tool() {
    let bare = OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap();
    let unconfigured_route = provider_route(OpenAiHostedToolKind::WebSearch);
    assert!(
        heycode_llm::Provider::request_options_for(
            &bare,
            ProviderOptionContext::new(&model(), &[unconfigured_route]),
        )
        .is_err(),
        "a provider route without its provider-owned plan must fail loud"
    );

    let provider = OpenAiProvider::new(
        HttpService::new(Arc::new(DeadTransport)),
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
    .with_hosted_tools(vec![
        OpenAiHostedToolDefinition::web_search(),
        OpenAiHostedToolDefinition::file_search(["vs_docs"]).unwrap(),
    ])
    .unwrap()
    .with_prompt_caching(
        OpenAiPromptCacheControl::new("route-filter-test", OpenAiPromptCacheMode::Implicit)
            .unwrap(),
    )
    .unwrap();

    let no_native = heycode_llm::Provider::request_options_for(
        &provider,
        ProviderOptionContext::new(&model(), &[]),
    )
    .unwrap();
    assert_eq!(
        no_native
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        vec!["prompt-cache"]
    );

    let web_route = provider_route(OpenAiHostedToolKind::WebSearch);
    let web_only = heycode_llm::Provider::request_options_for(
        &provider,
        ProviderOptionContext::new(&model(), &[web_route]),
    )
    .unwrap();
    let hosted = web_only
        .iter()
        .find(|option| option.kind() == "hosted-tools")
        .expect("the selected provider route must materialize its option");
    assert_eq!(
        hosted.data()["definitions"],
        serde_json::json!([{"type":"web_search"}])
    );
    assert_eq!(
        web_only
            .iter()
            .map(|option| option.kind())
            .collect::<Vec<_>>(),
        vec!["hosted-tools", "prompt-cache"]
    );
}

#[tokio::test]
async fn shared_responses_parser_normalizes_safe_families_and_replays_exact_state() {
    let definitions = vec![
        OpenAiHostedToolDefinition::web_search(),
        OpenAiHostedToolDefinition::file_search(["vs_docs"]).unwrap(),
        OpenAiHostedToolDefinition::code_interpreter(),
        OpenAiHostedToolDefinition::hosted_shell(),
    ];
    let events = vec![
        sse(
            "response.created",
            0,
            serde_json::json!({"response":{"id":"resp_1","status":"in_progress"}}),
        ),
        sse(
            "response.output_item.added",
            1,
            serde_json::json!({
                "output_index":0,
                "item":{"type":"web_search_call","id":"ws_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            2,
            serde_json::json!({
                "output_index":0,
                "item":{
                    "type":"web_search_call","id":"ws_1","status":"completed",
                    "action":{
                        "type":"search","query":"protocol",
                        "sources":[{"type":"url","url":"https://example.test/source"}]
                    }
                }
            }),
        ),
        sse(
            "response.output_item.added",
            3,
            serde_json::json!({
                "output_index":1,
                "item":{"type":"file_search_call","id":"fs_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            4,
            serde_json::json!({
                "output_index":1,
                "item":{
                    "type":"file_search_call","id":"fs_1","status":"completed",
                    "queries":["protocol"],"results":[{"file_id":"file_1"}]
                }
            }),
        ),
        sse(
            "response.output_item.added",
            5,
            serde_json::json!({
                "output_index":2,
                "item":{"type":"code_interpreter_call","id":"ci_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            6,
            serde_json::json!({
                "output_index":2,
                "item":{
                    "type":"code_interpreter_call","id":"ci_1","status":"completed",
                    "code":"print(1)","container_id":"cntr_1",
                    "outputs":[{"type":"logs","logs":"1"}]
                }
            }),
        ),
        sse(
            "response.output_item.added",
            7,
            serde_json::json!({
                "output_index":3,
                "item":{"type":"shell_call","id":"sh_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            8,
            serde_json::json!({
                "output_index":3,
                "item":{
                    "type":"shell_call","id":"sh_1","call_id":"call_shell_1",
                    "status":"completed",
                    "environment":{"type":"container_reference","container_id":"cntr_1"},
                    "action":{"commands":["pwd"]}
                }
            }),
        ),
        sse(
            "response.output_item.added",
            9,
            serde_json::json!({
                "output_index":4,
                "item":{"type":"shell_call_output","id":"sho_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            10,
            serde_json::json!({
                "output_index":4,
                "item":{
                    "type":"shell_call_output","id":"sho_1","call_id":"call_shell_1",
                    "status":"completed","max_output_length":4096,
                    "output":[{
                        "stdout":"/work","stderr":"",
                        "outcome":{"type":"exit","exit_code":0}
                    }]
                }
            }),
        ),
        sse(
            "response.output_item.added",
            11,
            serde_json::json!({
                "output_index":5,
                "item":{"type":"message","id":"msg_1","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            12,
            serde_json::json!({
                "output_index":5,
                "item":{
                    "type":"message","id":"msg_1","status":"completed","role":"assistant",
                    "phase":"final_answer",
                    "content":[{
                        "type":"output_text","text":"source",
                        "annotations":[{
                            "type":"url_citation","url":"https://example.test/source",
                            "title":"Source","start_index":0,"end_index":6
                        }]
                    }]
                }
            }),
        ),
        sse(
            "response.completed",
            13,
            serde_json::json!({
                "response":{
                    "id":"resp_1","status":"completed","output":[],
                    "usage":{"input_tokens":10,"output_tokens":5}
                }
            }),
        ),
    ];
    let transport = ScriptTransport::new(events);
    let provider = OpenAiProvider::with_base_url(
        HttpService::new(transport.clone()),
        "https://api.openai.test",
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
    .with_hosted_tools(definitions)
    .unwrap();
    let native_tool_routes = provider_routes(OpenAiHostedToolKind::ALL[..4].iter().copied());
    let provider_options = heycode_llm::Provider::request_options_for(
        &provider,
        ProviderOptionContext::new(&model(), &native_tool_routes),
    )
    .unwrap();
    let draft = RequestDraft {
        provider: "openai".to_owned(),
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user(
            "use hosted tools",
        ))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![NativeFeature::Web],
        native_tool_routes,
        provider_options,
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    };
    let call = provider.resolve(draft.clone(), &model()).unwrap();
    let normalized = provider
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(
        normalized
            .iter()
            .filter(|event| matches!(event, InferenceEvent::ServerToolCall { .. }))
            .count(),
        4
    );
    assert_eq!(
        normalized
            .iter()
            .filter(|event| matches!(event, InferenceEvent::ServerToolResult { .. }))
            .count(),
        4
    );
    assert_eq!(
        normalized
            .iter()
            .filter(|event| matches!(event, InferenceEvent::Citation { .. }))
            .count(),
        1
    );
    let replay_state = normalized
        .iter()
        .filter_map(|event| match event {
            InferenceEvent::ProviderState(state) => {
                Some(InferenceInput::ProviderState(state.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replay_state.len(), 6);

    let body = transport.body.lock().unwrap().clone().unwrap();
    let tool_types = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        tool_types,
        vec!["web_search", "file_search", "code_interpreter", "shell"]
    );

    let mut replay = draft;
    replay.inputs.extend(replay_state);
    assert!(provider.resolve(replay, &model()).is_ok());
}

#[tokio::test]
async fn hosted_shell_plan_rejects_a_completed_local_shell_item_before_finish() {
    let events = vec![
        sse(
            "response.created",
            0,
            serde_json::json!({"response":{"id":"resp_local","status":"in_progress"}}),
        ),
        sse(
            "response.output_item.added",
            1,
            serde_json::json!({
                "output_index":0,
                "item":{"type":"shell_call","id":"sh_local","status":"in_progress"}
            }),
        ),
        sse(
            "response.output_item.done",
            2,
            serde_json::json!({
                "output_index":0,
                "item":{
                    "type":"shell_call","id":"sh_local","call_id":"call_local",
                    "status":"completed","environment":{"type":"local"},
                    "action":{"commands":["pwd"]}
                }
            }),
        ),
        sse(
            "response.completed",
            3,
            serde_json::json!({
                "response":{
                    "id":"resp_local","status":"completed","output":[],
                    "usage":{"input_tokens":2,"output_tokens":1}
                }
            }),
        ),
    ];
    let transport = ScriptTransport::new(events);
    let provider = OpenAiProvider::with_base_url(
        HttpService::new(transport),
        "https://api.openai.test",
        "test-key",
        Some(OPENAI_GPT_5_6_SOL.to_owned()),
    )
    .unwrap()
    .with_hosted_tools(vec![OpenAiHostedToolDefinition::hosted_shell()])
    .unwrap();
    let native_tool_routes = vec![provider_route(OpenAiHostedToolKind::HostedShell)];
    let provider_options = heycode_llm::Provider::request_options_for(
        &provider,
        ProviderOptionContext::new(&model(), &native_tool_routes),
    )
    .unwrap();
    let draft = RequestDraft {
        provider: "openai".to_owned(),
        model: OPENAI_GPT_5_6_SOL.to_owned(),
        catalog_revision: None,
        catalog_fetched_at_ms: None,
        effective_at_ms: 1,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("run pwd"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes,
        provider_options,
        temperature: None,
        max_output_tokens: None,
        purpose: CallPurpose::Conversation,
    };
    let call = provider.resolve(draft, &model()).unwrap();
    let events = provider.stream(call).collect::<Vec<_>>().await;
    assert!(events.iter().any(Result::is_err));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            Ok(InferenceEvent::ServerToolCall { .. })
                | Ok(InferenceEvent::ServerToolResult { .. })
                | Ok(InferenceEvent::Finish(_))
        )
    }));
}

#[test]
fn web_search_never_synthesizes_missing_action_and_normalizes_exact_sources() {
    let actionless = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"web_search_call",
            "id":"ws_exact",
            "status":"completed"
        }),
    );
    let event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, actionless.clone()).unwrap();
    assert_eq!(event.kind(), OpenAiHostedToolKind::WebSearch);
    assert_eq!(event.role(), OpenAiHostedToolItemRole::ProviderOperation);
    assert_eq!(event.outcome(), OpenAiHostedToolOutcome::Completed);
    assert_eq!(event.state(), &actionless);
    assert!(
        event.call().is_none(),
        "missing action must not become empty input"
    );
    assert!(
        event.result().is_none(),
        "a result cannot exist without its call"
    );

    let exact = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"web_search_call",
            "id":"ws_exact",
            "status":"completed",
            "action":{
                "type":"search",
                "query":"current protocol",
                "sources":[
                    {"type":"url","url":"https://example.test/a"},
                    {"type":"url","url":"https://example.test/b"}
                ]
            }
        }),
    );
    let event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, exact).unwrap();
    let call = event.call().unwrap();
    assert_eq!(call.id().as_str(), "ws_exact");
    assert_eq!(call.logical(), "web_search");
    assert_eq!(call.provider_name(), "web_search");
    assert_eq!(call.input()["type"], "search");
    let result = event.result().unwrap();
    assert_eq!(result.call_id(), call.id());
    assert_eq!(result.outcome(), ServerToolOutcome::Success);
    assert_eq!(result.output_count(), Some(2));
    assert_eq!(result.sources().len(), 2);
}

#[test]
fn web_url_citations_are_safe_facts_but_not_synthetically_call_correlated() {
    let message = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"message",
            "id":"msg_1",
            "status":"completed",
            "role":"assistant",
            "content":[{
                "type":"output_text",
                "text":"source",
                "annotations":[
                    {
                        "type":"url_citation",
                        "url":"https://example.test/source",
                        "title":"Source title",
                        "start_index":0,
                        "end_index":6
                    },
                    {
                        "type":"file_citation",
                        "file_id":"file_1",
                        "filename":"private.pdf",
                        "index":0
                    }
                ]
            }]
        }),
    );
    let citations = classify_hosted_tool_citations(OPENAI_GPT_5_6_SOL, &message).unwrap();
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0].url(), "https://example.test/source");
    assert_eq!(citations[0].title(), Some("Source title"));
    assert_eq!(citations[0].start_index(), Some(0));
    assert_eq!(citations[0].end_index(), Some(6));
}

#[test]
fn file_and_code_items_normalize_only_observed_inputs_and_output_counts() {
    let file = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"file_search_call",
            "id":"fs_exact",
            "status":"completed",
            "queries":["one", "two"],
            "results":[{"file_id":"file_1"},{"file_id":"file_2"}]
        }),
    );
    let file_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, file).unwrap();
    assert_eq!(file_event.kind(), OpenAiHostedToolKind::FileSearch);
    assert_eq!(
        file_event.call().unwrap().input()["queries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(file_event.result().unwrap().output_count(), Some(2));

    let code = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"code_interpreter_call",
            "id":"ci_exact",
            "status":"completed",
            "code":"print(1)",
            "container_id":"cntr_exact",
            "outputs":[{"type":"logs","logs":"1"}]
        }),
    );
    let code_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, code).unwrap();
    assert_eq!(code_event.kind(), OpenAiHostedToolKind::CodeInterpreter);
    assert_eq!(code_event.call().unwrap().input()["code"], "print(1)");
    assert_eq!(code_event.result().unwrap().output_count(), Some(1));
    assert!(!format!("{code_event:?}").contains("print(1)"));
}

#[test]
fn hosted_shell_uses_exact_call_id_and_classifies_the_paired_output() {
    let call = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"shell_call",
            "id":"sh_item",
            "call_id":"call_shell_exact",
            "status":"completed",
            "environment":{"type":"container_reference","container_id":"cntr_1"},
            "action":{"commands":["printf secret"],"timeout_ms":1000}
        }),
    );
    let call_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, call).unwrap();
    assert_eq!(call_event.kind(), OpenAiHostedToolKind::HostedShell);
    assert_eq!(
        call_event.role(),
        OpenAiHostedToolItemRole::ProviderOperation
    );
    assert_eq!(call_event.call().unwrap().id().as_str(), "call_shell_exact");
    assert!(call_event.result().is_none());

    let output = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"shell_call_output",
            "id":"sh_output_item",
            "call_id":"call_shell_exact",
            "status":"completed",
            "max_output_length":4096,
            "output":[{
                "stdout":"sk-proj-SECRET-CANARY",
                "stderr":"",
                "outcome":{"type":"exit","exit_code":0}
            }]
        }),
    );
    let output_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, output).unwrap();
    assert_eq!(
        output_event.role(),
        OpenAiHostedToolItemRole::ProviderResult
    );
    assert!(output_event.call().is_none());
    let result = output_event.result().unwrap();
    assert_eq!(result.call_id().as_str(), "call_shell_exact");
    assert_eq!(result.output_count(), Some(1));
    let rendered = format!("{output_event:?}");
    assert!(!rendered.contains("SECRET-CANARY"), "{rendered}");
}

#[test]
fn computer_items_remain_client_round_trip_state_not_server_tool_events() {
    for (data, role) in [
        (
            serde_json::json!({
                "type":"computer_call",
                "id":"cmp_item",
                "call_id":"call_computer_exact",
                "status":"completed",
                "pending_safety_checks":[],
                "action":{"type":"screenshot"}
            }),
            OpenAiHostedToolItemRole::ClientAction,
        ),
        (
            serde_json::json!({
                "type":"computer_call_output",
                "id":"cmp_output",
                "call_id":"call_computer_exact",
                "status":"completed",
                "output":{"type":"computer_screenshot","file_id":"file_1"}
            }),
            OpenAiHostedToolItemRole::ClientResult,
        ),
    ] {
        let event =
            classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, state(OPENAI_GPT_5_6_SOL, data)).unwrap();
        assert_eq!(event.kind(), OpenAiHostedToolKind::ComputerUse);
        assert_eq!(event.role(), role);
        assert!(event.call().is_none());
        assert!(event.result().is_none());
    }
}

#[test]
fn image_generation_uses_observed_prompt_and_never_logs_base64_result() {
    let image = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"image_generation_call",
            "id":"ig_exact",
            "status":"completed",
            "revised_prompt":"an exact revised prompt",
            "result":"c2stcHJvai1TRUNSRVQtQ0FOQVJZ"
        }),
    );
    let event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, image).unwrap();
    assert_eq!(event.kind(), OpenAiHostedToolKind::ImageGeneration);
    assert_eq!(event.call().unwrap().id().as_str(), "ig_exact");
    assert_eq!(
        event.call().unwrap().input()["revised_prompt"],
        "an exact revised prompt"
    );
    assert_eq!(event.result().unwrap().output_count(), Some(1));
    assert!(!format!("{event:?}").contains("c2stcHJvai"));

    let actionless = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"image_generation_call",
            "id":"ig_no_prompt",
            "status":"completed",
            "result":"aW1hZ2U="
        }),
    );
    let event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, actionless).unwrap();
    assert!(event.call().is_none());
    assert!(event.result().is_none());
}

#[test]
fn mcp_items_distinguish_inventory_execution_and_approval_without_status_invention() {
    let listing = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"mcp_list_tools",
            "id":"mcpl_exact",
            "server_label":"docs",
            "tools":[
                {"name":"search","input_schema":{"type":"object"}},
                {"name":"fetch","input_schema":{"type":"object"}}
            ]
        }),
    );
    let listing_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, listing).unwrap();
    assert_eq!(listing_event.kind(), OpenAiHostedToolKind::RemoteMcp);
    assert_eq!(
        listing_event.role(),
        OpenAiHostedToolItemRole::ProviderOperation
    );
    assert_eq!(listing_event.call().unwrap().id().as_str(), "mcpl_exact");
    assert_eq!(listing_event.result().unwrap().output_count(), Some(2));

    let call = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"mcp_call",
            "id":"mcp_exact",
            "arguments":"{\"query\":\"safe\"}",
            "name":"search",
            "server_label":"docs",
            "output":"one result"
        }),
    );
    let call_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, call).unwrap();
    assert_eq!(call_event.call().unwrap().id().as_str(), "mcp_exact");
    assert_eq!(
        call_event.call().unwrap().input()["arguments"]["query"],
        "safe"
    );
    assert_eq!(call_event.result().unwrap().output_count(), Some(1));

    let approval = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"mcp_approval_request",
            "id":"mcpr_exact",
            "arguments":"{\"query\":\"safe\"}",
            "name":"search",
            "server_label":"docs"
        }),
    );
    let approval_event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, approval).unwrap();
    assert_eq!(
        approval_event.role(),
        OpenAiHostedToolItemRole::ApprovalRequest
    );
    assert_eq!(
        approval_event.outcome(),
        OpenAiHostedToolOutcome::ApprovalRequired
    );
    assert!(approval_event.call().is_none());
    assert!(approval_event.result().is_none());
}

#[test]
fn mcp_errors_keep_only_the_documented_discriminator() {
    let canary = "sk-proj-SECRET-CANARY-never-render";
    let item = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"mcp_call",
            "id":"mcp_failed",
            "arguments":"{}",
            "name":"search",
            "server_label":"docs",
            "error":{
                "type":"http_error",
                "code":500,
                "message":canary
            }
        }),
    );
    let event = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, item).unwrap();
    assert_eq!(event.outcome(), OpenAiHostedToolOutcome::Failed);
    let result = event.result().unwrap();
    assert_eq!(result.outcome(), ServerToolOutcome::Error);
    assert_eq!(result.error_code(), Some("http_error"));
    assert!(!format!("{event:?}").contains(canary));
}

#[test]
fn malformed_or_unproven_items_refuse_without_echoing_provider_text() {
    let canary = "sk-proj-SECRET-CANARY-never-render";
    let wrong = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({"type":"unknown_hosted_call","id":canary,"status":"completed"}),
    );
    let fault = classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, wrong).unwrap_err();
    assert_eq!(fault, OpenAiHostedToolFault::WrongItemType);
    assert!(!format!("{fault:?} {fault}").contains(canary));

    let fault = classify_hosted_tool_item(
        "unlisted-model",
        state(
            "unlisted-model",
            serde_json::json!({
                "type":"web_search_call",
                "id":"ws_1",
                "status":"completed"
            }),
        ),
    )
    .unwrap_err();
    assert_eq!(fault, OpenAiHostedToolFault::UnprovenCapability);

    let malformed = state(
        OPENAI_GPT_5_6_SOL,
        serde_json::json!({
            "type":"shell_call_output",
            "id":"output_1",
            "status":"completed",
            "output":[]
        }),
    );
    assert_eq!(
        classify_hosted_tool_item(OPENAI_GPT_5_6_SOL, malformed).unwrap_err(),
        OpenAiHostedToolFault::InvalidItem
    );
}

#[test]
fn configuration_that_could_smuggle_credentials_or_unbounded_ids_is_refused() {
    for url in [
        "https://user:token@mcp.example.test/v1",
        "https://mcp.example.test/v1?api_key=secret",
        "file:///tmp/mcp.sock",
    ] {
        assert!(OpenAiHostedToolDefinition::remote_mcp("docs", url).is_err());
    }
    assert!(OpenAiHostedToolDefinition::file_search(std::iter::empty::<&str>()).is_err());
    assert!(OpenAiHostedToolDefinition::file_search(["bad id with spaces"]).is_err());
}
