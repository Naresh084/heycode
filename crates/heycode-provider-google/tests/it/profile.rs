//! PGCP02 provider-owned Google profile.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::{Context, CoreError, Plugin, ProviderProtocol, compose};
use heycode_credentials::{CredentialProviderId, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{CatalogRegistry, SERVICE_MODELS};
use heycode_provider_google::{
    GOOGLE_API_KEY_REFERENCE, GOOGLE_GEMINI_3_7_FLASH, GoogleCatalogConfig, google_catalog_plugin,
    google_profile,
};

use super::support::{SecretProvider, TEST_SECRET, http, query};

#[test]
fn the_profile_declares_the_gemini_protocol_and_its_own_credential_reference() {
    let profile = google_profile();
    assert_eq!(profile.registry_name, "google");
    assert_eq!(profile.descriptor.id, "google");
    assert_eq!(profile.descriptor.display_name, "Google Gemini");
    // Protocol compatibility is descriptor evidence, not model capability
    // evidence, and Gemini speaks exactly one of heycode's protocol families.
    assert_eq!(
        profile.descriptor.protocols,
        vec![ProviderProtocol::GeminiGenerateContent]
    );
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(GOOGLE_API_KEY_REFERENCE)
    );
    assert_eq!(GOOGLE_API_KEY_REFERENCE, "GEMINI_API_KEY");
}

#[test]
fn the_provider_default_is_a_request_facing_id_the_catalog_could_publish() {
    // The default reaches a URL as `models/{model}`, so it must satisfy the
    // same identity rule the catalog applies to a discovered row — including
    // carrying no method separator.
    assert_eq!(profile_default(), GOOGLE_GEMINI_3_7_FLASH);
    assert!(!GOOGLE_GEMINI_3_7_FLASH.is_empty());
    assert!(!GOOGLE_GEMINI_3_7_FLASH.contains(':'));
    assert!(
        GOOGLE_GEMINI_3_7_FLASH
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    );
}

fn profile_default() -> String {
    google_profile().default_model
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

#[test]
fn the_profile_registry_name_is_the_provider_the_catalog_registers() {
    // A profile that named a different provider than its own catalog source
    // would make setup and routing describe a provider with no catalog.
    let (catalog_http, _) = http(Vec::new());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_llm::model_catalog_plugin(Duration::from_secs(300)),
        Box::new(HttpPlugin(catalog_http)),
        Box::new(CredentialsPlugin),
        google_catalog_plugin(GoogleCatalogConfig::api_key(query())),
    ];
    let mut context = compose(&plugins).unwrap();
    let models = context.get::<CatalogRegistry>(SERVICE_MODELS).unwrap();
    let profile = google_profile();
    assert!(
        models.descriptors().unwrap().contains(&profile.descriptor),
        "the registered catalog must advertise the profile's exact descriptor"
    );
    context.shutdown();
}
