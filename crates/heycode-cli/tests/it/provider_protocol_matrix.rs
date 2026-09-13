//! Synthetic protocol matrix over actual production adapters and the native Agent.
//! No real model quality, account access or provider capability claim is inferred.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_cli::testing::RealCompositionHarness;
use heycode_core::Context;
use heycode_credentials::*;
use heycode_http::*;
use heycode_llm::{CapabilitySupport, Provider, RouteCredential};
use heycode_session::{Session, SessionEventKind, project_requests};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub(super) enum Wire {
    Converse,
    Chat,
    Responses,
    Anthropic,
    Gemini,
}
pub(super) struct MatrixTransport {
    pub(super) wire: Wire,
    pub(super) provider: String,
    pub(super) requests: Mutex<Vec<Value>>,
}
fn event(value: Value) -> Result<SseEvent, TransportError> {
    Ok(SseEvent {
        event: value["type"].as_str().unwrap_or("message").into(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}
impl HttpTransport for MatrixTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        assert!(!cancellation.is_cancelled());
        if self.provider == "lmstudio" {
            assert!(
                !request
                    .headers()
                    .iter()
                    .any(|h| h.name().eq_ignore_ascii_case("authorization")),
                "unauthenticated local route must not emit a dummy bearer key"
            );
        }
        super::cloud_provider_matrix::check_route(&self.provider, request.url(), request.headers());
        let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
        let mut requests = self.requests.lock().unwrap();
        let first = requests.is_empty();
        requests.push(body);
        drop(requests);
        let args = json!({"path":"proof.txt"});
        let events = match self.wire {
            Wire::Converse => panic!("Converse uses binary event streams"),
            Wire::Chat => {
                let mut delta = if first {
                    json!({"tool_calls":[{"index":0,"id":"call_read","type":"function","function":{"name":"read","arguments":args.to_string()}}]})
                } else {
                    json!({"content":"Read verified."})
                };
                if matches!(self.provider.as_str(), "deepseek" | "zai") {
                    delta["reasoning_content"] = json!("Inspect the file before concluding.");
                }
                if self.provider.starts_with("minimax") && first {
                    delta["content"] = json!("<think>Inspect the file before concluding.</think>");
                }
                vec![
                    event(
                        json!({"id":if first{"chat_read"}else{"chat_done"},"choices":[{"index":0,"delta":delta,"finish_reason":if first{"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":40,"completion_tokens":12}}),
                    ),
                    Ok(SseEvent {
                        event: "message".into(),
                        data: "[DONE]".into(),
                        id: None,
                        retry_ms: None,
                    }),
                ]
            }
            Wire::Responses => {
                let item = if first {
                    json!({"type":"function_call","id":"fc_read","call_id":"call_read","name":"read","arguments":args.to_string(),"status":"completed"})
                } else {
                    json!({"type":"message","id":"msg_done","role":"assistant","phase":"final_answer","status":"completed","content":[{"type":"output_text","text":"Read verified.","annotations":[]}]})
                };
                let reasoning = json!({"type":"reasoning","id":"rs_read","summary":[],"encrypted_content":"opaque-matrix-reasoning","status":"completed"});
                let items = if matches!(self.provider.as_str(), "openai" | "cloud-mantle-responses")
                    && first
                {
                    vec![reasoning, item]
                } else {
                    vec![item]
                };
                let mut sequence = 0;
                let mut result = vec![];
                for (index, item) in items.iter().enumerate() {
                    result.push(event(json!({"type":"response.output_item.added","sequence_number":sequence,"output_index":index,"item":item})));
                    sequence += 1;
                    if item["type"] == "message" {
                        result.push(event(json!({"type":"response.output_text.delta","sequence_number":sequence,"output_index":index,"content_index":0,"item_id":"msg_done","delta":"Read verified."})));
                        sequence += 1;
                    }
                    result.push(event(json!({"type":"response.output_item.done","sequence_number":sequence,"output_index":index,"item":item})));
                    sequence += 1;
                }
                result.push(event(json!({"type":"response.completed","sequence_number":sequence,"response":{"id":if first{"resp_read"}else{"resp_done"},"status":"completed","output":items,"usage":{"input_tokens":40,"output_tokens":12}}})));
                result
            }
            Wire::Anthropic => {
                let block = if first {
                    json!({"type":"tool_use","id":"call_read","name":"read","input":{}})
                } else {
                    json!({"type":"text","text":""})
                };
                let delta = if first {
                    json!({"type":"input_json_delta","partial_json":args.to_string()})
                } else {
                    json!({"type":"text_delta","text":"Read verified."})
                };
                vec![
                    event(
                        json!({"type":"message_start","message":{"id":if first{"msg_read"}else{"msg_done"},"type":"message","role":"assistant","model":"claude-opus-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":40,"output_tokens":0}}}),
                    ),
                    event(json!({"type":"content_block_start","index":0,"content_block":block})),
                    event(json!({"type":"content_block_delta","index":0,"delta":delta})),
                    event(json!({"type":"content_block_stop","index":0})),
                    event(
                        json!({"type":"message_delta","delta":{"stop_reason":if first{"tool_use"}else{"end_turn"},"stop_sequence":null},"usage":{"output_tokens":12}}),
                    ),
                    event(json!({"type":"message_stop"})),
                ]
            }
            Wire::Gemini => {
                let part = if first {
                    json!({"functionCall":{"id":"call_read","name":"read","args":args},"thoughtSignature":"opaque-matrix-signature"})
                } else {
                    json!({"text":"Read verified."})
                };
                vec![event(
                    json!({"candidates":[{"index":0,"content":{"role":"model","parts":[part]},"finishReason":"STOP"}],"responseId":if first{"gemini_read"}else{"gemini_done"},"usageMetadata":{"promptTokenCount":40,"candidatesTokenCount":12,"totalTokenCount":52}}),
                )]
            }
        };
        let events = super::cloud_provider_matrix::with_thinking(&self.provider, first, events);
        Box::pin(futures::stream::iter(events))
    }
    fn send(&self, request: HttpRequest, _: CancellationToken) -> BufferedResponseFuture {
        if matches!(self.wire, Wire::Converse) {
            return super::cloud_provider_matrix::converse_response(self, request);
        }
        // LM Studio's actual preparation checks loaded state and native/Chat surfaces.
        assert_eq!(
            self.provider,
            "lmstudio",
            "unexpected buffered provider operation: {}",
            request.url()
        );
        let (status, body) = if request.url().ends_with("/api/v1/models") {
            (
                200,
                json!({"models":[{"type":"llm","key":"local-model","display_name":"Local","loaded_instances":[{"id":"local-model","config":{}}],"capabilities":{"trained_for_tool_use":true}}]}),
            )
        } else if request.url().ends_with("/v1/models") {
            (
                200,
                json!({"object":"list","data":[{"id":"local-model","object":"model"}]}),
            )
        } else {
            (404, json!({}))
        };
        Box::pin(async move {
            Ok(HttpResponse {
                status,
                headers: Default::default(),
                content_type: Some("application/json".into()),
                body: body.to_string().into_bytes(),
            })
        })
    }
}
pub(super) struct MatrixKeys {
    pub(super) id: CredentialProviderId,
}
impl CredentialProvider for MatrixKeys {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }
    fn precedence(&self) -> u16 {
        0
    }
    fn inspect(&self, _: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(CredentialProviderState::configured(
            CredentialSource::Environment,
            false,
        ))
    }
    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(Some(CredentialSecret::new(
            if query.kind.as_str() == "subscription-key" {
                "sk-cp-fixture"
            } else {
                "fixture-key"
            },
        )))
    }
}
fn actual_provider(
    id: &str,
    http: HttpService,
    credentials: CredentialsService,
) -> Arc<dyn Provider> {
    let key = RouteCredential::fixed("fixture-key");
    match id {
        "openai"=>Arc::new(heycode_provider_openai::configure_openai_bridge_complete_hosted_tools(heycode_provider_openai::OpenAiProvider::with_credential(http,key,None).unwrap()).unwrap()),
        "anthropic"=>Arc::new(heycode_provider_anthropic::configure_anthropic_default_server_tools(heycode_provider_anthropic::AnthropicProvider::with_credential(http,key,None).unwrap()).unwrap()),
        "google"=>Arc::new(heycode_provider_google::GoogleGeminiProvider::developer(http,key,"GOOGLE_API_KEY",heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH).unwrap()),
        "deepseek"=>Arc::new(heycode_llm::DeepSeekProvider::from_credential_with_transport(key,Some("deepseek-reasoner".into()),http).unwrap()),
        "openrouter"=>Arc::new(heycode_llm::OpenRouterProvider::from_credential_with_transport(key,Some("openai/gpt-4.1".into()),http,vec![heycode_provider_openrouter::OpenRouterTransformPolicy::all_disabled().provider_option(heycode_provider_openrouter::OpenRouterTransformRequestContext::new(true,false)).unwrap()]).unwrap()),
        "ollama"=>Arc::new(heycode_provider_lmstudio::OllamaInference::new(http,heycode_provider_lmstudio::OllamaEndpoint::local(),"local-model").unwrap()),
        "lmstudio"=>Arc::new(heycode_provider_lmstudio::LmStudioInference::new(http,None,heycode_provider_lmstudio::LmStudioConfig::local(),"local-model").unwrap()),
        "zai"=>Arc::new(heycode_provider_zai::ZaiInference::<heycode_provider_zai::General>::with_credential(http,key,None).unwrap()),
        "minimax"=>Arc::new(heycode_provider_minimax::MiniMaxInference::new(http,credentials,heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::PayAsYouGo>::international(),heycode_provider_minimax::MINIMAX_M3.into()).unwrap()),
        "minimax-token-plan"=>Arc::new(heycode_provider_minimax::MiniMaxInference::new(http,credentials,heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::TokenPlan>::international(),heycode_provider_minimax::MINIMAX_M3.into()).unwrap()),
        "custom-openai"=>Arc::new(heycode_provider_openai_compatible::CustomOpenAiProvider::new(http,heycode_provider_openai_compatible::CustomOpenAiEndpoint::new("http://localhost:8000/v1").unwrap(),heycode_provider_openai_compatible::CustomOpenAiModel::new("custom-model").unwrap(),None).unwrap()),
        "azure-openai"=>Arc::new(heycode_provider_azure::AzureOpenAiProvider::new(http,heycode_provider_azure::AzureResourceName::new("matrix").unwrap(),heycode_provider_azure::AzureDeploymentName::new("matrix-deployment").unwrap(),"AZURE_OPENAI_API_KEY",key).unwrap()),
        other=>{let spec=heycode_provider_compatible::spec(other).unwrap();Arc::new(heycode_provider_compatible::CompatibleProvider::new(*spec,http,"https://gateway.example/v1","gateway-model",key).unwrap())}
    }
}
fn schemas(wire: Wire, body: &Value) -> Vec<(&str, &Value)> {
    match wire {
        Wire::Converse => body["toolConfig"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                (
                    tool["toolSpec"]["name"].as_str().unwrap(),
                    &tool["toolSpec"]["inputSchema"]["json"],
                )
            })
            .collect(),
        Wire::Chat => body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| {
                Some((
                    tool["function"]["name"].as_str()?,
                    &tool["function"]["parameters"],
                ))
            })
            .collect(),
        Wire::Responses => body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| Some((tool["name"].as_str()?, &tool["parameters"])))
            .collect(),
        Wire::Anthropic => body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| Some((tool["name"].as_str()?, &tool["input_schema"])))
            .collect(),
        Wire::Gemini => body["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| {
                assert!(tool.get("parameters").is_none());
                (
                    tool["name"].as_str().unwrap(),
                    &tool["parametersJsonSchema"],
                )
            })
            .collect(),
    }
}
async fn roundtrip(id: &str, wire: Wire) {
    let context = Context::new();
    let credentials = CredentialsService::new();
    credentials
        .register(
            &context,
            Arc::new(MatrixKeys {
                id: CredentialProviderId::new("matrix").unwrap(),
            }),
        )
        .unwrap();
    let transport = Arc::new(MatrixTransport {
        wire,
        provider: id.into(),
        requests: Mutex::new(vec![]),
    });
    let provider = actual_provider(id, HttpService::new(transport.clone()), credentials);
    run_journey(id, wire, transport, provider).await;
}
pub(super) async fn run_journey(
    id: &str,
    wire: Wire,
    transport: Arc<MatrixTransport>,
    provider: Arc<dyn Provider>,
) {
    let mut model = provider.describe_model(&provider.info().default_model);
    // Synthetic fixture admission, separate from actual catalog evidence tests.
    // The test proves adapter/tool execution behavior, not remote model capabilities.
    if !matches!(
        id,
        "minimax"
            | "minimax-token-plan"
            | "custom-openai"
            | "azure-openai"
            | "fireworks"
            | "groq"
            | "mistral"
            | "together"
            | "xai"
    ) {
        model.capabilities.tools = CapabilitySupport::Supported;
    }
    if matches!(id, "openai" | "anthropic" | "openrouter") {
        model.capabilities.native_web = CapabilitySupport::Supported;
    }
    if !id.starts_with("cloud-") || model.context_window.is_none() {
        model.context_window = Some(200_000);
        model.max_output_tokens = Some(16_384);
    }
    let mut harness = RealCompositionHarness::new().unwrap();
    if !id.starts_with("cloud-") && !matches!(id, "azure-openai" | "custom-openai") {
        harness.config_mut().llm.provider = id.into();
    }
    harness.config_mut().llm.model = model.id.clone();
    std::fs::write(
        harness.root().join("workspace/proof.txt"),
        "provider parity evidence\n",
    )
    .unwrap();
    let snapshot = heycode_llm::CatalogSnapshot {
        provider: provider.descriptor(),
        models: vec![model],
        revision: 1,
        fetched_at_ms: 1_800_000_000_000,
    };
    harness.seed_catalog_snapshot(snapshot.clone()).unwrap();
    let world = harness.with_provider(provider).compose().unwrap();
    super::cloud_provider_matrix::register_mantle_catalog(id, world.context(), snapshot);
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let reply = agent
        .send("Read proof.txt using the read tool.")
        .await
        .unwrap_or_else(|e| panic!("{id}: {e:#}"));
    assert_eq!(reply.text, "Read verified.", "{id}");
    let session = world
        .context()
        .get::<Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let session_events = session.lock().unwrap().events().to_vec();
    let persisted = project_requests(&session_events).unwrap();
    assert_eq!(persisted.len(), 2, "{id}");
    assert!(session_events.iter().any(|event|matches!(&event.kind,SessionEventKind::ToolResult{content,..} if content.contains("provider parity evidence"))),"{id}");
    let catalog = agent
        .tool_catalog(&world.context().plugin_inventory())
        .unwrap();
    assert_eq!(
        catalog.prepared_client_count,
        Some(persisted[1].header.tools.len()),
        "{id}"
    );
    assert_eq!(
        catalog
            .tools
            .iter()
            .find(|row| row.name == "read")
            .unwrap()
            .successful_calls,
        Some(1),
        "{id}"
    );
    {
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "{id}");
        super::cloud_provider_matrix::check_replay(id, &requests, &session_events);
        let declarations = schemas(wire, &requests[0]);
        for name in [
            "read",
            "read_many",
            "multi_edit",
            "write",
            "agent",
            "background_shell",
            "background_terminal",
            "exit_plan_mode",
            "workflow",
            "team",
        ] {
            assert!(
                declarations.iter().any(|(n, _)| *n == name),
                "{id} omitted {name}"
            );
        }
        let last_declarations = schemas(wire, &requests[1]);
        for row in catalog.tools.iter().filter(|row| row.kind == "client") {
            assert_eq!(
                row.prepared,
                Some(last_declarations.iter().any(|(name, _)| *name == row.name)),
                "{id}: inspector preparation mismatch for {}",
                row.name
            );
        }
        for tool in &persisted[0].header.tools {
            let actual = declarations
                .iter()
                .find(|(name, _)| *name == tool.name)
                .unwrap_or_else(|| panic!("{id} omitted {}", tool.name));
            assert_eq!(
                *actual.1, tool.parameters,
                "{id} changed {} schema",
                tool.name
            );
        }
        assert!(
            requests[1].to_string().contains("provider parity evidence"),
            "{id} lost tool result"
        );
        match id {
            "minimax" | "minimax-token-plan" => {
                assert_eq!(requests[0]["reasoning_split"], false);
                assert!(
                    requests[1]
                        .to_string()
                        .contains("<think>Inspect the file before concluding.</think>")
                );
            }
            "zai" => {
                assert_eq!(requests[0]["thinking"]["clear_thinking"], false);
                assert!(
                    requests[1]
                        .to_string()
                        .contains("Inspect the file before concluding.")
                );
            }
            "google" => assert!(requests[1].to_string().contains("opaque-matrix-signature")),
            _ => {}
        }
    }
    agent
        .send("Summarize the verified result.")
        .await
        .unwrap_or_else(|e| panic!("{id} second turn: {e:#}"));
    let compact = agent
        .compact("portable-summary", 1, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{id} compaction: {e:#}"));
    assert!(
        matches!(compact, heycode_agent::CompactionOutcome::Applied { .. }),
        "{id}: {compact:?}"
    );
    assert_eq!(
        transport.requests.lock().unwrap().len(),
        4,
        "{id}: portable summary must use actual adapter"
    );
}
macro_rules! matrix {($($name:ident:$id:literal=>$wire:ident),*$(,)?)=>{$(#[tokio::test]async fn $name(){roundtrip($id,Wire::$wire).await;})*};}
matrix! {
    openai:"openai"=>Responses,anthropic:"anthropic"=>Anthropic,google:"google"=>Gemini,
    deepseek_reasoning:"deepseek"=>Chat,glm_reasoning:"zai"=>Chat,minimax:"minimax"=>Chat,minimax_subscription:"minimax-token-plan"=>Chat,
    ollama:"ollama"=>Chat,lmstudio:"lmstudio"=>Chat,openrouter:"openrouter"=>Chat,
    azure:"azure-openai"=>Responses,custom_gateway:"custom-openai"=>Chat,
    fireworks:"fireworks"=>Chat,groq:"groq"=>Chat,mistral:"mistral"=>Chat,together:"together"=>Chat,xai:"xai"=>Chat,
}

#[test]
fn selected_minimax_and_zai_routes_activate_through_production_composition() {
    for (id, reference, secret, model) in [
        (
            "minimax",
            "MINIMAX_API_KEY",
            "fixture-key",
            heycode_provider_minimax::MINIMAX_M3,
        ),
        (
            "minimax-token-plan",
            "MINIMAX_TOKEN_PLAN_KEY",
            "sk-cp-fixture",
            heycode_provider_minimax::MINIMAX_M3,
        ),
        (
            "zai",
            "ZAI_API_KEY",
            "fixture-key",
            heycode_provider_zai::ZAI_GLM_5_3,
        ),
    ] {
        let mut harness = RealCompositionHarness::new().unwrap();
        harness.config_mut().llm.provider = id.into();
        harness.config_mut().llm.model = model.into();
        heycode_cli::write_credential_at(reference, secret, &harness.credentials_root()).unwrap();
        let world = harness
            .without_fake_provider()
            .compose()
            .unwrap_or_else(|e| panic!("{id}: {e:#}"));
        let providers = world
            .context()
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .unwrap();
        let provider = providers.get(id).unwrap();
        let adapter = provider
            .inference_adapter()
            .expect("selected route has actual inference");
        assert_eq!(adapter.descriptor().id, id);
        assert_eq!(
            adapter.authentication_binding(),
            heycode_llm::AuthenticationBinding::Credential(
                heycode_llm::CredentialHandle::new(reference).unwrap()
            )
        );
    }
}

#[test]
fn unsupported_subscription_route_reports_policy_without_using_the_general_entitlement() {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "zai-coding".into();
    let error = harness
        .without_fake_provider()
        .compose()
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("restricted by Z.ai"), "{error}");
    assert!(error.contains("separate general API key"), "{error}");
}

struct DualActivationTransport {
    source: MatrixTransport,
    target: MatrixTransport,
    fail_source: std::sync::atomic::AtomicBool,
    source_failures: std::sync::atomic::AtomicUsize,
    fail_target: std::sync::atomic::AtomicBool,
    target_failures: std::sync::atomic::AtomicUsize,
}
impl HttpTransport for DualActivationTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        let credential = |name: &str| {
            request
                .headers()
                .iter()
                .find(|h| h.name() == name)
                .map(HttpHeader::value)
        };
        if request.url().starts_with("https://source.example/v1/") {
            assert!(credential("authorization") == Some("Bearer source-fixture-secret"));
            assert!(credential("x-api-key").is_none());
            if self.fail_source.load(std::sync::atomic::Ordering::SeqCst) {
                self.source_failures
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Box::pin(futures::stream::iter([Err(TransportError::http(
                    503,
                    "fixture source unavailable",
                    Default::default(),
                ))]));
            }
            self.source.sse(request, cancellation)
        } else {
            assert_eq!(request.url(), "https://api.anthropic.com/v1/messages");
            assert!(credential("x-api-key") == Some("target-fixture-secret"));
            assert!(credential("authorization").is_none());
            if self.fail_target.load(std::sync::atomic::Ordering::SeqCst) {
                self.target_failures
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Box::pin(futures::stream::iter([Err(TransportError::http(
                    503,
                    "fixture unavailable",
                    Default::default(),
                ))]));
            }
            self.target.sse(request, cancellation)
        }
    }
    fn send(&self, request: HttpRequest, _: CancellationToken) -> BufferedResponseFuture {
        assert_eq!(
            request.url(),
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
        assert!(
            request
                .headers()
                .iter()
                .any(|h| h.name() == "x-api-key" && h.value() == "target-fixture-secret")
        );
        Box::pin(async {
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                content_type: Some("application/json".into()),
                body: br#"{"input_tokens":50}"#.to_vec(),
            })
        })
    }
}

