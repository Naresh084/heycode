//! Injected doubles shared by the PGCP02 cases.
//!
//! Every case drives the catalog through a recording transport, so no test
//! reaches the network and each one can assert the exact URL and header set
//! that went to the wire.

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
    SseEventStream,
};
use heycode_provider_google::GOOGLE_API_KEY_REFERENCE;
use tokio_util::sync::CancellationToken;

pub(crate) const TEST_SECRET: &str = "test-not-a-real-google-key";

/// One recorded request: URL plus every header name/value in send order.
pub(crate) type RecordedRequest = (String, Vec<(String, String)>);
pub(crate) type RecordedRequests = Arc<Mutex<Vec<RecordedRequest>>>;

pub(crate) struct RecordingTransport {
    responses: Mutex<Vec<HttpResponse>>,
    requests: RecordedRequests,
}

impl HttpTransport for RecordingTransport {
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
        let mut responses = self.responses.lock().unwrap();
        assert!(!responses.is_empty(), "unexpected extra catalog request");
        let response = responses.remove(0);
        Box::pin(async move { Ok(response) })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

pub(crate) struct SecretProvider {
    pub(crate) id: CredentialProviderId,
    pub(crate) secret: Option<String>,
}

impl CredentialProvider for SecretProvider {
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

pub(crate) fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(GOOGLE_API_KEY_REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

pub(crate) fn credentials(secret: Option<&str>) -> (Context, Arc<CredentialsService>) {
    let context = Context::new();
    let credentials = Arc::new(CredentialsService::new());
    credentials
        .register(
            &context,
            Arc::new(SecretProvider {
                id: CredentialProviderId::new("test-secret").unwrap(),
                secret: secret.map(str::to_owned),
            }),
        )
        .unwrap();
    (context, credentials)
}

pub(crate) fn http(responses: Vec<HttpResponse>) -> (HttpService, RecordedRequests) {
    let requests: RecordedRequests = Arc::new(Mutex::new(Vec::new()));
    let service = HttpService::new(Arc::new(RecordingTransport {
        responses: Mutex::new(responses),
        requests: requests.clone(),
    }));
    (service, requests)
}

pub(crate) fn response(status: u16, body: serde_json::Value) -> HttpResponse {
    HttpResponse {
        headers: BTreeMap::new(),
        status,
        content_type: Some("application/json".to_owned()),
        body: body.to_string().into_bytes(),
    }
}
