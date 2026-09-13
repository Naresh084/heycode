//! Cloud wrappers through the same real-Agent journey as the direct-provider matrix.
//! All HTTP is scripted: no cloud account, live inference, IAM or model-access claim.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::provider_protocol_matrix::{MatrixKeys, MatrixTransport, Wire, run_journey};
use heycode_authorization_aws::AwsRegion;
use heycode_authorization_gcp::{testing::MapGcpEnvironment, *};
use heycode_core::Context;
use heycode_credentials::*;
use heycode_http::*;
use heycode_llm::{Provider, RouteCredential};
use heycode_session::{SessionEvent, SessionEventKind};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const SIGNATURE: &str = "opaque-cloud-signature";
const CONVERSE_MODEL: &str = "anthropic.claude-sonnet-4-20250514-v1:0";

pub(super) fn check_route(id: &str, url: &str, headers: &[HttpHeader]) {
    let expected = match id {
        "cloud-converse" => "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-sonnet-4-20250514-v1%3A0/converse-stream".into(),
        "cloud-mantle-responses" => "https://bedrock-mantle.us-east-1.api.aws/v1/responses".into(),
        "cloud-mantle-messages" => "https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages".into(),
        "cloud-vertex-gemini" => format!("https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/publishers/google/models/{}:streamGenerateContent?alt=sse", heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH),
        "cloud-vertex-claude" => "https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/publishers/anthropic/models/claude-sonnet-5:streamRawPredict".into(),
        _ => return,
    };
    assert_eq!(url, expected, "{id}");
    let header = |name: &str| {
        headers
            .iter()
            .find(|h| h.name().eq_ignore_ascii_case(name))
            .map(HttpHeader::value)
    };
    if id == "cloud-mantle-messages" {
        assert_eq!(header("x-api-key"), Some("fixture-key"));
        assert_eq!(header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(header("authorization"), None);
    } else {
        assert_eq!(header("authorization"), Some("Bearer fixture-key"));
        assert_eq!(header("x-api-key"), None);
        assert_eq!(header("x-goog-api-key"), None);
        assert_eq!(header("anthropic-version"), None);
    }
}

pub(super) fn with_thinking(
    id: &str,
    first: bool,
    mut events: Vec<Result<SseEvent, TransportError>>,
) -> Vec<Result<SseEvent, TransportError>> {
    if matches!(id, "cloud-mantle-messages" | "cloud-vertex-claude") {
        let start = events[0].as_mut().unwrap();
        let mut value: Value = serde_json::from_str(&start.data).unwrap();
        value["message"]["model"] = json!(if id == "cloud-vertex-claude" {
            "claude-sonnet-5"
        } else {
            "anthropic.claude-sonnet-5"
        });
        start.data = value.to_string();
        if !first {
            return events;
        }
        for event in events.iter_mut().skip(1) {
            let event = event.as_mut().unwrap();
            let mut value: Value = serde_json::from_str(&event.data).unwrap();
            if value.get("index").is_some() {
                value["index"] = json!(1);
            }
            event.data = value.to_string();
        }
        let thinking = [
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Inspect the file before concluding."}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":SIGNATURE}}),
            json!({"type":"content_block_stop","index":0}),
        ].into_iter().map(|value| Ok(SseEvent {event:value["type"].as_str().unwrap().into(),data:value.to_string(),id:None,retry_ms:None}));
        events.splice(1..1, thinking);
    }
    events
}

fn contains_value(tree: &Value, needle: &Value) -> bool {
    tree == needle
        || match tree {
            Value::Array(items) => items.iter().any(|v| contains_value(v, needle)),
            Value::Object(items) => items.values().any(|v| contains_value(v, needle)),
            _ => false,
        }
}

