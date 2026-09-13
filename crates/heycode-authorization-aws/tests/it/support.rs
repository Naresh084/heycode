//! Deterministic collaborators shared by the PAWS01 cases.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_core::Context;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsService,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use tokio_util::sync::CancellationToken;

/// Stand-in Bedrock API key. Every "no secret leaked" assertion searches for
/// this exact string.
pub const TEST_BEDROCK_KEY: &str = "bedrock-api-key-must-not-leak";

pub type RecordedRequest = (String, Vec<(String, String)>);
pub type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

pub enum Reply {
    Response(HttpResponse),
    Failure(TransportError),
}

pub struct ScriptedTransport {
    replies: Mutex<Vec<Reply>>,
    requests: RecordedRequests,
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
        assert!(!replies.is_empty(), "unexpected extra HTTP request");
        match replies.remove(0) {
            Reply::Response(response) => Box::pin(async move { Ok(response) }),
            Reply::Failure(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

/// One scripted HTTP service plus the log of what was sent through it.
#[must_use]
pub fn scripted_http(replies: Vec<Reply>) -> (HttpService, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let http = HttpService::new(Arc::new(ScriptedTransport {
        replies: Mutex::new(replies),
        requests: requests.clone(),
    }));
    (http, requests)
}

#[must_use]
pub fn json_reply(status: u16, body: serde_json::Value) -> Reply {
    Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    })
}

#[must_use]
pub fn error_reply(status: u16) -> Reply {
    json_reply(
        status,
        serde_json::json!({"message": "provider-body-must-not-leak"}),
    )
}

#[must_use]
pub fn urls(requests: &RecordedRequests) -> Vec<String> {
    requests
        .lock()
        .unwrap()
        .iter()
        .map(|(url, _)| url.clone())
        .collect()
}

#[must_use]
pub fn bedrock_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("AWS_BEARER_TOKEN_BEDROCK").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

/// A credential provider that always holds `secret` for any reference.
struct StaticProvider {
    id: CredentialProviderId,
    secret: Option<String>,
    failing: bool,
}

impl CredentialProvider for StaticProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(if self.secret.is_some() || self.failing {
            CredentialProviderState::configured(CredentialSource::Environment, false)
        } else {
            CredentialProviderState::unconfigured(false)
        })
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        if self.failing {
            return Err("store-detail-must-not-leak".to_owned());
        }
        Ok(self.secret.as_ref().map(CredentialSecret::new))
    }
}

/// Owns the registry plus the `Context` its registration effect lives in;
/// dropping the context would unregister the provider.
pub struct Credentials {
    pub service: Arc<CredentialsService>,
    _context: Context,
}

fn credentials(secret: Option<&str>, failing: bool) -> Credentials {
    let context = Context::new();
    let service = Arc::new(CredentialsService::new());
    service
        .register(
            &context,
            Arc::new(StaticProvider {
                id: CredentialProviderId::new("test-static").unwrap(),
                secret: secret.map(str::to_owned),
                failing,
            }),
        )
        .unwrap();
    Credentials {
        service,
        _context: context,
    }
}

/// A registry holding exactly one resolvable secret.
#[must_use]
pub fn credentials_with(secret: &str) -> Credentials {
    credentials(Some(secret), false)
}

/// A registry that holds nothing.
#[must_use]
pub fn credentials_empty() -> Credentials {
    credentials(None, false)
}

/// A registry whose only provider fails on resolution.
#[must_use]
pub fn credentials_failing() -> Credentials {
    credentials(None, true)
}
