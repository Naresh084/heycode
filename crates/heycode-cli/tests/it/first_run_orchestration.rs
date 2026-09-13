//! U10 production-loader authorization-to-recomposition contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization::{
    AuthorizationDescriptor, AuthorizationFlow, AuthorizationFlowFailure, AuthorizationFlowId,
    AuthorizationGrant, AuthorizationMethod, AuthorizationRequest,
};
use heycode_config::Config;
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference, CredentialSecret};
use heycode_onboarding::OnboardingService;
use tokio_util::sync::CancellationToken;

const CUSTOM_REFERENCE: &str = "HEYCODE_U10_CUSTOM_DEEPSEEK_KEY_8F4C";

struct ValidatedTestFlow {
    descriptor: AuthorizationDescriptor,
}

#[async_trait]
impl AuthorizationFlow for ValidatedTestFlow {
    fn descriptor(&self) -> AuthorizationDescriptor {
        self.descriptor.clone()
    }

    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationFlowFailure> {
        if request.cancellation.is_cancelled() {
            return Err(AuthorizationFlowFailure::new(
                "cancelled",
                "authorization was cancelled",
            ));
        }
        if request.query != self.descriptor.query {
            return Err(AuthorizationFlowFailure::new(
                "query",
                "authorization query changed",
            ));
        }
        Ok(AuthorizationGrant::validated(
            CredentialSecret::new("u10-test-secret-never-log"),
            42,
        ))
    }
}

fn trust(workspace: &std::path::Path) -> heycode_trust::WorkspaceTrustService {
    heycode_trust::WorkspaceTrustService::memory(workspace, heycode_cli::project_content_policy())
        .unwrap()
}

#[tokio::test]
async fn validated_custom_reference_recomposes_into_a_ready_connected_world() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let credentials_root = root.path().join("credentials-home");
    let profile_layers = [];
    let mut config = Config::defaults();
    config.llm.provider = "deepseek".to_owned();
    config.llm.model = heycode_llm::DeepSeekProvider::DEFAULT_MODEL.to_owned();
    config.llm.api_key_env = Some(CUSTOM_REFERENCE.to_owned());

    let mut first = heycode_cli::compose_world(&heycode_cli::WorldOptions {
        config: &config,
        trust: trust(&workspace),
        config_migration: None,
        profile_layers: &profile_layers,
        sessions_dir: root.path().join("sessions-first"),
        attachments_dir: root.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Interactive,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: root.path().join("settings.toml"),
        credentials_root: credentials_root.clone(),
        catalog_cache_path: root.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: true,
        credential_validated_at_ms: None,
        cwd: workspace.clone(),
        fake: None,
        resume: None,
    })
    .unwrap();
    let onboarding = first
        .get::<OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap();
    assert!(onboarding.snapshot().unwrap().active);
    let authorization = first
        .get::<heycode_authorization::AuthorizationService>(
            heycode_authorization::SERVICE_AUTHORIZATION,
        )
        .unwrap();
    let selected_flow = authorization
        .descriptors()
        .unwrap()
        .into_iter()
        .find(|descriptor| descriptor.id.as_str() == "deepseek-api-key")
        .unwrap();
    assert_eq!(selected_flow.query.reference.as_str(), CUSTOM_REFERENCE);
    let routing = first
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "deepseek")
        .unwrap();
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(CUSTOM_REFERENCE),
        "connection setup must find the registered validator for a custom reference"
    );

    let test_flow_id = AuthorizationFlowId::new("u10-validated-test").unwrap();
    let query = CredentialQuery::new(
        CredentialReference::new(CUSTOM_REFERENCE).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    );
    authorization
        .register(
            &first,
            Arc::new(ValidatedTestFlow {
                descriptor: AuthorizationDescriptor {
                    id: test_flow_id.clone(),
                    label: "U10 validated test flow".to_owned(),
                    method: AuthorizationMethod::ApiKey,
                    interactive: true,
                    query: query.clone(),
                },
            }),
        )
        .unwrap();
    let receipt = authorization
        .authorize(&test_flow_id, query, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(receipt.committed_by.as_str(), "file");
    assert!(receipt.credential.configured);
    first.shutdown();

    assert!(
        heycode_cli::provider_key_present_at(&config, &credentials_root).unwrap(),
        "startup must inspect the configured custom reference"
    );

    let mut second = heycode_cli::compose_world(&heycode_cli::WorldOptions {
        config: &config,
        trust: trust(&workspace),
        config_migration: None,
        profile_layers: &profile_layers,
        sessions_dir: root.path().join("sessions-second"),
        attachments_dir: root.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Interactive,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: root.path().join("settings.toml"),
        credentials_root,
        catalog_cache_path: root.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: Some(42),
        cwd: workspace,
        fake: None,
        resume: None,
    })
    .unwrap();
    let onboarding = second
        .get::<OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap();
    assert!(!onboarding.snapshot().unwrap().active);
    let routing = second
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    let profile = routing
        .connection_profiles()
        .iter()
        .find(|profile| profile.registry_name == "deepseek")
        .unwrap();
    assert_eq!(
        profile.credential_reference.as_deref(),
        Some(CUSTOM_REFERENCE),
        "connected provider metadata must retain the validator's reference after recomposition"
    );
    let selection = second
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap()
        .selection();
    assert_eq!(selection.provider_name, "deepseek");
    assert_eq!(
        selection.model,
        heycode_llm::DeepSeekProvider::DEFAULT_MODEL
    );
    let provider_default = second
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap()
        .profiles()
        .into_iter()
        .find(|profile| profile.registry_name == selection.provider_name)
        .unwrap()
        .default_model;
    assert_eq!(selection.model, provider_default);
    let providers = second
        .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
        .unwrap();
    assert_eq!(
        providers
            .get("deepseek")
            .unwrap()
            .inference_adapter()
            .unwrap()
            .authentication_binding(),
        heycode_llm::AuthenticationBinding::Credential(
            heycode_llm::CredentialHandle::new(CUSTOM_REFERENCE).unwrap()
        )
    );
    let sandbox = second
        .get::<heycode_exec::SandboxService>(heycode_exec::SERVICE_SANDBOX)
        .unwrap()
        .capability_report();
    assert!(sandbox.choice(sandbox.effective_mode).unwrap().selectable);
    second.shutdown();
}

#[test]
fn saved_connection_needing_credentials_starts_targeted_repair_in_real_composition() {
    let harness = heycode_cli::testing::RealCompositionHarness::new()
        .unwrap()
        .without_fake_provider()
        .with_onboarding_required();
    std::fs::write(
        harness.settings_path(),
        r#"schema_version = 1
[settings.routing]
runtime = "native"
provider = "openrouter"
model = "z-ai/glm-5.3-flash"
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            harness.settings_path(),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let world = harness.compose().unwrap();
    let onboarding = world
        .context()
        .get::<OnboardingService>(heycode_onboarding::SERVICE_ONBOARDING)
        .unwrap();
    let view = onboarding.snapshot().unwrap();
    assert_eq!(view.step, heycode_onboarding::OnboardingStep::Reconnect);
    assert_eq!(view.options[0].id, "openrouter");
    let routing = world
        .context()
        .get::<heycode_routing::RoutingService>(heycode_routing::SERVICE_ROUTING)
        .unwrap();
    assert!(
        routing
            .connection_profiles()
            .iter()
            .find(|row| row.registry_name == "openrouter")
            .unwrap()
            .credential_reference
            .is_some(),
        "a disconnected placeholder cannot erase provider-owned authentication metadata"
    );
}