fn activation_harness(target_key: bool) -> (RealCompositionHarness, Arc<DualActivationTransport>) {
    let mut harness = RealCompositionHarness::new().unwrap();
    harness.config_mut().llm.provider = "deepseek".into();
    harness.config_mut().llm.model = heycode_llm::DeepSeekProvider::DEFAULT_MODEL.into();
    harness.config_mut().llm.base_url = Some("https://source.example/v1".into());
    harness.config_mut().llm.api_key_env = Some("SOURCE_ONLY_KEY".into());
    std::fs::write(
        harness.root().join("workspace/proof.txt"),
        "provider parity evidence\n",
    )
    .unwrap();
    heycode_cli::write_credential_at(
        "SOURCE_ONLY_KEY",
        "source-fixture-secret",
        &harness.credentials_root(),
    )
    .unwrap();
    if target_key {
        heycode_cli::write_credential_at(
            "ANTHROPIC_API_KEY",
            "target-fixture-secret",
            &harness.credentials_root(),
        )
        .unwrap();
    }
    let snapshots = [
        (
            heycode_llm::DeepSeekProvider::setup_profile().descriptor,
            heycode_llm::DeepSeekProvider::DEFAULT_MODEL,
        ),
        (
            heycode_provider_anthropic::anthropic_profile().descriptor,
            heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5,
        ),
    ]
    .into_iter()
    .map(|(provider, model)| {
        let mut model = heycode_llm::ModelDescriptor::unknown(model);
        model.capabilities.tools = CapabilitySupport::Supported;
        model.capabilities.reasoning = CapabilitySupport::Supported;
        model.capabilities.native_web = CapabilitySupport::Supported;
        model.context_window = Some(200_000);
        model.max_output_tokens = Some(16384);
        Arc::new(heycode_llm::CatalogSnapshot {
            provider,
            models: vec![model],
            revision: 1,
            fetched_at_ms: 1_800_000_000_000,
        })
    })
    .collect::<Vec<_>>();
    use heycode_llm::CatalogPersistence as _;
    heycode_catalog_file::FileCatalogPersistence::open(
        heycode_catalog_file::FileCatalogConfig::new(harness.catalog_cache_path()),
    )
    .unwrap()
    .save(&snapshots, &CancellationToken::new())
    .unwrap();
    let transport = Arc::new(DualActivationTransport {
        source: MatrixTransport {
            wire: Wire::Chat,
            provider: "deepseek".into(),
            requests: Mutex::new(vec![]),
        },
        target: MatrixTransport {
            wire: Wire::Anthropic,
            provider: "anthropic".into(),
            requests: Mutex::new(vec![]),
        },
        fail_source: std::sync::atomic::AtomicBool::new(false),
        source_failures: std::sync::atomic::AtomicUsize::new(0),
        fail_target: std::sync::atomic::AtomicBool::new(false),
        target_failures: std::sync::atomic::AtomicUsize::new(0),
    });
    (
        harness
            .without_fake_provider()
            .with_http_transport(transport.clone()),
        transport,
    )
}

