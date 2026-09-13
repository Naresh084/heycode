//! Unknown-capability endpoints retain normal tools through the real agent loop.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_cli::testing::RealCompositionHarness;
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{CapabilitySupport, Provider, RouteCredential};
use heycode_provider_azure::{AzureDeploymentName, AzureOpenAiProvider, AzureResourceName};
use heycode_provider_openai_compatible::{
    CustomOpenAiEndpoint, CustomOpenAiModel, CustomOpenAiProvider,
};
use heycode_session::{Session, SessionEventKind, project_requests};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct EndpointTransport {
    responses: bool,
    reject: bool,
    requests: Mutex<Vec<Value>>,
}

fn event(value: Value) -> Result<SseEvent, heycode_http::TransportError> {
    Ok(SseEvent {
        event: value["type"].as_str().unwrap_or("message").to_owned(),
        data: value.to_string(),
        id: None,
        retry_ms: None,
    })
}

impl HttpTransport for EndpointTransport {
    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        assert!(!cancellation.is_cancelled());
        let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
        let mut requests = self.requests.lock().unwrap();
        let first = requests.is_empty();
        assert!(requests.len() < 2, "unexpected extra inference request");
        assert!(body["tools"].as_array().unwrap().iter().any(|tool| {
            if self.responses {
                tool["name"] == "read"
            } else {
                tool["function"]["name"] == "read"
            }
        }));
        if !first {
            let inputs = body[if self.responses { "input" } else { "messages" }]
                .as_array()
                .unwrap();
            assert!(
                inputs.iter().any(|input| {
                    if self.responses {
                        input["type"] == "function_call_output"
                            && input["call_id"] == "call_read"
                            && input["output"]
                                .as_str()
                                .is_some_and(|text| text.contains("endpoint tool proof"))
                    } else {
                        input["role"] == "tool"
                            && input["tool_call_id"] == "call_read"
                            && input["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("endpoint tool proof"))
                    }
                }),
                "missing real read result: {inputs:?}"
            );
        }
        requests.push(body);
        if self.reject {
            return Box::pin(futures::stream::iter([Err(
                heycode_http::TransportError::http(
                    400,
                    "private-endpoint-error-body",
                    heycode_http::HttpErrorMetadata::default(),
                ),
            )]));
        }
        let events = if self.responses {
            let item = if first {
                json!({"id":"fc_read","type":"function_call","call_id":"call_read","name":"read",
                    "arguments":"{\"path\":\"proof.txt\"}","status":"completed"})
            } else {
                json!({"id":"msg_done","type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"Read verified.","annotations":[]}]})
            };
            let mut events = vec![
                event(
                    json!({"type":"response.output_item.added","sequence_number":0,"output_index":0,"item":item}),
                ),
                event(
                    json!({"type":"response.output_item.done","sequence_number":if first {1} else {2},"output_index":0,"item":item}),
                ),
                event(
                    json!({"type":"response.completed","sequence_number":if first {2} else {3},"response":{
                    "id":if first {"resp_read"} else {"resp_done"},"status":"completed","output":[item],
                    "usage":{"input_tokens":20,"output_tokens":5}}}),
                ),
            ];
            if !first {
                events.insert(
                    1,
                    event(json!({"type":"response.output_text.delta",
                    "sequence_number":1,"output_index":0,"content_index":0,
                    "item_id":"msg_done","delta":"Read verified."})),
                );
            }
            events
        } else {
            let delta = if first {
                json!({"tool_calls":[{"index":0,"id":"call_read","type":"function","function":{
                    "name":"read","arguments":"{\"path\":\"proof.txt\"}"}}]})
            } else {
                json!({"content":"Read verified."})
            };
            vec![
                event(
                    json!({"id":if first {"chat_read"} else {"chat_done"},"choices":[{
                "index":0,"delta":delta,"finish_reason":if first {"tool_calls"} else {"stop"}}],
                "usage":{"prompt_tokens":20,"completion_tokens":5}}),
                ),
                Ok(SseEvent {
                    event: "message".to_owned(),
                    data: "[DONE]".to_owned(),
                    id: None,
                    retry_ms: None,
                }),
            ]
        };
        Box::pin(futures::stream::iter(events))
    }
}

async fn roundtrip(responses: bool, reject: bool) {
    let transport = Arc::new(EndpointTransport {
        responses,
        reject,
        requests: Mutex::new(Vec::new()),
    });
    let http = HttpService::new(transport.clone());
    let (provider, model): (Arc<dyn Provider>, &str) = if responses {
        (
            Arc::new(
                AzureOpenAiProvider::new(
                    http,
                    AzureResourceName::new("team-agent").unwrap(),
                    AzureDeploymentName::new("prod-gpt").unwrap(),
                    "AZURE_OPENAI_API_KEY",
                    RouteCredential::fixed("fixture-key"),
                )
                .unwrap(),
            ),
            "prod-gpt",
        )
    } else {
        (
            Arc::new(
                CustomOpenAiProvider::new(
                    http,
                    CustomOpenAiEndpoint::new("http://localhost:8000/v1").unwrap(),
                    CustomOpenAiModel::new("local/model").unwrap(),
                    None,
                )
                .unwrap(),
            ),
            "local/model",
        )
    };
    assert_eq!(
        provider.describe_model(model).capabilities.tools,
        CapabilitySupport::Unknown
    );
    let mut harness = RealCompositionHarness::new().unwrap();
    std::fs::write(
        harness.root().join("workspace/proof.txt"),
        "endpoint tool proof\n",
    )
    .unwrap();
    harness.config_mut().llm.model = model.to_owned();
    harness
        .seed_catalog_snapshot(heycode_llm::CatalogSnapshot {
            provider: provider.descriptor(),
            models: vec![provider.describe_model(model)],
            revision: 1,
            fetched_at_ms: 1_800_000_000_000,
        })
        .unwrap();
    let world = harness.with_provider(provider.clone()).compose().unwrap();
    let agent = world
        .context()
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let result = agent.send("Read proof.txt using the read tool.").await;
    let expected_requests = if reject {
        let error = result.unwrap_err().to_string();
        assert!(!error.contains("private-endpoint-error-body"));
        1
    } else {
        assert_eq!(result.unwrap().text, "Read verified.");
        2
    };
    assert_eq!(transport.requests.lock().unwrap().len(), expected_requests);
    assert_eq!(
        provider.describe_model(model).capabilities.tools,
        CapabilitySupport::Unknown
    );
    let session = world
        .context()
        .get::<Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let session = session.lock().unwrap();
    let requests = project_requests(session.events()).unwrap();
    assert_eq!(requests.len(), expected_requests);
    for request in requests {
        assert!(request.header.tools.iter().any(|tool| tool.name == "read"));
    }
    let has_read_result = session.events().iter().any(|event| {
        matches!(&event.kind,
        SessionEventKind::ToolResult { content, .. } if content.contains("endpoint tool proof"))
    });
    assert_eq!(has_read_result, !reject);
}

#[tokio::test]
async fn azure_unknown_deployment_executes_and_replays_a_real_tool() {
    roundtrip(true, false).await;
}

#[tokio::test]
async fn custom_unknown_model_executes_and_replays_a_real_tool() {
    roundtrip(false, false).await;
}

#[tokio::test]
async fn azure_endpoint_rejection_is_logged_without_tool_free_fallback() {
    roundtrip(true, true).await;
}

#[tokio::test]
async fn custom_endpoint_rejection_is_logged_without_tool_free_fallback() {
    roundtrip(false, true).await;
}
