//! PGCP02 OAuth access to the Gemini Developer API with a PGCP01 quota
//! project.
//!
//! The documented ADC recipe is an access token plus an explicit quota project
//! (<https://ai.google.dev/gemini-api/docs/oauth>):
//!
//! ```text
//! curl -X GET https://generativelanguage.googleapis.com/v1/models \
//!   -H "Authorization: Bearer ${access_token}" \
//!   -H "x-goog-user-project: ${project_id}"
//! ```
//!
//! The project comes from `heycode-authorization-gcp`, whose three project states
//! stay distinct here: unset and malformed are determinate authorization
//! failures, while undetermined is reported as unavailable and never as a
//! negative finding.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_authorization_gcp::{
    ENV_GOOGLE_CLOUD_PROJECT, ENV_HOME, GcpAuthService, GcpEnvironment, GcpFileError,
    GcpHostPlatform, GcpMetadataPolicy, GcpProfileRequest, SERVICE_GCP_AUTH,
    testing::MapGcpEnvironment,
};
use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{CredentialProviderId, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpResponse, HttpService, SERVICE_HTTP};
use heycode_llm::{
    CatalogFailureKind, CatalogRefreshMode, CatalogRegistry, ModelCatalog, SERVICE_MODELS,
};
use heycode_provider_google::{
    GOOGLE_GEMINI_3_7_FLASH, GeminiCatalog, GoogleCatalogConfig, google_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    RecordedRequests, SecretProvider, TEST_SECRET, credentials, http, query, response,
};

const PROJECT: &str = "heycode-example-42";
const FIRST_PAGE_URL: &str = "https://generativelanguage.googleapis.com/v1/models?pageSize=1000";

fn model_page() -> serde_json::Value {
    serde_json::json!({
        "models": [{
            "name": format!("models/{GOOGLE_GEMINI_3_7_FLASH}"),
            "baseModelId": GOOGLE_GEMINI_3_7_FLASH,
            "version": "3.7",
            "displayName": "Gemini 3.7 Flash",
            "inputTokenLimit": 1_048_576,
            "outputTokenLimit": 65_536,
            "supportedGenerationMethods": ["generateContent"],
            "thinking": true
        }]
    })
}

/// A profile request that never touches the network: the ambient metadata
/// probe is off, so every ambient fact stays explicitly undetermined.
fn offline_request() -> GcpProfileRequest {
    GcpProfileRequest {
        project: None,
        location: None,
        platform: GcpHostPlatform::Unix,
        metadata: GcpMetadataPolicy::Disabled,
    }
}

fn gcp(environment: Arc<dyn GcpEnvironment>) -> Arc<GcpAuthService> {
    // The auth service owns its own transport; the catalog's recording
    // transport stays free to assert only catalog requests.
    let (probe_http, _) = http(Vec::new());
    Arc::new(GcpAuthService::new(environment, probe_http))
}

fn make_catalog(
    environment: Arc<dyn GcpEnvironment>,
    responses: Vec<HttpResponse>,
) -> (Context, GeminiCatalog, RecordedRequests) {
    let (context, service) = credentials(Some(TEST_SECRET));
    let (catalog_http, requests) = http(responses);
    let catalog = GeminiCatalog::oauth_quota_project(
        catalog_http,
        service,
        query(),
        gcp(environment),
        offline_request(),
    )
    .unwrap();
    (context, catalog, requests)
}

/// An environment whose only fact is a valid project id.
fn project_environment(project: &str) -> Arc<dyn GcpEnvironment> {
    Arc::new(MapGcpEnvironment::new().with_var(ENV_GOOGLE_CLOUD_PROJECT, project))
}

/// An environment with a locatable but empty gcloud configuration directory
/// and no project variable at all.
fn empty_environment() -> Arc<dyn GcpEnvironment> {
    Arc::new(MapGcpEnvironment::new().with_var(ENV_HOME, "/home/heycode"))
}

