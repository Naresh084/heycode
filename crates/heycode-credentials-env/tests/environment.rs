//! Environment inspection/resolution and shadow-safe write contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use heycode_core::compose;
use heycode_credentials::{
    CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialSource, CredentialsError,
    CredentialsService, SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_credentials_env::{EnvironmentCredentialProvider, environment_credentials_plugin};
use heycode_settings::{SettingsDocuments, settings_plugin};

fn query(reference: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(reference).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[test]
fn configured_nonempty_environment_value_is_read_only_and_resolves_explicitly() {
    let provider = EnvironmentCredentialProvider::from_map(BTreeMap::from([
        ("OPENROUTER_API_KEY".to_owned(), "sk-or-live".to_owned()),
        ("EMPTY_KEY".to_owned(), "   ".to_owned()),
    ]))
    .unwrap();
    let state = provider.inspect(&query("OPENROUTER_API_KEY")).unwrap();
    assert_eq!(
        state,
        CredentialProviderState::configured(CredentialSource::Environment, false)
    );
    assert_eq!(
        provider
            .resolve(&query("OPENROUTER_API_KEY"))
            .unwrap()
            .unwrap()
            .expose(),
        "sk-or-live"
    );
    assert_eq!(
        provider.inspect(&query("EMPTY_KEY")).unwrap(),
        CredentialProviderState::unconfigured(false)
    );
    assert!(provider.resolve(&query("EMPTY_KEY")).unwrap().is_none());
}

#[test]
fn configured_environment_record_blocks_writable_lower_provider() {
    #[derive(Default)]
    struct WritableFallback(std::sync::atomic::AtomicUsize);
    impl CredentialProvider for WritableFallback {
        fn id(&self) -> &CredentialProviderId {
            static ID: std::sync::OnceLock<CredentialProviderId> = std::sync::OnceLock::new();
            ID.get_or_init(|| CredentialProviderId::new("writable-fallback").unwrap())
        }
        fn precedence(&self) -> u16 {
            20
        }
        fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Ok(CredentialProviderState::unconfigured(true))
        }
        fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(None)
        }
        fn write(
            &self,
            _query: &CredentialQuery,
            _secret: &CredentialSecret,
        ) -> Result<(), String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        environment_credentials_plugin(
            EnvironmentCredentialProvider::from_map(BTreeMap::from([(
                "OPENROUTER_API_KEY".to_owned(),
                "env-secret".to_owned(),
            )]))
            .unwrap(),
        ),
    ];
    let mut context = compose(&plugins).unwrap();
    let service = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let fallback = Arc::new(WritableFallback::default());
    service.register(&context, fallback.clone()).unwrap();

    let error = service
        .write(
            &query("OPENROUTER_API_KEY"),
            &CredentialSecret::new("replacement"),
        )
        .unwrap_err();
    assert!(matches!(error, CredentialsError::ShadowedReadOnly { .. }));
    assert_eq!(
        fallback.0.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a lower provider must not receive a shadowed write"
    );
    context.shutdown();
}