#[tokio::test]
async fn explicit_activation_builds_two_real_routes_with_independent_auth_transport_and_options() {
    let (harness, transport) = activation_harness(true);
    let world = harness.compose().unwrap();
    let registry = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(registry.names(), ["deepseek"]);
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    assert_eq!(
        agent.send("Read proof.txt.").await.unwrap().text,
        "Read verified."
    );
    assert_eq!(transport.source.requests.lock().unwrap().len(), 2);
    assert!(transport.target.requests.lock().unwrap().is_empty());
    registry
        .activate(
            "anthropic",
            heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        agent.selection().provider_name,
        "deepseek",
        "activation must not change routing"
    );
    assert!(
        transport.target.requests.lock().unwrap().is_empty(),
        "activation must not dispatch inference"
    );
    let target = registry.get("anthropic").unwrap();
    assert_eq!(target.credential_reference(), Some("ANTHROPIC_API_KEY"));
    assert_eq!(
        target.inference_adapter().unwrap().authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new("ANTHROPIC_API_KEY").unwrap()
        )
    );
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    routing.select_provider("anthropic").unwrap();
    assert_eq!(
        agent
            .send("Read proof.txt on the selected route.")
            .await
            .unwrap()
            .text,
        "Read verified."
    );
    assert_eq!(transport.source.requests.lock().unwrap().len(), 2);
    assert_eq!(transport.target.requests.lock().unwrap().len(), 2);
    let target_requests = transport.target.requests.lock().unwrap();
    assert!(target_requests[0].get("thinking").is_some());
    assert!(target_requests[0].get("reasoning_effort").is_none());
    assert!(target_requests[0].get("max_tokens").is_some());
    drop(target_requests);
    world.shutdown();
    assert_eq!(
        registry.names(),
        ["deepseek"],
        "late provider must leave with activation owner"
    );
}