#[tokio::test]
async fn an_oauth_refresh_sends_a_bearer_token_and_the_resolved_quota_project() {
    let (_context, catalog, requests) = make_catalog(
        project_environment(PROJECT),
        vec![response(200, model_page())],
    );
    let models = catalog.fetch(CancellationToken::new()).await.unwrap();
    assert_eq!(models.len(), 1);

    // The token is a bearer credential, the project is the documented quota
    // header, and the API-key header is absent: the two modes are not
    // interchangeable dialects that both get sent.
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            FIRST_PAGE_URL.to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                ("authorization".to_owned(), format!("Bearer {TEST_SECRET}")),
                ("x-goog-user-project".to_owned(), PROJECT.to_owned()),
            ]
        )]
    );
}

#[tokio::test]
async fn a_malformed_quota_project_is_unauthorized_before_any_catalog_request() {
    // `Not A Project` violates the documented project-id charset, which is a
    // determinate finding: there is no usable quota project.
    let (_context, catalog, requests) = make_catalog(
        project_environment("Not A Project"),
        vec![response(200, model_page())],
    );
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_unset_quota_project_is_unauthorized_before_any_catalog_request() {
    // A responder that answers without the metadata flavor header proves it is
    // not a metadata server, so every documented project source has now been
    // checked determinately and none is set.
    struct NotAMetadataServer;

    impl heycode_http::HttpTransport for NotAMetadataServer {
        fn send(
            &self,
            _request: heycode_http::HttpRequest,
            _cancellation: CancellationToken,
        ) -> heycode_http::BufferedResponseFuture {
            Box::pin(async {
                Ok(HttpResponse {
                    headers: BTreeMap::new(),
                    status: 200,
                    content_type: Some("text/html".to_owned()),
                    body: b"<html>captive portal</html>".to_vec(),
                })
            })
        }

        fn sse(
            &self,
            _request: heycode_http::HttpSseRequest,
            _cancellation: CancellationToken,
        ) -> heycode_http::SseEventStream {
            Box::pin(futures::stream::empty())
        }
    }

    let (context, service) = credentials(Some(TEST_SECRET));
    let (catalog_http, requests) = http(vec![response(200, model_page())]);
    let auth = Arc::new(GcpAuthService::new(
        empty_environment(),
        HttpService::new(Arc::new(NotAMetadataServer)),
    ));
    let catalog = GeminiCatalog::oauth_quota_project(
        catalog_http,
        service,
        query(),
        auth,
        GcpProfileRequest {
            metadata: GcpMetadataPolicy::probe(),
            ..offline_request()
        },
    )
    .unwrap();

    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unauthorized);
    assert!(requests.lock().unwrap().is_empty());
    drop(context);
}

#[tokio::test]
async fn an_undetermined_quota_project_is_unavailable_rather_than_a_negative_finding() {
    // Nothing names a project and the ambient probe is off, so PGCP01 reaches
    // no determinate answer. "Could not determine" is not "there is none", so
    // this must not be reported as an authorization failure.
    let (_context, catalog, requests) =
        make_catalog(empty_environment(), vec![response(200, model_page())]);
    let error = catalog.fetch(CancellationToken::new()).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Unavailable);
    assert!(requests.lock().unwrap().is_empty());
}

/// An environment that answers with a different project each time it is asked.
struct RotatingEnvironment {
    reads: Mutex<usize>,
    projects: Vec<String>,
}

impl GcpEnvironment for RotatingEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        if name != ENV_GOOGLE_CLOUD_PROJECT {
            return None;
        }
        let mut reads = self.reads.lock().unwrap();
        let value = self.projects.get(*reads).cloned();
        *reads += 1;
        value
    }

    fn read_file(&self, _path: &Path, _max_bytes: usize) -> Result<Vec<u8>, GcpFileError> {
        Err(GcpFileError::NotFound)
    }
}