pub(super) fn check_replay(id: &str, requests: &[Value], events: &[SessionEvent]) {
    if !id.starts_with("cloud-") {
        return;
    }
    let items: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AssistantProviderItem { step: 1, item, .. } => Some(item.as_ref()),
            _ => None,
        })
        .collect();
    assert!(!items.is_empty(), "{id} must persist lossless state");
    let expected_protocol = match id {
        "cloud-converse" => heycode_core::ProviderProtocol::BedrockConverse,
        "cloud-mantle-responses" => heycode_core::ProviderProtocol::OpenAiResponses,
        "cloud-vertex-gemini" => heycode_core::ProviderProtocol::GeminiGenerateContent,
        _ => heycode_core::ProviderProtocol::AnthropicMessages,
    };
    for item in items {
        assert_eq!(item.protocol(), expected_protocol, "{id}");
        assert!(
            contains_value(&requests[1], item.data()),
            "{id} failed exact provider-state replay: {}",
            item.data()
        );
    }
    let signature = match id {
        "cloud-mantle-responses" => "opaque-matrix-reasoning",
        "cloud-vertex-gemini" => "opaque-matrix-signature",
        _ => SIGNATURE,
    };
    assert!(
        requests[1].to_string().contains(signature),
        "{id} omitted reasoning signature"
    );
    for body in requests {
        if id == "cloud-vertex-claude" {
            assert!(body.get("model").is_none());
            assert_eq!(
                body["anthropic_version"],
                heycode_provider_google::CLAUDE_VERTEX_ANTHROPIC_VERSION
            );
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut state = u32::MAX;
    for byte in bytes {
        state ^= u32::from(*byte);
        for _ in 0..8 {
            state = if state & 1 == 1 {
                0xEDB8_8320 ^ (state >> 1)
            } else {
                state >> 1
            };
        }
    }
    state ^ u32::MAX
}
fn frame(kind: &str, value: Value) -> Vec<u8> {
    let mut headers = vec![];
    for (name, value) in [
        (":message-type", "event"),
        (":event-type", kind),
        (":content-type", "application/json"),
    ] {
        headers.push(u8::try_from(name.len()).unwrap());
        headers.extend_from_slice(name.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let payload = value.to_string();
    let mut bytes = vec![];
    bytes.extend_from_slice(
        &u32::try_from(16 + headers.len() + payload.len())
            .unwrap()
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&u32::try_from(headers.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes.extend(headers);
    bytes.extend_from_slice(payload.as_bytes());
    bytes.extend_from_slice(&crc32(&bytes).to_be_bytes());
    bytes
}
pub(super) fn converse_response(
    transport: &MatrixTransport,
    request: HttpRequest,
) -> BufferedResponseFuture {
    check_route(&transport.provider, request.url(), request.headers());
    let body: Value = serde_json::from_slice(request.body().unwrap()).unwrap();
    let mut requests = transport.requests.lock().unwrap();
    let first = requests.is_empty();
    requests.push(body);
    drop(requests);
    let mut frames = vec![frame("messageStart", json!({"role":"assistant"}))];
    if first {
        frames.extend([
            frame("contentBlockDelta",json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Inspect the file before concluding."}}})),
            frame("contentBlockDelta",json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"signature":SIGNATURE}}})),
            frame("contentBlockStop",json!({"contentBlockIndex":0})),
            frame("contentBlockStart",json!({"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"call_read","name":"read"}}})),
            frame("contentBlockDelta",json!({"contentBlockIndex":1,"delta":{"toolUse":{"input":json!({"path":"proof.txt"}).to_string()}}})),
            frame("contentBlockStop",json!({"contentBlockIndex":1})),
        ]);
    } else {
        frames.extend([
            frame(
                "contentBlockDelta",
                json!({"contentBlockIndex":0,"delta":{"text":"Read verified."}}),
            ),
            frame("contentBlockStop", json!({"contentBlockIndex":0})),
        ]);
    }
    frames.push(frame(
        "messageStop",
        json!({"stopReason":if first {"tool_use"} else {"end_turn"}}),
    ));
    frames.push(frame("metadata",json!({"usage":{"inputTokens":40,"outputTokens":12,"totalTokens":52},"metrics":{"latencyMs":12}})));
    Box::pin(async move {
        Ok(HttpResponse {
            status: 200,
            headers: Default::default(),
            content_type: Some("application/vnd.amazon.eventstream".into()),
            body: frames.concat(),
        })
    })
}

async fn cloud_roundtrip(id: &str, wire: Wire) {
    let context = Context::new();
    let credentials = CredentialsService::new();
    credentials
        .register(
            &context,
            Arc::new(MatrixKeys {
                id: CredentialProviderId::new("cloud-fixture").unwrap(),
            }),
        )
        .unwrap();
    let transport = Arc::new(MatrixTransport {
        wire,
        provider: id.into(),
        requests: Mutex::new(vec![]),
    });
    let http = HttpService::new(transport.clone());
    let region = AwsRegion::new("us-east-1").unwrap();
    let key = RouteCredential::fixed("fixture-key");
    let provider: Arc<dyn Provider> = match id {
        "cloud-converse" => Arc::new(heycode_provider_aws::BedrockConverseProvider::with_credential(http,&region,key,CONVERSE_MODEL).unwrap()),
        "cloud-mantle-responses" => Arc::new(heycode_provider_aws::MantleResponsesProvider::with_credential(http,&region,key,"openai.gpt-5.6-sol").unwrap()),
        "cloud-mantle-messages" => Arc::new(heycode_provider_aws::MantleMessagesProvider::with_credential(http,&region,key,"anthropic.claude-sonnet-5",Some(8192)).unwrap()),
        "cloud-vertex-gemini" => Arc::new(heycode_provider_google::GoogleGeminiProvider::vertex(http,"https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/publishers/google",key,heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE,heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH).unwrap()),
        "cloud-vertex-claude" => {
            let path = "/fixture/application_default_credentials.json";
            let env = MapGcpEnvironment::new().with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS,path).with_file(path,br#"{"type":"authorized_user"}"#.to_vec());
            let auth = GcpAuthService::new(Arc::new(env),http.clone()).resolve(GcpProfileRequest{project:Some("vertex-fixture".into()),location:Some("global".into()),platform:GcpHostPlatform::Unix,metadata:GcpMetadataPolicy::Disabled},CancellationToken::new()).await;
            let profile = heycode_provider_google::ClaudeVertexProfile::from_gcp(&auth,CredentialQuery::new(CredentialReference::new(heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE).unwrap(),CredentialKind::new("oauth-token").unwrap())).unwrap();
            Arc::new(heycode_provider_google::ClaudeVertexProvider::new(profile,http,&credentials).unwrap())
        }
        _ => panic!("unexpected cloud fixture"),
    };
    run_journey(id, wire, transport, provider).await;
}
#[tokio::test]
async fn bedrock_converse() {
    cloud_roundtrip("cloud-converse", Wire::Converse).await;
}
#[tokio::test]
async fn mantle_responses() {
    cloud_roundtrip("cloud-mantle-responses", Wire::Responses).await;
}
#[tokio::test]
async fn mantle_messages() {
    cloud_roundtrip("cloud-mantle-messages", Wire::Anthropic).await;
}
#[tokio::test]
async fn vertex_gemini() {
    cloud_roundtrip("cloud-vertex-gemini", Wire::Gemini).await;
}
#[tokio::test]
async fn vertex_claude() {
    cloud_roundtrip("cloud-vertex-claude", Wire::Anthropic).await;
}

// Mantle's real catalog plugin is conditional on configured AWS coordinates.
// Supply only synthetic catalog evidence to this adapter-override journey.
struct FixtureCatalog(heycode_llm::CatalogSnapshot);
#[async_trait::async_trait]
impl heycode_llm::ModelCatalog for FixtureCatalog {
    fn provider(&self) -> heycode_llm::ProviderDescriptor {
        self.0.provider.clone()
    }
    async fn fetch(
        &self,
        _: CancellationToken,
    ) -> Result<Vec<heycode_llm::ModelDescriptor>, heycode_llm::CatalogFetchError> {
        Ok(self.0.models.clone())
    }
}
pub(super) fn register_mantle_catalog(
    id: &str,
    context: &Context,
    snapshot: heycode_llm::CatalogSnapshot,
) {
    if id.starts_with("cloud-mantle-") {
        let catalogs = context
            .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
            .unwrap();
        if !catalogs
            .descriptors()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor.id == snapshot.provider.id)
        {
            catalogs
                .register(context, Arc::new(FixtureCatalog(snapshot)))
                .unwrap();
        }
    }
}
