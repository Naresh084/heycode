//! Deterministic credential and HTTP doubles so no test needs a real store or
//! the network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use tokio_util::sync::CancellationToken;

/// Serves a fixed `(reference, kind) -> secret` map at environment precedence.
pub struct MapCredentials {
    id: CredentialProviderId,
    entries: BTreeMap<(String, String), String>,
}

/// Fails every inspection, as a locked or malformed store would.
pub struct FailingCredentials {
    id: CredentialProviderId,
}

impl MapCredentials {
    pub fn new(entries: &[(&str, &str, &str)]) -> Self {
        Self {
            id: CredentialProviderId::new("test-map").unwrap(),
            entries: entries
                .iter()
                .map(|(reference, kind, secret)| {
                    (
                        ((*reference).to_owned(), (*kind).to_owned()),
                        (*secret).to_owned(),
                    )
                })
                .collect(),
        }
    }

    fn lookup(&self, query: &CredentialQuery) -> Option<&String> {
        self.entries.get(&(
            query.reference.as_str().to_owned(),
            query.kind.as_str().to_owned(),
        ))
    }
}

impl CredentialProvider for MapCredentials {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(match self.lookup(query) {
            Some(_) => CredentialProviderState::configured(CredentialSource::Environment, false),
            None => CredentialProviderState::unconfigured(false),
        })
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self.lookup(query).map(CredentialSecret::new))
    }
}

impl FailingCredentials {
    pub fn new() -> Self {
        Self {
            id: CredentialProviderId::new("test-failing").unwrap(),
        }
    }
}

impl CredentialProvider for FailingCredentials {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Err("store is locked".to_owned())
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Err("store is locked".to_owned())
    }
}

/// One scripted reply, in call order.
pub enum Reply {
    Response(HttpResponse),
    Failure(TransportError),
}

/// Successful JSON reply.
pub fn ok(body: serde_json::Value) -> Reply {
    Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status: 200,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    })
}

/// Reply with an explicit status and content type.
pub fn reply(status: u16, content_type: Option<&str>, body: &str) -> Reply {
    Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: content_type.map(str::to_owned),
        body: body.as_bytes().to_vec(),
    })
}

/// URL plus the exact headers one request carried.
pub type RecordedRequest = (String, BTreeMap<String, String>);

/// Replays scripted replies and records every request it was asked to send.
pub struct ScriptedTransport {
    replies: Mutex<Vec<Reply>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl ScriptedTransport {
    pub fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for ScriptedTransport {
    fn send(
        &self,
        request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push((
            request.url().to_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        let mut replies = self.replies.lock().unwrap();
        assert!(!replies.is_empty(), "unexpected extra catalog request");
        match replies.remove(0) {
            Reply::Response(response) => Box::pin(async move { Ok(response) }),
            Reply::Failure(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

/// A credentials service holding one secret under one reference and kind.
pub fn credentials_holding(
    context: &heycode_core::Context,
    entries: &[(&str, &str, &str)],
) -> Arc<CredentialsService> {
    let credentials = CredentialsService::new();
    credentials
        .register(context, Arc::new(MapCredentials::new(entries)))
        .expect("test provider registers once");
    Arc::new(credentials)
}

/// An `HttpService` backed by the scripted transport.
pub fn http_from(transport: Arc<ScriptedTransport>) -> HttpService {
    HttpService::new(transport)
}