#[tokio::test]
async fn the_quota_project_is_resolved_on_every_refresh_so_a_changed_selection_is_visible() {
    let environment = Arc::new(RotatingEnvironment {
        reads: Mutex::new(0),
        projects: vec![
            "heycode-first-000".to_owned(),
            "heycode-second-11".to_owned(),
        ],
    });
    let (_context, catalog, requests) = make_catalog(
        environment,
        vec![response(200, model_page()), response(200, model_page())],
    );

    catalog.fetch(CancellationToken::new()).await.unwrap();
    catalog.fetch(CancellationToken::new()).await.unwrap();

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|(_, headers)| headers
                .iter()
                .find(|(name, _)| name == "x-goog-user-project")
                .map(|(_, value)| value.clone())
                .unwrap())
            .collect::<Vec<_>>(),
        vec![
            "heycode-first-000".to_owned(),
            "heycode-second-11".to_owned()
        ]
    );
}

/// An environment that cancels the caller while the profile is being resolved.
struct CancellingEnvironment(CancellationToken);

impl GcpEnvironment for CancellingEnvironment {
    fn var(&self, _name: &str) -> Option<String> {
        self.0.cancel();
        None
    }

    fn read_file(&self, _path: &Path, _max_bytes: usize) -> Result<Vec<u8>, GcpFileError> {
        Err(GcpFileError::NotFound)
    }
}

#[tokio::test]
async fn cancellation_during_profile_resolution_is_cancelled_not_unavailable() {
    // PGCP01 reports a cancelled check as an undetermined subject. Passing
    // that straight through would tell CAT02 the provider was unavailable,
    // which is a different fact from "the caller went away".
    let cancellation = CancellationToken::new();
    let (_context, catalog, requests) = make_catalog(
        Arc::new(CancellingEnvironment(cancellation.clone())),
        vec![response(200, model_page())],
    );
    let error = catalog.fetch(cancellation).await.unwrap_err();
    assert_eq!(error.kind(), CatalogFailureKind::Cancelled);
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn only_the_oauth_mode_declares_a_dependency_on_the_gcp_auth_service() {
    let api_key = google_catalog_plugin(GoogleCatalogConfig::api_key(query()));
    assert!(!api_key.inject().contains(&SERVICE_GCP_AUTH));
    assert!(api_key.inject().contains(&SERVICE_CREDENTIALS));

    let oauth = google_catalog_plugin(GoogleCatalogConfig::oauth_quota_project(
        query(),
        offline_request(),
    ));
    assert!(oauth.inject().contains(&SERVICE_GCP_AUTH));
}

struct HttpPlugin(HttpService);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), self.0.clone())
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())?;
        let service = context
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .ok_or_else(|| CoreError::other("credentials service type mismatch"))?;
        service
            .register(
                context,
                Arc::new(SecretProvider {
                    id: CredentialProviderId::new("test-secret")
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    secret: Some(TEST_SECRET.to_owned()),
                }),
            )
            .map_err(|error| CoreError::other(error.to_string()))
    }
}

/// Mounts a deterministic PGCP01 service under the production key, so the
/// plugin is proven to resolve it by key rather than construct its own.
struct GcpPlugin(PathBuf);

impl Plugin for GcpPlugin {
    fn name(&self) -> &'static str {
        "test-gcp-auth"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_GCP_AUTH]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        let (probe_http, _) = http(Vec::new());
        let environment = MapGcpEnvironment::new()
            .with_var(ENV_GOOGLE_CLOUD_PROJECT, PROJECT)
            .with_var(ENV_HOME, &self.0.display().to_string());
        context.provide(
            SERVICE_GCP_AUTH,
            self.name(),
            GcpAuthService::new(Arc::new(environment), probe_http),
        )
    }
}

#[tokio::test]
async fn the_oauth_catalog_plugin_resolves_its_quota_project_through_the_composed_service() {
    let (catalog_http, requests) = http(vec![response(200, model_page())]);
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(catalog_http)),
        Box::new(CredentialsPlugin),
        Box::new(GcpPlugin(PathBuf::from("/home/heycode"))),
        google_catalog_plugin(GoogleCatalogConfig::oauth_quota_project(
            query(),
            offline_request(),
        )),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let view = models
        .refresh(
            "google",
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(view.snapshot.models.len(), 1);
    assert!(
        requests.lock().unwrap()[0]
            .1
            .contains(&("x-goog-user-project".to_owned(), PROJECT.to_owned()))
    );
    context.shutdown();
}
