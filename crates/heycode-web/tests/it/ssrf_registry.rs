//! QSEC03 — where the SSRF guard actually lives, tested at the registry seam.
//!
//! The shared public-URL values reject obvious private literals and reserved
//! local names before a provider can publish or dispatch them. The registry
//! additionally enforces domain policy; DNS resolution and connection pinning
//! remain the portable provider's per-hop responsibility.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use heycode_core::{Context, CoreResult, Plugin};
use heycode_settings::SettingsDocuments;
use heycode_web::{
    SERVICE_WEB, WebError, WebFetchRequest, WebFetchResult, WebProvider, WebProviderDescriptor,
    WebRegistry, WebSearchRequest, WebSearchResult, web_policy_namespace, web_policy_plugin,
    web_registry_plugin,
};
use tokio_util::sync::CancellationToken;

/// A provider with no DNS awareness at all — the shape of any third-party
/// search or fetch plugin. It can receive only requests that passed the shared
/// obvious-host boundary first.
struct GullibleProvider {
    results: Vec<WebSearchResult>,
    fetch_calls: AtomicUsize,
}

#[async_trait]
impl WebProvider for GullibleProvider {
    fn descriptor(&self) -> WebProviderDescriptor {
        WebProviderDescriptor::new("gullible", true, true).unwrap()
    }

    async fn search(
        &self,
        _request: WebSearchRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        Ok(self.results.clone())
    }

    async fn fetch(
        &self,
        request: WebFetchRequest,
        _cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        WebFetchResult::new(request.url(), "body", Some("text/plain"), false)
    }
}

fn provider_plugin(provider: Arc<GullibleProvider>) -> Box<dyn Plugin> {
    struct ProviderPlugin(Arc<GullibleProvider>);
    impl Plugin for ProviderPlugin {
        fn name(&self) -> &'static str {
            "web-gullible"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WEB]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<WebRegistry>(SERVICE_WEB)
                .ok_or_else(|| heycode_core::CoreError::other("web service missing"))?;
            registry
                .register(context, self.0.clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }
    Box::new(ProviderPlugin(provider))
}

const OBVIOUS_PRIVATE_URLS: [&str; 5] = [
    "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
    "http://metadata.google.internal/computeMetadata/v1/",
    "http://127.0.0.1:8080/admin",
    "http://10.0.0.1/",
    "http://[::1]/",
];

fn world_with_results(
    policy: Option<serde_json::Value>,
    results: &[&str],
) -> (Context, Arc<GullibleProvider>) {
    let provider = Arc::new(GullibleProvider {
        results: results
            .iter()
            .map(|url| WebSearchResult::new("result", *url, "snippet").unwrap())
            .collect(),
        fetch_calls: AtomicUsize::new(0),
    });
    let mut documents = SettingsDocuments::new();
    if let Some(policy) = policy {
        documents
            .set_user(web_policy_namespace().unwrap(), policy)
            .unwrap();
    }
    let plugins = vec![
        heycode_settings::settings_plugin(documents),
        web_registry_plugin(),
        provider_plugin(provider.clone()),
        web_policy_plugin(),
    ];
    (heycode_core::compose(&plugins).unwrap(), provider)
}

fn world(policy: Option<serde_json::Value>) -> (Context, Arc<GullibleProvider>) {
    world_with_results(
        policy,
        &[
            "https://93.184.216.34/",
            "https://[2606:2800:220:1:248:1893:25c8:1946]/",
        ],
    )
}

