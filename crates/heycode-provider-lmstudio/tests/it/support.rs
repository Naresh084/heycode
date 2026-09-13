//! Injected transport and credential fixtures. No test touches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpResponse, HttpService, HttpSseRequest,
    HttpTransport, SseEventStream, TransportError,
};
use heycode_provider_lmstudio::{LM_STUDIO_DEFAULT_BASE_URL, LmStudioSurface};
use tokio_util::sync::CancellationToken;

/// What the scripted transport does for one request URL.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// `200 application/json` with this body.
    Json(&'static str),
    /// An explicit status, content type and body.
    Response(u16, Option<&'static str>, &'static str),
    /// Connection-level failure: nothing answered.
    Refused,
    /// A response that never arrives.
    Hang,
}

/// Per-URL scripted transport. Detection sends one GET per surface, so a map
/// from URL to outcome drives every case deterministically.
pub struct ScriptedTransport {
    outcomes: BTreeMap<String, Outcome>,
    sequences: Mutex<BTreeMap<String, VecDeque<Outcome>>>,
    fallback: Outcome,
    requests: Mutex<Vec<Request>>,
}

/// One observed request, reduced to the facts tests assert on.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: HttpMethod,
    pub url: String,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}

impl ScriptedTransport {
    pub fn new(fallback: Outcome) -> Self {
        Self {
            outcomes: BTreeMap::new(),
            sequences: Mutex::new(BTreeMap::new()),
            fallback,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Script one surface at the documented default origin.
    pub fn on(mut self, surface: LmStudioSurface, outcome: Outcome) -> Self {
        self.outcomes.insert(url(surface), outcome);
        self
    }

    /// Script one exact URL for sibling-provider tests.
    pub fn on_url(mut self, url: impl Into<String>, outcome: Outcome) -> Self {
        self.outcomes.insert(url.into(), outcome);
        self
    }

    /// Script successive responses for repeated reads of one exact URL.
    pub fn on_sequence(
        mut self,
        url: impl Into<String>,
        outcomes: impl IntoIterator<Item = Outcome>,
    ) -> Self {
        self.sequences
            .get_mut()
            .unwrap()
            .insert(url.into(), outcomes.into_iter().collect());
        self
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        let url = request.url().to_string();
        let authorization = request
            .headers()
            .iter()
            .find(|header| header.name() == "authorization")
            .map(|header| header.value().to_owned());
        self.requests.lock().unwrap().push(Request {
            method: request.method(),
            url: url.clone(),
            authorization,
            body: request.body().unwrap_or_default().to_vec(),
        });
        let outcome = self
            .sequences
            .lock()
            .unwrap()
            .get_mut(&url)
            .and_then(VecDeque::pop_front)
            .or_else(|| self.outcomes.get(&url).cloned())
            .unwrap_or_else(|| self.fallback.clone());
        Box::pin(async move {
            match outcome {
                Outcome::Json(body) => Ok(HttpResponse {
                    status: 200,
                    content_type: Some("application/json".to_owned()),
                    headers: BTreeMap::new(),
                    body: body.as_bytes().to_vec(),
                }),
                Outcome::Response(status, content_type, body) => Ok(HttpResponse {
                    status,
                    content_type: content_type.map(str::to_owned),
                    headers: BTreeMap::new(),
                    body: body.as_bytes().to_vec(),
                }),
                Outcome::Refused => Err(TransportError::Network {
                    message: "connection refused".to_owned(),
                }),
                Outcome::Hang => std::future::pending().await,
            }
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

/// Documented probe URL for one surface at the default origin.
pub fn url(surface: LmStudioSurface) -> String {
    format!("{LM_STUDIO_DEFAULT_BASE_URL}{}", surface.path())
}

/// Wrap a scripted transport so tests can still read what it recorded.
pub fn service(transport: &Arc<ScriptedTransport>) -> HttpService {
    HttpService::new(transport.clone())
}

/// Verbatim documented `GET /api/v1/models` envelope.
/// <https://lmstudio.ai/docs/developer/rest/list>
pub const NATIVE_V1_BODY: &str = r#"{"models":[{"type":"llm","publisher":"google","key":"google/gemma-4-26b-a4b","display_name":"Gemma 4 26B A4B"}]}"#;

/// Verbatim documented `GET /api/v0/models` envelope.
/// <https://lmstudio.ai/docs/developer/rest/endpoints>
pub const NATIVE_V0_BODY: &str = r#"{"object":"list","data":[{"id":"qwen2-vl-7b-instruct","object":"model","type":"vlm","publisher":"mlx-community","arch":"qwen2_vl","compatibility_type":"mlx","quantization":"4bit","state":"not-loaded","max_context_length":32768}]}"#;

/// OpenAI-compatible model-list envelope.
/// <https://lmstudio.ai/docs/developer/openai-compat/models>
pub const OPENAI_BODY: &str =
    r#"{"object":"list","data":[{"id":"qwen2-vl-7b-instruct","object":"model"}]}"#;

/// What LM Studio answers on a path it does not route: `200`, not `404`.
/// <https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1323>
pub const UNKNOWN_PATH_BODY: &str = r#"{"error":"Unexpected endpoint or method."}"#;

/// A credentials service whose single provider holds `secret`, or nothing.
///
/// The provider registration is a context effect, so the owning context is
/// returned with the service rather than dropped inside this helper.
pub fn credentials(secret: Option<&str>) -> (heycode_core::Context, Arc<CredentialsService>) {
    struct Fixed {
        id: CredentialProviderId,
        secret: Option<String>,
    }

    impl CredentialProvider for Fixed {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }

        fn precedence(&self) -> u16 {
            0
        }

        fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Ok(if self.secret.is_some() {
                CredentialProviderState::configured(CredentialSource::Environment, false)
            } else {
                CredentialProviderState::unconfigured(false)
            })
        }

        fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(self.secret.as_ref().map(CredentialSecret::new))
        }
    }

    let context = heycode_core::Context::new();
    let service = Arc::new(CredentialsService::new());
    service
        .register(
            &context,
            Arc::new(Fixed {
                id: CredentialProviderId::new("test-fixed").unwrap(),
                secret: secret.map(str::to_owned),
            }),
        )
        .unwrap();
    (context, service)
}

/// The non-secret credential query tests configure a bearer token through.
pub fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("LM_STUDIO_API_TOKEN").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}
