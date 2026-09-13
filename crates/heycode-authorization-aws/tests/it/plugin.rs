//! PAWS01 plugin contributions and their unwind.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use heycode_authorization::{AuthorizationError, AuthorizationService, SERVICE_AUTHORIZATION};
use heycode_authorization_api_key::{SecretPrompt, SecretPromptRequest};
use heycode_authorization_aws::{
    AWS_BEDROCK_API_KEY_REFERENCE, AWS_BEDROCK_FLOW_ID, AwsAuthPluginConfig, AwsAuthService,
    MapAwsHost, SERVICE_AWS_AUTH, aws_authorization_plugin,
};
use heycode_core::{Context, CoreError, Plugin, ServiceKey, compose};
use heycode_credentials::{CredentialSecret, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{
    BufferedResponseFuture, HttpRequest, HttpService, HttpSseRequest, HttpTransport, SERVICE_HTTP,
    SseEventStream, TransportError,
};
use tokio_util::sync::CancellationToken;

use super::support::bedrock_query;

struct RefusingTransport;

impl HttpTransport for RefusingTransport {
    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        Box::pin(async {
            Err(TransportError::Network {
                message: "no network in tests".to_owned(),
            })
        })
    }

    fn sse(&self, _request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_CREDENTIALS]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

struct HttpPlugin;

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn provides(&self) -> &'static [ServiceKey] {
        &[SERVICE_HTTP]
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(
            SERVICE_HTTP,
            self.name(),
            HttpService::new(Arc::new(RefusingTransport)),
        )
    }
}

struct RefusingPrompt;

#[async_trait]
impl SecretPrompt for RefusingPrompt {
    async fn prompt(
        &self,
        request: SecretPromptRequest,
        _cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        assert!(request.masked, "an API key prompt must always mask");
        Err("no interactive prompt in tests".to_owned())
    }
}

fn world() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(CredentialsPlugin),
        Box::new(HttpPlugin),
        heycode_authorization::authorization_plugin(),
        aws_authorization_plugin(AwsAuthPluginConfig::new(
            bedrock_query(),
            Arc::new(RefusingPrompt),
            Arc::new(MapAwsHost::new().with_var("AWS_REGION", "us-east-1")),
        )),
    ]
}

#[test]
fn the_plugin_contributes_one_named_flow_and_publishes_the_status_service() {
    let plugins = world();
    let context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    let descriptors = authorization.descriptors().unwrap();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].id.as_str(), AWS_BEDROCK_FLOW_ID);
    assert_eq!(descriptors[0].query, bedrock_query());
    assert!(descriptors[0].interactive);

    assert!(context.get::<AwsAuthService>(SERVICE_AWS_AUTH).is_some());

    let inventory = context.plugin_inventory().snapshot().unwrap();
    let mine: Vec<_> = inventory
        .contributions
        .iter()
        .filter(|row| row.plugin == "authorization-aws")
        .map(|row| (row.kind, row.name.clone()))
        .collect();
    assert_eq!(
        mine,
        vec![
            (
                heycode_core::ContributionKind::AuthorizationFlow,
                AWS_BEDROCK_FLOW_ID.to_owned()
            ),
            (
                heycode_core::ContributionKind::Service,
                SERVICE_AWS_AUTH.as_str().to_owned()
            ),
        ]
    );
}

#[test]
fn the_credential_reference_matches_the_variable_bedrock_itself_reads() {
    assert_eq!(AWS_BEDROCK_API_KEY_REFERENCE, "AWS_BEARER_TOKEN_BEDROCK");
    assert_eq!(
        AWS_BEDROCK_API_KEY_REFERENCE,
        heycode_authorization_aws::AWS_BEARER_TOKEN_BEDROCK_VAR
    );
}

#[test]
fn shutdown_removes_the_flow_this_plugin_registered() {
    let plugins = world();
    let mut context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    assert_eq!(authorization.descriptors().unwrap().len(), 1);
    context.shutdown();
    assert!(
        authorization.descriptors().unwrap().is_empty(),
        "the flow lives on the plugin's effect, not the registry's lifetime"
    );
}

#[tokio::test]
async fn a_refused_prompt_fails_the_flow_before_any_credential_commit() {
    let plugins = world();
    let context = compose(&plugins).unwrap();
    let authorization = context
        .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
        .unwrap();
    let id = heycode_authorization::AuthorizationFlowId::new(AWS_BEDROCK_FLOW_ID).unwrap();
    let error = authorization
        .authorize(&id, bedrock_query(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthorizationError::Flow { ref code, .. } if code == "input"),
        "unexpected {error:?}"
    );
}