#[tokio::test]
async fn activation_rejects_missing_target_keys_unproven_models_and_incomplete_deployments() {
    let (harness, transport) = activation_harness(false);
    let world = harness.compose().unwrap();
    let registry = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let error = registry
        .activate(
            "anthropic",
            heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    assert_eq!(registry.names(), ["deepseek"]);
    assert!(transport.source.requests.lock().unwrap().is_empty());
    assert!(transport.target.requests.lock().unwrap().is_empty());
    world.shutdown();
    let (harness, _) = activation_harness(true);
    let world = harness.compose().unwrap();
    let registry = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert!(
        registry
            .activate("anthropic", "unproven-model", CancellationToken::new())
            .await
            .is_err()
    );
    for provider in [
        "azure-openai",
        "vertex-google",
        "vertex-claude",
        "bedrock",
        "bedrock-mantle",
        "custom-openai",
    ] {
        let error = registry
            .activate(provider, "deployment", CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("separately configured connection")
        );
    }
    let error = registry
        .activate("zai-coding", "model", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("restricted by Z.ai"));
    assert_eq!(registry.names(), ["deepseek"]);
}

#[tokio::test]
async fn activated_hosted_tool_route_preserves_no_replay_safety_on_definitive_http_error() {
    use futures::StreamExt as _;
    let (harness, transport) = activation_harness(true);
    let world = harness.compose().unwrap();
    let registry = world
        .context()
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    let model_id = heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5;
    registry
        .activate("anthropic", model_id, CancellationToken::new())
        .await
        .unwrap();
    let provider = registry.get("anthropic").unwrap();
    let native = world
        .context()
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = native
        .resolve_for_model("anthropic", model_id)
        .unwrap()
        .into_iter()
        .filter(|route| route.kind() == heycode_core::NativeToolImplementationKind::Provider)
        .collect::<Vec<_>>();
    assert!(
        !routes.is_empty(),
        "production hosted-tool candidates must be present"
    );
    let catalogs = world
        .context()
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    let model = catalogs
        .resolve_model("anthropic", model_id, 1_800_000_000_000)
        .unwrap()
        .descriptor;
    let options = provider
        .request_options_for(heycode_llm::ProviderOptionContext::new(&model, &routes))
        .unwrap();
    let draft = heycode_llm::RequestDraft {
        provider: "anthropic".into(),
        model: model_id.into(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1_800_000_000_000),
        effective_at_ms: 1_800_000_000_000,
        system: None,
        inputs: vec![heycode_llm::InferenceInput::Message(
            heycode_llm::ChatMessage::user("Search for the requested document."),
        )],
        tools: vec![],
        input_modalities: vec![heycode_llm::InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: vec![heycode_llm::NativeFeature::Web],
        native_tool_routes: routes,
        provider_options: options,
        temperature: None,
        max_output_tokens: None,
        purpose: heycode_llm::CallPurpose::Conversation,
    };
    let adapter = provider.inference_adapter().unwrap();
    let call = adapter.resolve(draft, &model).unwrap();
    assert_eq!(
        call.retry_spec().safety(),
        heycode_llm::RetrySafety::Never,
        "upper-loop fallback must retain this resolved replay veto"
    );
    transport
        .fail_target
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = adapter.stream(call).collect::<Vec<_>>().await;
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].as_ref().unwrap_err().class(),
        heycode_llm::ProviderErrorClass::Overloaded
    );
    assert_eq!(
        transport
            .target_failures
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(transport.source.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn configured_fallback_uses_independent_real_provider_and_prepares_saved_authorization() {
    use std::sync::atomic::Ordering;
    for saved_before_turn in [false, true] {
        let (harness, transport) = activation_harness(true);
        transport.fail_source.store(true, Ordering::SeqCst);
        let world = harness.compose().unwrap();
        let context = world.context();
        let agent = context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap();
        let registry = context
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .unwrap();
        let model = heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5;
        if saved_before_turn {
            let path = agent
                .session()
                .lock()
                .unwrap()
                .path()
                .parent()
                .unwrap()
                .join("fallback.json");
            std::fs::write(
                path,
                serde_json::to_vec(&json!({"provider":"anthropic","model":model})).unwrap(),
            )
            .unwrap();
            assert!(registry.get("anthropic").is_none());
        } else {
            context
                .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .unwrap()
                .get("fallback")
                .unwrap()
                .unwrap()
                .execute(&agent, &format!("anthropic {model}"))
                .await
                .unwrap();
            assert!(registry.get("anthropic").is_some());
        }
        assert_eq!(agent.selection().provider_name, "deepseek");
        assert_eq!(transport.source_failures.load(Ordering::SeqCst), 0);
        assert!(transport.target.requests.lock().unwrap().is_empty());
        let report = agent
            .send("Read proof.txt after configured fallback")
            .await
            .unwrap();
        assert_eq!(report.text, "Read verified.");
        assert_eq!(agent.selection().provider_name, "anthropic");
        assert!(transport.source_failures.load(Ordering::SeqCst) > 0);
        assert_eq!(transport.target.requests.lock().unwrap().len(), 2);
        let session = agent.session().lock().unwrap();
        assert_eq!(
            session
                .events()
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    heycode_session::SessionEventKind::UserMessage { .. }
                ))
                .count(),
            1
        );
        let routing = context
            .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
            .unwrap();
        assert_eq!(routing.selection().unwrap().provider(), "anthropic");
    }
}

#[tokio::test]
async fn unavailable_saved_fallback_keeps_primary_usable_without_late_error_activation() {
    let (harness, transport) = activation_harness(false);
    let world = harness.compose().unwrap();
    let context = world.context();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let path = agent
        .session()
        .lock()
        .unwrap()
        .path()
        .parent()
        .unwrap()
        .join("fallback.json");
    std::fs::write(path,serde_json::to_vec(&json!({"provider":"anthropic","model":heycode_provider_anthropic::ANTHROPIC_CLAUDE_OPUS_5})).unwrap()).unwrap();
    assert_eq!(
        agent.send("Read proof.txt on primary").await.unwrap().text,
        "Read verified."
    );
    assert_eq!(agent.selection().provider_name, "deepseek");
    assert_eq!(transport.source.requests.lock().unwrap().len(), 2);
    assert!(transport.target.requests.lock().unwrap().is_empty());
    assert!(
        context
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .unwrap()
            .get("anthropic")
            .is_none()
    );
}
