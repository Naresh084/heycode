//! WEB04 provider selection, domain policy, Settings and visibility contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_core::{Context, CoreResult, Plugin};
use heycode_settings::{SettingsDocuments, SettingsNamespace, SettingsService, SettingsWriter};
use heycode_web::{
    SERVICE_WEB, WebError, WebErrorClass, WebFetchRequest, WebFetchResult, WebProvider,
    WebProviderDescriptor, WebProviderSelection, WebRegistry, WebSearchRequest, WebSearchResult,
    web_policy_namespace, web_policy_plugin, web_registry_plugin,
};
use tokio_util::sync::CancellationToken;

struct ProbeProvider {
    descriptor: WebProviderDescriptor,
    available: AtomicBool,
    search_calls: AtomicUsize,
    fetch_calls: AtomicUsize,
    search_results: Vec<WebSearchResult>,
    fetch_final_url: Mutex<Option<String>>,
}

impl ProbeProvider {
    fn new(id: &str, search: bool, fetch: bool, search_results: Vec<WebSearchResult>) -> Self {
        Self {
            descriptor: WebProviderDescriptor::new(id, search, fetch).unwrap(),
            available: AtomicBool::new(true),
            search_calls: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
            search_results,
            fetch_final_url: Mutex::new(None),
        }
    }
}

#[async_trait]
impl WebProvider for ProbeProvider {
    fn descriptor(&self) -> WebProviderDescriptor {
        self.descriptor.clone()
    }

    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    async fn search(
        &self,
        _request: WebSearchRequest,
        _cancellation: CancellationToken,
    ) -> Result<Vec<WebSearchResult>, WebError> {
        self.search_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.search_results.clone())
    }

    async fn fetch(
        &self,
        request: WebFetchRequest,
        _cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebError> {
        assert!(request.domain_policy().allows(request.url()));
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        let final_url = self
            .fetch_final_url
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| request.url().to_owned());
        WebFetchResult::new(final_url, "fetched", Some("text/plain"), false)
    }
}