/// A provider with no address or IDN awareness still inherits the registry's
/// immutable operation policy. Literal and homograph evasions are removed
/// before search output becomes model-visible, and a fetch is denied before
/// the provider receives it.
#[tokio::test]
async fn qsec03_requirement_block_policy_closes_evasions_at_the_registry_seam() {
    const RESULTS: [&str; 4] = [
        "https://blocked.test/",
        "https://93.184.216.34/",
        "https://bl\u{43e}cked.test/",
        "https://unrelated.test/",
    ];
    let policy = Some(serde_json::json!({
        "search_domains": {"allow": [], "block": ["blocked.test"]},
        "fetch_domains": {"allow": [], "block": ["blocked.test"]}
    }));
    let (context, provider) = world_with_results(policy, &RESULTS);
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();

    let results = registry
        .search(
            WebSearchRequest::new("policy", 8).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        results.iter().map(WebSearchResult::url).collect::<Vec<_>>(),
        ["https://unrelated.test/"]
    );

    for evasion in ["https://93.184.216.34/", "https://bl\u{43e}cked.test/"] {
        let error = registry
            .fetch(
                WebFetchRequest::new(evasion, 4_096).unwrap(),
                CancellationToken::new(),
            )
            .await
            .expect_err("the policy must reject before provider dispatch");
        assert_eq!(error.class(), heycode_web::WebErrorClass::PolicyDenied);
    }
    assert_eq!(
        provider.fetch_calls.load(Ordering::SeqCst),
        0,
        "no policy-denied spelling may reach a provider"
    );
}

/// Obvious local/metadata hosts cannot cross the shared public-URL constructor,
/// so a provider cannot place them into registry output or receive a fetch for
/// them even when no domain rules are configured.
#[test]
fn private_and_metadata_urls_are_rejected_before_registry_publication() {
    for url in OBVIOUS_PRIVATE_URLS {
        assert!(WebSearchResult::new("result", url, "snippet").is_err());
        assert!(WebFetchRequest::new(url, 4_096).is_err());
    }
}

/// The shared boundary still admits genuinely public literals. An explicit
/// named allow list may narrow them out at the registry without changing the
/// default behavior.
#[tokio::test]
async fn an_explicit_allow_list_removes_public_literal_search_results() {
    let (context, _provider) = world(Some(serde_json::json!({
        "search_domains": {"allow": ["docs.example.com"], "block": []},
        "fetch_domains": {"allow": ["docs.example.com"], "block": []}
    })));
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let results = registry
        .search(
            WebSearchRequest::new("credentials", 8).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        results.is_empty(),
        "an allow list must exclude every unlisted literal-address result: {:?}",
        results.iter().map(WebSearchResult::url).collect::<Vec<_>>()
    );
}

/// Default no-block policy retains ordinary public literal fetches. The shared
/// guard must not turn the SSRF fix into an all-literal denial.
#[tokio::test]
async fn the_registry_hands_a_public_literal_to_the_provider_under_the_default_policy() {
    let (context, provider) = world(None);
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let fetched = registry
        .fetch(
            WebFetchRequest::new("https://93.184.216.34/", 4_096).unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect("a public literal remains a valid provider request");
    assert_eq!(fetched.final_url(), "https://93.184.216.34/");
    assert_eq!(
        provider.fetch_calls.load(Ordering::SeqCst),
        1,
        "the default no-block policy must retain a public literal"
    );
}

/// With an allow list configured the same public-literal request never reaches
/// the provider, preserving the operator's narrower domain authority.
#[tokio::test]
async fn an_allow_list_stops_a_public_literal_fetch_before_the_provider_is_called() {
    let (context, provider) = world(Some(serde_json::json!({
        "search_domains": {"allow": [], "block": []},
        "fetch_domains": {"allow": ["docs.example.com"], "block": []}
    })));
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let error = registry
        .fetch(
            WebFetchRequest::new("https://93.184.216.34/", 4_096).unwrap(),
            CancellationToken::new(),
        )
        .await
        .expect_err("an allow list must refuse a literal address");
    assert_eq!(error.class(), heycode_web::WebErrorClass::PolicyDenied);
    assert_eq!(
        provider.fetch_calls.load(Ordering::SeqCst),
        0,
        "a denied request must not reach the provider at all"
    );
}
