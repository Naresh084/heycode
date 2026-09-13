//! Secret/redaction, provider precedence, lifetime, and settings contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::compose;
use heycode_credentials::{
    CredentialDescriptor, CredentialKind, CredentialProvider, CredentialProviderId,
    CredentialProviderState, CredentialQuery, CredentialReference, CredentialSecret,
    CredentialSource, CredentialValidation, CredentialsError, CredentialsService,
    SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_settings::{SERVICE_SETTINGS, SettingsDocuments, SettingsService, settings_plugin};

struct FakeProvider {
    id: CredentialProviderId,
    precedence: u16,
    state: CredentialProviderState,
    value: Option<&'static str>,
}

impl CredentialProvider for FakeProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        self.precedence
    }

    fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        Ok(self.state.clone())
    }

    fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        Ok(self.value.map(CredentialSecret::new))
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("OPENROUTER_API_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[test]
fn secret_requires_explicit_exposure_and_safe_descriptor_serializes_without_it() {
    let secret_text = "sk-or-secret-never-log";
    let secret = CredentialSecret::new(secret_text);
    assert!(!format!("{secret:?}").contains(secret_text));
    assert_eq!(secret.expose(), secret_text);

    let descriptor = CredentialDescriptor::unconfigured(query());
    let json = serde_json::to_string(&descriptor).unwrap();
    assert!(json.contains("OPENROUTER_API_KEY"));
    assert!(json.contains("configured"));
    assert!(!json.contains(secret_text));
    assert!(
        !json.contains("value"),
        "descriptor must have no value field: {json}"
    );
}

#[test]
fn highest_precedence_configured_provider_controls_descriptor_and_resolution() {
    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    service
        .register(
            &context,
            Arc::new(FakeProvider {
                id: CredentialProviderId::new("file").unwrap(),
                precedence: 20,
                state: CredentialProviderState::configured(CredentialSource::File, true),
                value: Some("file-secret"),
            }),
        )
        .unwrap();
    service
        .register(
            &context,
            Arc::new(FakeProvider {
                id: CredentialProviderId::new("environment").unwrap(),
                precedence: 0,
                state: CredentialProviderState::configured(CredentialSource::Environment, false),
                value: Some("environment-secret"),
            }),
        )
        .unwrap();

    let descriptor = service.describe(&query()).unwrap();
    assert!(descriptor.configured);
    assert_eq!(descriptor.source, Some(CredentialSource::Environment));
    assert_eq!(descriptor.provider.unwrap().as_str(), "environment");
    assert!(!descriptor.writable, "shadowing env provider is read-only");
    assert_eq!(
        service.resolve(&query()).unwrap().unwrap().expose(),
        "environment-secret"
    );

    let duplicate = service
        .register(
            &context,
            Arc::new(FakeProvider {
                id: CredentialProviderId::new("environment").unwrap(),
                precedence: 99,
                state: CredentialProviderState::unconfigured(false),
                value: None,
            }),
        )
        .unwrap_err();
    assert!(matches!(
        duplicate,
        CredentialsError::DuplicateProvider { .. }
    ));

    context.shutdown();
    assert!(!service.describe(&query()).unwrap().configured);
}

#[test]
fn credentials_plugin_mounts_service_and_non_secret_settings_namespace() {
    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let credentials = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    assert!(!credentials.describe(&query()).unwrap().configured);

    let settings = context.get::<SettingsService>(SERVICE_SETTINGS).unwrap();
    let namespace = heycode_credentials::settings_namespace().unwrap();
    let snapshot = settings.get(&namespace).unwrap().unwrap();
    assert_eq!(snapshot.resolved(), &serde_json::json!({"references": {}}));
    let rendered = serde_json::to_string(snapshot.resolved()).unwrap();
    assert!(!rendered.to_ascii_lowercase().contains("secret"));

    let descriptor = context
        .plugin_descriptors()
        .iter()
        .find(|descriptor| descriptor.id == "credentials")
        .unwrap();
    assert_eq!(
        descriptor.contributions,
        &[heycode_core::PluginContributionKind::Service]
    );
    context.shutdown();
}

#[test]
fn invalid_persisted_reference_rejects_plugin_composition() {
    let namespace = heycode_credentials::settings_namespace().unwrap();
    let mut documents = SettingsDocuments::new();
    documents
        .set_user(
            namespace,
            serde_json::json!({"references": {"openrouter": "contains spaces"}}),
        )
        .unwrap();
    let error = match compose(&[settings_plugin(documents), credentials_plugin()]) {
        Ok(_) => panic!("invalid credential reference must fail composition"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("invalid credential reference"), "{error}");
}

#[test]
fn validation_cache_becomes_stale_and_rotation_clears_it_on_next_resolve() {
    struct RotatingProvider {
        id: CredentialProviderId,
        value: std::sync::Mutex<String>,
    }
    impl CredentialProvider for RotatingProvider {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }
        fn precedence(&self) -> u16 {
            10
        }
        fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Ok(CredentialProviderState::configured(
                CredentialSource::Keychain,
                true,
            ))
        }
        fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(Some(CredentialSecret::new(
                self.value.lock().unwrap().clone(),
            )))
        }
    }

    let service = CredentialsService::new();
    let mut context = heycode_core::Context::new();
    let provider = Arc::new(RotatingProvider {
        id: CredentialProviderId::new("rotating").unwrap(),
        value: std::sync::Mutex::new("first".to_owned()),
    });
    service.register(&context, provider.clone()).unwrap();
    let first = service.resolve(&query()).unwrap().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    service
        .record_validation(
            &query(),
            provider.id(),
            CredentialValidation::Valid { checked_at_ms: now },
            &first,
            60_000,
        )
        .unwrap();
    assert!(matches!(
        service.describe(&query()).unwrap().validation,
        CredentialValidation::Valid { .. }
    ));

    *provider.value.lock().unwrap() = "rotated".to_owned();
    let _ = service.resolve(&query()).unwrap();
    assert_eq!(
        service.describe(&query()).unwrap().validation,
        CredentialValidation::Unknown
    );

    let rotated = service.resolve(&query()).unwrap().unwrap();
    service
        .record_validation(
            &query(),
            provider.id(),
            CredentialValidation::Valid { checked_at_ms: now },
            &rotated,
            0,
        )
        .unwrap();
    assert!(matches!(
        service.describe(&query()).unwrap().validation,
        CredentialValidation::Stale { .. }
    ));
    context.shutdown();
}