fn provider_plugin(plugin_name: &'static str, provider: Arc<ProbeProvider>) -> Box<dyn Plugin> {
    struct ProbePlugin {
        name: &'static str,
        provider: Arc<ProbeProvider>,
    }

    impl Plugin for ProbePlugin {
        fn name(&self) -> &'static str {
            self.name
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::unclassified(self.name())
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_WEB]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let registry = context
                .get::<WebRegistry>(SERVICE_WEB)
                .ok_or_else(|| heycode_core::CoreError::other("web missing"))?;
            registry
                .register(context, self.provider.clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(ProbePlugin {
        name: plugin_name,
        provider,
    })
}

fn result(url: &str) -> WebSearchResult {
    WebSearchResult::new("result", url, "snippet").unwrap()
}

#[tokio::test]
async fn configured_selection_and_domain_policy_are_visible_and_enforced() {
    let alpha = Arc::new(ProbeProvider::new("alpha", true, true, Vec::new()));
    let bravo = Arc::new(ProbeProvider::new(
        "bravo",
        true,
        false,
        vec![
            result("https://docs.example.com/allowed"),
            result("https://api.docs.example.com/also-allowed"),
            result("https://private.docs.example.com/blocked"),
            result("https://unrelated.test/blocked"),
        ],
    ));
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            web_policy_namespace().unwrap(),
            serde_json::json!({
                "search_provider":"bravo",
                "fetch_provider":"alpha",
                "search_domains":{
                    "allow":["docs.example.com"],
                    "block":["private.docs.example.com"]
                },
                "fetch_domains":{
                    "allow":["example.com"],
                    "block":["private.example.com"]
                }
            }),
        )
        .unwrap();
    let plugins = vec![
        heycode_settings::settings_plugin(documents),
        web_registry_plugin(),
        provider_plugin("provider-alpha", alpha.clone()),
        provider_plugin("provider-bravo", bravo.clone()),
        web_policy_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let report = registry.capability_report().unwrap();
    assert_eq!(
        report.search,
        WebProviderSelection::Selected {
            provider: "bravo".to_owned(),
            configured: true,
        }
    );
    assert_eq!(
        report.fetch,
        WebProviderSelection::Selected {
            provider: "alpha".to_owned(),
            configured: true,
        }
    );
    assert_eq!(report.providers.len(), 2);
    assert_eq!(report.search_domains.allow(), ["docs.example.com"]);
    assert_eq!(report.fetch_domains.block(), ["private.example.com"]);

    let results = registry
        .search(
            WebSearchRequest::new("policy", 8).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        results.iter().map(WebSearchResult::url).collect::<Vec<_>>(),
        [
            "https://docs.example.com/allowed",
            "https://api.docs.example.com/also-allowed",
        ]
    );
    assert_eq!(alpha.search_calls.load(Ordering::SeqCst), 0);
    assert_eq!(bravo.search_calls.load(Ordering::SeqCst), 1);

    let denied = registry
        .fetch(
            WebFetchRequest::new("https://private.example.com/secret", 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.class(), WebErrorClass::PolicyDenied);
    assert_eq!(alpha.fetch_calls.load(Ordering::SeqCst), 0);
    registry
        .fetch(
            WebFetchRequest::new("https://api.example.com/public", 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(alpha.fetch_calls.load(Ordering::SeqCst), 1);
    *alpha.fetch_final_url.lock().unwrap() =
        Some("https://private.example.com/provider-redirect".to_owned());
    let denied_final = registry
        .fetch(
            WebFetchRequest::new("https://api.example.com/start", 1024).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(denied_final.class(), WebErrorClass::PolicyDenied);
    assert_eq!(alpha.fetch_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn domain_rules_are_canonical_bounded_and_suffix_safe() {
    let policy = heycode_web::WebDomainPolicy::new(
        vec!["Docs.Example.COM".to_owned()],
        vec!["private.docs.example.com".to_owned()],
    )
    .unwrap();
    assert_eq!(policy.allow(), ["docs.example.com"]);
    assert!(policy.allows("https://docs.example.com/page"));
    assert!(policy.allows("https://api.docs.example.com/page"));
    assert!(policy.allows("https://api.docs.example.com./fqdn"));
    assert!(!policy.allows("https://evil-docs.example.com/page"));
    assert!(!policy.allows("https://private.docs.example.com/page"));
    assert!(!policy.allows("file://docs.example.com/page"));
    assert!(
        heycode_web::WebDomainPolicy::new(
            vec!["docs.example.com".to_owned(), "DOCS.EXAMPLE.COM".to_owned()],
            Vec::new(),
        )
        .is_err()
    );
    assert!(
        heycode_web::WebDomainPolicy::new(
            vec!["docs.example.com".to_owned()],
            vec!["docs.example.com".to_owned()],
        )
        .is_err()
    );
}

#[tokio::test]
async fn automatic_selection_refuses_ambiguity_and_invalid_persisted_ids_fail_composition() {
    let alpha = Arc::new(ProbeProvider::new("alpha", true, true, Vec::new()));
    let bravo = Arc::new(ProbeProvider::new("bravo", true, false, Vec::new()));
    let plugins = vec![
        heycode_settings::settings_plugin(SettingsDocuments::new()),
        web_registry_plugin(),
        provider_plugin("provider-alpha", alpha.clone()),
        provider_plugin("provider-bravo", bravo.clone()),
        web_policy_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    assert_eq!(
        registry.capability_report().unwrap().search,
        WebProviderSelection::Ambiguous {
            providers: vec!["alpha".to_owned(), "bravo".to_owned()]
        }
    );
    bravo.available.store(false, Ordering::SeqCst);
    assert_eq!(
        registry.capability_report().unwrap().search,
        WebProviderSelection::Selected {
            provider: "alpha".to_owned(),
            configured: false,
        }
    );
    bravo.available.store(true, Ordering::SeqCst);
    assert_eq!(
        registry
            .search(
                WebSearchRequest::new("ambiguous", 1).unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .class(),
        WebErrorClass::Ambiguous
    );
    assert_eq!(alpha.search_calls.load(Ordering::SeqCst), 0);
    assert_eq!(bravo.search_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        registry.capability_report().unwrap().fetch,
        WebProviderSelection::Selected {
            provider: "alpha".to_owned(),
            configured: false,
        }
    );

    let mut invalid = SettingsDocuments::new();
    invalid
        .set_user(
            web_policy_namespace().unwrap(),
            serde_json::json!({"search_provider":"missing"}),
        )
        .unwrap();
    let invalid_plugins = vec![
        heycode_settings::settings_plugin(invalid),
        web_registry_plugin(),
        provider_plugin("provider-alpha", alpha),
        web_policy_plugin(),
    ];
    assert!(heycode_core::compose(&invalid_plugins).is_err());
}

struct MemoryWriter;

impl SettingsWriter for MemoryWriter {
    fn persist_user(
        &self,
        _namespace: &SettingsNamespace,
        _section: &serde_json::Value,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn writable_settings_plugin(service: SettingsService) -> Box<dyn Plugin> {
    struct WritableSettingsPlugin(SettingsService);

    impl Plugin for WritableSettingsPlugin {
        fn name(&self) -> &'static str {
            "settings-test-writable"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::unclassified(self.name())
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(
                heycode_settings::SERVICE_SETTINGS,
                self.name(),
                self.0.clone(),
            )
        }
    }

    Box::new(WritableSettingsPlugin(service))
}

#[test]
fn committed_settings_replace_selection_and_policy_live() {
    let service = SettingsService::with_writer(SettingsDocuments::new(), Arc::new(MemoryWriter));
    let alpha = Arc::new(ProbeProvider::new("alpha", true, true, Vec::new()));
    let bravo = Arc::new(ProbeProvider::new("bravo", true, false, Vec::new()));
    let plugins = vec![
        writable_settings_plugin(service.clone()),
        web_registry_plugin(),
        provider_plugin("provider-alpha", alpha),
        provider_plugin("provider-bravo", bravo),
        web_policy_plugin(),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    assert!(matches!(
        registry.capability_report().unwrap().search,
        WebProviderSelection::Ambiguous { .. }
    ));
    let snapshot = service
        .replace_user(
            &web_policy_namespace().unwrap(),
            serde_json::json!({
                "search_provider":"alpha",
                "fetch_provider":"alpha",
                "search_domains":{"allow":[],"block":["noise.test"]},
                "fetch_domains":{"allow":["example.com"],"block":[]}
            }),
            Some(0),
        )
        .unwrap();
    assert_eq!(snapshot.revision(), 1);
    let report = registry.capability_report().unwrap();
    assert_eq!(
        report.search,
        WebProviderSelection::Selected {
            provider: "alpha".to_owned(),
            configured: true,
        }
    );
    assert_eq!(report.search_domains.block(), ["noise.test"]);
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "web-policy"
                && row.kind == heycode_core::ContributionKind::SettingsNamespace
                && row.name == "web")
    );
}
