//! Deterministic fixtures shared by the PGCP01 cases.
//!
//! Every live check runs through an injected transport, so no case reaches a
//! network, and the credential fixtures carry realistic secret-shaped material
//! precisely so the redaction cases have something real to catch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    GcpAuthService, GcpHostPlatform, GcpMetadataPolicy, GcpProfileRequest,
};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpResponse, HttpService, HttpSseRequest, HttpTransport,
    SseEventStream, TransportError,
};
use tokio_util::sync::CancellationToken;

/// Metadata URLs the probe is allowed to use, spelled out so a case can assert
/// on the exact request set.
pub(crate) const SERVICE_ACCOUNT_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/";
pub(crate) const PROJECT_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/project/project-id";
pub(crate) const ZONE_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/zone";

/// A `service_account` document with realistic key material. Every `CANARY-`
/// string must stay out of every rendered value.
pub(crate) const SERVICE_ACCOUNT_JSON: &str = r#"{
  "type": "service_account",
  "project_id": "pgcp01-fixture",
  "private_key_id": "CANARY-PRIVATE-KEY-ID-4a7b",
  "private_key": "-----BEGIN PRIVATE KEY-----\nCANARY-PRIVATE-KEY-MATERIAL-do-not-log\n-----END PRIVATE KEY-----\n",
  "client_email": "fixture@pgcp01-fixture.iam.gserviceaccount.com",
  "client_id": "CANARY-CLIENT-ID-99",
  "token_uri": "https://oauth2.googleapis.com/token"
}"#;

/// An `authorized_user` document with realistic refresh material.
pub(crate) const AUTHORIZED_USER_JSON: &str = r#"{
  "type": "authorized_user",
  "client_id": "CANARY-OAUTH-CLIENT-ID.apps.googleusercontent.com",
  "client_secret": "CANARY-OAUTH-CLIENT-SECRET",
  "refresh_token": "CANARY-OAUTH-REFRESH-TOKEN",
  "quota_project_id": "quota-fixture-one"
}"#;

/// Secret-shaped strings that must never appear in any rendered output.
pub(crate) const CANARIES: &[&str] = &[
    "CANARY-PRIVATE-KEY-ID-4a7b",
    "CANARY-PRIVATE-KEY-MATERIAL-do-not-log",
    "BEGIN PRIVATE KEY",
    "CANARY-CLIENT-ID-99",
    "fixture@pgcp01-fixture.iam.gserviceaccount.com",
    "CANARY-OAUTH-CLIENT-ID.apps.googleusercontent.com",
    "CANARY-OAUTH-CLIENT-SECRET",
    "CANARY-OAUTH-REFRESH-TOKEN",
];

pub(crate) type RecordedRequest = (String, Vec<(String, String)>);

