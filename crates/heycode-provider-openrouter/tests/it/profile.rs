//! POR01 provider-owned authorization/profile contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization::{AuthorizationError, AuthorizationService, SERVICE_AUTHORIZATION};
use heycode_authorization_api_key::{
    ApiKeyValidationFailure, ApiKeyValidator, SecretPrompt, SecretPromptRequest,
};
use heycode_core::{Context, CoreError, Plugin, compose};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialsService,
    SERVICE_CREDENTIALS,
};
use heycode_provider_openrouter::{
    OPENROUTER_FLOW_ID, OPENROUTER_WEB_SEARCH_IMPLEMENTATION, OpenRouterPluginConfig,
    openrouter_native_tools_plugin, openrouter_plugin, openrouter_profile,
};
use tokio_util::sync::CancellationToken;

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct StaticPrompt;

#[async_trait]
impl SecretPrompt for StaticPrompt {
    async fn prompt(
        &self,
        request: SecretPromptRequest,
        _cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        assert!(request.masked);
        Ok(CredentialSecret::new("invalid-openrouter-test-secret"))
    }
}

struct RejectingValidator;

#[async_trait]
impl ApiKeyValidator for RejectingValidator {
    async fn validate(
        &self,
        _secret: &CredentialSecret,
        _cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        Err(ApiKeyValidationFailure::Unauthorized)
    }
}

fn query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new("CUSTOM_OPENROUTER_KEY").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[test]
fn profile_owns_current_identity_default_and_credential_reference() {
    let profile = openrouter_profile();
    assert_eq!(profile.registry_name, "openrouter");
    assert_eq!(profile.descriptor.id, "openrouter");
    assert_eq!(profile.descriptor.display_name, "OpenRouter");
    assert_eq!(profile.default_model, "z-ai/glm-5.3-flash");
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some("OPENROUTER_API_KEY")
    );
}

#[test]
fn provider_native_web_candidate_is_effect_owned_and_route_specific() {
    let plugins = vec![
        heycode_native_tools::native_tools_plugin(),
        openrouter_native_tools_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let registry = context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    let routes = registry.resolve("openrouter").unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].logical(), "web_search");
    assert_eq!(
        routes[0].implementation(),
        OPENROUTER_WEB_SEARCH_IMPLEMENTATION
    );
    assert_eq!(routes[0].provider(), Some("openrouter"));
    assert!(registry.resolve("deepseek").unwrap().is_empty());
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "native-openrouter"
                && row.kind == heycode_core::ContributionKind::NativeTool
                && row.name == OPENROUTER_WEB_SEARCH_IMPLEMENTATION)
    );
    context.shutdown();
    assert!(registry.resolve("openrouter").unwrap().is_empty());
}

#[tokio::test]
async fn plugin_owns_flow_rejects_invalid_key_before_commit_and_disposes() {
    let config = OpenRouterPluginConfig::new(
        query(),
        Arc::new(StaticPrompt),
        Arc::new(RejectingValidator),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(CredentialsPlugin),
        heycode_authorization::authorization_plugin(),
        openrouter_plugin(config),
    ];
    let mut context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    let descriptors = authorization.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id.as_str(), OPENROUTER_FLOW_ID);
    assert_eq!(descriptors[0].query, query());

    let error = authorization
        .authorize(
            &descriptors[0].id,
            descriptors[0].query.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AuthorizationError::Flow { ref code, .. } if code == "unauthorized"
    ));

    let inventory = context.plugin_inventory().snapshot().unwrap();
    assert!(inventory.contributions.iter().any(|row| {
        row.plugin == "provider-openrouter"
            && row.kind == heycode_core::ContributionKind::AuthorizationFlow
            && row.name == OPENROUTER_FLOW_ID
    }));

    context.shutdown();
    assert!(authorization.descriptors().unwrap().is_empty());
}
