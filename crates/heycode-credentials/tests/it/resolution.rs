//! P09 route-scoped per-operation resolution: rotation reaches the next
//! operation, and one route never resolves through another route's record.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialResolutionError, CredentialSecret,
    CredentialSource, CredentialValidation, CredentialsService,
};

/// One record per reference, exactly as every shipped backend behaves, plus a
/// log of every reference the registry actually asked about.
struct RecordingProvider {
    id: CredentialProviderId,
    records: Mutex<BTreeMap<String, String>>,
    inspected: Mutex<Vec<String>>,
    resolved: Mutex<Vec<String>>,
}

impl RecordingProvider {
    fn new(id: &str, records: &[(&str, &str)]) -> Arc<Self> {
        Arc::new(Self {
            id: CredentialProviderId::new(id).unwrap(),
            records: Mutex::new(
                records
                    .iter()
                    .map(|(reference, secret)| ((*reference).to_owned(), (*secret).to_owned()))
                    .collect(),
            ),
            inspected: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
        })
    }

    fn rotate(&self, reference: &str, secret: &str) {
        self.records
            .lock()
            .unwrap()
            .insert(reference.to_owned(), secret.to_owned());
    }

    fn references_seen(&self) -> Vec<String> {
        let mut seen = self.inspected.lock().unwrap().clone();
        seen.extend(self.resolved.lock().unwrap().iter().cloned());
        seen.sort();
        seen.dedup();
        seen
    }

    fn resolve_calls(&self) -> usize {
        self.resolved.lock().unwrap().len()
    }
}

impl CredentialProvider for RecordingProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        10
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        self.inspected
            .lock()
            .unwrap()
            .push(query.reference.as_str().to_owned());
        Ok(
            if self
                .records
                .lock()
                .unwrap()
                .contains_key(query.reference.as_str())
            {
                CredentialProviderState::configured(CredentialSource::Keychain, true)
            } else {
                CredentialProviderState::unconfigured(true)
            },
        )
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        self.resolved
            .lock()
            .unwrap()
            .push(query.reference.as_str().to_owned());
        Ok(self
            .records
            .lock()
            .unwrap()
            .get(query.reference.as_str())
            .map(CredentialSecret::new))
    }
}

struct FailingProvider {
    id: CredentialProviderId,
    calls: AtomicUsize,
}

impl CredentialProvider for FailingProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        0
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err("keychain is locked".to_owned())
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Err("keychain is locked".to_owned())
    }
}