/// A transport that answers only the metadata URLs a case maps.
pub(crate) struct FakeMetadata {
    responses: BTreeMap<String, HttpResponse>,
    error: Option<TransportError>,
    stall: bool,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl FakeMetadata {
    /// A transport that maps nothing: every request fails as unreachable.
    pub(crate) fn unreachable() -> Self {
        Self {
            responses: BTreeMap::new(),
            error: Some(TransportError::Network {
                message: "no route".to_owned(),
            }),
            stall: false,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// A transport whose every request fails with `error`.
    pub(crate) fn failing(error: TransportError) -> Self {
        Self {
            responses: BTreeMap::new(),
            error: Some(error),
            stall: false,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// A transport whose requests never settle on their own.
    pub(crate) fn stalling() -> Self {
        Self {
            responses: BTreeMap::new(),
            error: None,
            stall: true,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// A transport that maps the supplied responses and fails anything else.
    pub(crate) fn answering(responses: Vec<(&str, HttpResponse)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(url, response)| (url.to_owned(), response))
                .collect(),
            error: None,
            stall: false,
            requests: Mutex::new(Vec::new()),
        }
    }

    /// An authoritative metadata server with an attached service account.
    pub(crate) fn gce(project: &str, zone: &str) -> Self {
        Self::answering(vec![
            (
                SERVICE_ACCOUNT_URL,
                metadata_ok("aliases\ndefault\nemail\n"),
            ),
            (PROJECT_URL, metadata_ok(project)),
            (ZONE_URL, metadata_ok(zone)),
        ])
    }

    /// URLs and headers of every request this transport received, in order.
    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Just the URLs, in order.
    pub(crate) fn urls(&self) -> Vec<String> {
        self.requests().into_iter().map(|(url, _)| url).collect()
    }
}

impl HttpTransport for FakeMetadata {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.requests.lock().unwrap().push((
            request.url().to_owned(),
            request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
        ));
        if self.stall {
            return Box::pin(async move {
                cancellation.cancelled().await;
                Err(TransportError::Cancelled)
            });
        }
        let mapped = self.responses.get(request.url()).cloned();
        let error = self.error.clone();
        Box::pin(async move {
            match mapped {
                Some(response) => Ok(response),
                None => Err(error.unwrap_or(TransportError::Network {
                    message: "unmapped".to_owned(),
                })),
            }
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

/// A 200 response carrying the authoritative `Metadata-Flavor` marker.
pub(crate) fn metadata_ok(body: &str) -> HttpResponse {
    metadata_response(200, true, body)
}

/// One metadata response with explicit status and marker presence.
pub(crate) fn metadata_response(status: u16, authoritative: bool, body: &str) -> HttpResponse {
    let mut headers = BTreeMap::new();
    if authoritative {
        headers.insert("metadata-flavor".to_owned(), "Google".to_owned());
    }
    HttpResponse {
        status,
        content_type: Some("text/plain".to_owned()),
        headers,
        body: body.as_bytes().to_vec(),
    }
}

/// Build a service over an environment and a transport, keeping the transport
/// so a case can assert on the exact requests made.
pub(crate) fn service(
    environment: MapGcpEnvironment,
    transport: Arc<FakeMetadata>,
) -> GcpAuthService {
    GcpAuthService::new(Arc::new(environment), HttpService::new(transport))
}

/// A request pinned to the Unix layout with the ambient probe switched off.
pub(crate) fn unix_offline() -> GcpProfileRequest {
    GcpProfileRequest {
        platform: GcpHostPlatform::Unix,
        metadata: GcpMetadataPolicy::Disabled,
        ..GcpProfileRequest::default()
    }
}

/// A request pinned to the Unix layout that probes with a short budget.
pub(crate) fn unix_probing() -> GcpProfileRequest {
    GcpProfileRequest {
        platform: GcpHostPlatform::Unix,
        metadata: GcpMetadataPolicy::Probe {
            budget: std::time::Duration::from_millis(50),
        },
        ..GcpProfileRequest::default()
    }
}

/// A request pinned to the Windows layout with the ambient probe switched off.
pub(crate) fn windows_offline() -> GcpProfileRequest {
    GcpProfileRequest {
        platform: GcpHostPlatform::Windows,
        metadata: GcpMetadataPolicy::Disabled,
        ..GcpProfileRequest::default()
    }
}

/// An environment with a Unix home and nothing else set.
pub(crate) fn home_only() -> MapGcpEnvironment {
    MapGcpEnvironment::new().with_var("HOME", "/home/fixture")
}

/// The Unix well-known ADC path under [`home_only`].
pub(crate) const WELL_KNOWN_UNIX: &str =
    "/home/fixture/.config/gcloud/application_default_credentials.json";

/// A responder that proves it is not a metadata server: it answers 200 without
/// the `Metadata-Flavor` marker, which is exactly google-auth's negative ping.
pub(crate) fn no_metadata_server() -> FakeMetadata {
    FakeMetadata::answering(vec![(
        SERVICE_ACCOUNT_URL,
        metadata_response(200, false, "hello from something else"),
    )])
}