fn query(reference: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(reference).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[test]
fn a_key_rotated_between_operations_is_resolved_by_the_next_operation() {
    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    let provider = RecordingProvider::new("keychain", &[("OPENAI_API_KEY", "sk-first")]);
    service.register(&context, provider.clone()).unwrap();

    assert_eq!(
        service
            .resolve_route(&query("OPENAI_API_KEY"))
            .unwrap()
            .expose(),
        "sk-first"
    );
    provider.rotate("OPENAI_API_KEY", "sk-rotated");
    assert_eq!(
        service
            .resolve_route(&query("OPENAI_API_KEY"))
            .unwrap()
            .expose(),
        "sk-rotated",
        "the registry must walk providers again rather than reuse a captured secret"
    );
    assert_eq!(
        provider.resolve_calls(),
        2,
        "each operation resolves once; a cached secret would show one call"
    );
    context.shutdown();
}

#[test]
fn a_route_resolution_that_observes_rotation_also_clears_the_stale_validation_record() {
    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    let provider = RecordingProvider::new("keychain", &[("OPENAI_API_KEY", "sk-first")]);
    service.register(&context, provider.clone()).unwrap();

    let first = service.resolve_route(&query("OPENAI_API_KEY")).unwrap();
    service
        .record_validation(
            &query("OPENAI_API_KEY"),
            provider.id(),
            CredentialValidation::Valid {
                checked_at_ms: 1_000,
            },
            &first,
            60_000,
        )
        .unwrap();
    assert!(matches!(
        service
            .describe(&query("OPENAI_API_KEY"))
            .unwrap()
            .validation,
        CredentialValidation::Valid { .. }
    ));

    provider.rotate("OPENAI_API_KEY", "sk-rotated");
    let _rotated = service.resolve_route(&query("OPENAI_API_KEY")).unwrap();
    assert_eq!(
        service
            .describe(&query("OPENAI_API_KEY"))
            .unwrap()
            .validation,
        CredentialValidation::Unknown,
        "a validation recorded against the previous key must not survive rotation"
    );
    context.shutdown();
}

#[test]
fn an_unconfigured_route_fails_instead_of_resolving_through_another_routes_record() {
    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    let provider = RecordingProvider::new("keychain", &[("ANTHROPIC_API_KEY", "sk-ant-neighbour")]);
    service.register(&context, provider.clone()).unwrap();

    let error = service.resolve_route(&query("OPENAI_API_KEY")).unwrap_err();
    assert_eq!(
        error,
        CredentialResolutionError::Missing {
            reference: "OPENAI_API_KEY".to_owned()
        }
    );
    assert_eq!(
        provider.references_seen(),
        vec!["OPENAI_API_KEY".to_owned()],
        "resolving one route must never consult a second reference"
    );

    let rendered = error.to_string();
    assert!(rendered.contains("OPENAI_API_KEY"), "{rendered}");
    assert!(
        !rendered.contains("ANTHROPIC_API_KEY"),
        "the failure must not disclose which other routes are configured: {rendered}"
    );
    assert!(!rendered.contains("sk-ant-neighbour"), "{rendered}");
    assert!(
        !format!("{error:?}").contains("sk-ant-neighbour"),
        "debug output must not carry a secret"
    );
    context.shutdown();
}

#[test]
fn a_failing_provider_reports_the_requested_route_without_naming_another() {
    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    service
        .register(
            &context,
            Arc::new(FailingProvider {
                id: CredentialProviderId::new("keychain").unwrap(),
                calls: AtomicUsize::new(0),
            }),
        )
        .unwrap();
    service
        .register(
            &context,
            RecordingProvider::new("file", &[("OPENAI_API_KEY", "sk-lower-precedence")]),
        )
        .unwrap();

    let error = service.resolve_route(&query("OPENAI_API_KEY")).unwrap_err();
    assert!(
        matches!(error, CredentialResolutionError::Unavailable { .. }),
        "a broken high-precedence provider must fail loud, not fall through: {error:?}"
    );
    assert_eq!(error.reference(), "OPENAI_API_KEY");
    let rendered = error.to_string();
    assert!(rendered.contains("OPENAI_API_KEY"), "{rendered}");
    assert!(!rendered.contains("sk-lower-precedence"), "{rendered}");
    context.shutdown();
}

#[test]
fn every_resolution_error_variant_names_only_the_requested_reference() {
    let variants = [
        CredentialResolutionError::Missing {
            reference: "OPENAI_API_KEY".to_owned(),
        },
        CredentialResolutionError::Unavailable {
            reference: "OPENAI_API_KEY".to_owned(),
            message: "credential provider `keychain` failed: store is locked".to_owned(),
        },
        CredentialResolutionError::RouteMismatch {
            reference: "OPENAI_API_KEY".to_owned(),
        },
    ];
    for variant in variants {
        assert_eq!(variant.reference(), "OPENAI_API_KEY");
        let rendered = variant.to_string();
        assert!(rendered.contains("OPENAI_API_KEY"), "{rendered}");
        assert!(
            !rendered.contains("ANTHROPIC_API_KEY"),
            "no variant may name a second route: {rendered}"
        );
    }
}

#[test]
fn provider_failure_text_is_dropped_at_the_registry_boundary() {
    const CANARY: &str = "qsec02-provider-secret-canary";

    struct LeakyProvider {
        id: CredentialProviderId,
    }

    impl CredentialProvider for LeakyProvider {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }

        fn precedence(&self) -> u16 {
            0
        }

        fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Err(CANARY.to_owned())
        }

        fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Err(CANARY.to_owned())
        }
    }

    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    service
        .register(
            &context,
            Arc::new(LeakyProvider {
                id: CredentialProviderId::new("leaky").unwrap(),
            }),
        )
        .unwrap();

    let error = service.resolve_route(&query("OPENAI_API_KEY")).unwrap_err();
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains(CANARY), "{rendered}");
    assert!(rendered.contains("leaky"), "{rendered}");
    context.shutdown();
}
