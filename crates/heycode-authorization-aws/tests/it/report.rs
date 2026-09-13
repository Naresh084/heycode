//! PAWS01 composed status report over both credential paths.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_aws::{
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR, AWS_REGION_VAR, AwsAuthService,
    AwsCredentialSource, AwsCredentialStatus, AwsProfileName, AwsProfileResolution, AwsRejection,
    AwsUndetermined, MapAwsHost,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    Credentials, TEST_BEDROCK_KEY, bedrock_query, credentials_empty, credentials_failing,
    credentials_with, error_reply, json_reply, scripted_http, urls,
};

const CONFIG: &str = "/home/dev/.aws/config";

fn service(
    host: MapAwsHost,
    credentials: &Credentials,
    replies: Vec<super::support::Reply>,
) -> (AwsAuthService, super::support::RecordedRequests) {
    let (http, requests) = scripted_http(replies);
    (
        AwsAuthService::new(
            http,
            credentials.service.clone(),
            Arc::new(host),
            bedrock_query(),
        ),
        requests,
    )
}

fn configured_host() -> MapAwsHost {
    MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_REGION_VAR, "us-west-2")
}

#[tokio::test]
async fn a_proven_api_key_and_an_unprovable_chain_are_reported_side_by_side() {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let host = configured_host().with_file(
        CONFIG,
        "[default]\nsso_session = corp\nsso_account_id = 123456789012\n",
    );
    let (service, requests) = service(
        host,
        &credentials,
        vec![json_reply(200, serde_json::json!({"modelSummaries": []}))],
    );
    let report = service.report(CancellationToken::new()).await;

    assert!(report.api_key.is_valid());
    assert_eq!(
        report.api_key.source(),
        Some(&AwsCredentialSource::BedrockApiKey {
            reference: bedrock_query().reference
        })
    );
    assert_eq!(
        report.chain,
        AwsCredentialStatus::Undetermined {
            source: Some(AwsCredentialSource::SsoSession {
                profile: AwsProfileName::default_profile()
            }),
            reason: AwsUndetermined::RequiresSignedRequest,
        },
        "a chain source that needs a signed request may not be reported as working"
    );
    assert_eq!(
        report.profile,
        AwsProfileResolution::Default {
            profile: AwsProfileName::default_profile()
        }
    );
    assert_eq!(
        urls(&requests),
        ["https://bedrock.us-west-2.amazonaws.com/foundation-models".to_owned()],
        "an unprovable chain source costs no request"
    );
}

#[tokio::test]
async fn an_absent_api_key_is_absent_rather_than_a_failed_check() {
    let credentials = credentials_empty();
    let (service, requests) = service(configured_host(), &credentials, Vec::new());
    let report = service.report(CancellationToken::new()).await;
    assert_eq!(report.api_key, AwsCredentialStatus::Absent);
    assert_eq!(report.api_key.source(), None);
    assert!(urls(&requests).is_empty());
}

#[tokio::test]
async fn a_credential_store_failure_is_undetermined_rather_than_absent() {
    let credentials = credentials_failing();
    let (service, requests) = service(configured_host(), &credentials, Vec::new());
    let report = service.report(CancellationToken::new()).await;
    assert_eq!(
        report.api_key,
        AwsCredentialStatus::Undetermined {
            source: Some(AwsCredentialSource::BedrockApiKey {
                reference: bedrock_query().reference
            }),
            reason: AwsUndetermined::CredentialStoreUnavailable,
        },
        "a store that could not be asked has not said the key is missing"
    );
    assert!(urls(&requests).is_empty());
}

#[tokio::test]
async fn a_configured_key_with_no_region_is_undetermined_and_costs_no_request() {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let host = MapAwsHost::new().with_home("/home/dev");
    let (service, requests) = service(host, &credentials, Vec::new());
    let report = service.report(CancellationToken::new()).await;
    assert_eq!(
        report.api_key,
        AwsCredentialStatus::Undetermined {
            source: Some(AwsCredentialSource::BedrockApiKey {
                reference: bedrock_query().reference
            }),
            reason: AwsUndetermined::RegionUnresolved,
        }
    );
    assert!(urls(&requests).is_empty());
}

#[tokio::test]
async fn a_rejected_key_and_a_proven_container_role_keep_their_own_verdicts() {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let host = configured_host().with_var(
        AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
        "/v2/credentials/task",
    );
    let (service, _) = service(
        host,
        &credentials,
        vec![
            error_reply(403),
            json_reply(
                200,
                serde_json::json!({
                    "AccessKeyId": "ASIAEXAMPLE",
                    "SecretAccessKey": "container-secret-must-not-leak"
                }),
            ),
        ],
    );
    let report = service.report(CancellationToken::new()).await;
    assert_eq!(
        report.api_key,
        AwsCredentialStatus::Rejected {
            source: AwsCredentialSource::BedrockApiKey {
                reference: bedrock_query().reference
            },
            reason: AwsRejection::Unauthorized,
        }
    );
    assert!(report.chain.is_valid());
    assert_eq!(
        report.chain.source(),
        Some(&AwsCredentialSource::ContainerRole {
            variable: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR
        })
    );
}

#[tokio::test]
async fn an_unreachable_bedrock_endpoint_leaves_the_key_undetermined() {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let (service, _) = service(configured_host(), &credentials, vec![error_reply(503)]);
    let report = service.report(CancellationToken::new()).await;
    assert_eq!(
        report.api_key,
        AwsCredentialStatus::Undetermined {
            source: Some(AwsCredentialSource::BedrockApiKey {
                reference: bedrock_query().reference
            }),
            reason: AwsUndetermined::ServiceUnreachable,
        },
        "an unreachable service has not rejected anything"
    );
}

#[tokio::test]
async fn chain_discovery_answers_without_touching_the_network() {
    let credentials = credentials_empty();
    let host =
        configured_host().with_file(CONFIG, "[default]\ncredential_process = /usr/bin/helper\n");
    let (service, requests) = service(host, &credentials, Vec::new());
    assert_eq!(
        service.chain_discovery(),
        heycode_authorization_aws::AwsChainDiscovery::Found(
            AwsCredentialSource::CredentialProcess {
                profile: AwsProfileName::default_profile()
            }
        )
    );
    assert_eq!(
        service.region().region().map(|region| region.as_str()),
        Some("us-west-2")
    );
    assert!(urls(&requests).is_empty());
}

#[tokio::test]
async fn saved_region_controls_both_validation_request_and_report_origin() {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let (http, requests) = scripted_http(vec![json_reply(
        200,
        serde_json::json!({"modelSummaries": []}),
    )]);
    let service = AwsAuthService::new_with_region(
        http,
        credentials.service.clone(),
        Arc::new(configured_host()),
        bedrock_query(),
        Some(heycode_authorization_aws::AwsRegion::new("eu-central-1").unwrap()),
    );
    let report = service.report(CancellationToken::new()).await;
    assert!(report.api_key.is_valid());
    assert_eq!(
        report.region,
        heycode_authorization_aws::AwsRegionResolution::Resolved {
            region: heycode_authorization_aws::AwsRegion::new("eu-central-1").unwrap(),
            origin: heycode_authorization_aws::AwsRegionOrigin::Connection
        }
    );
    assert_eq!(service.region(), report.region);
    assert_eq!(
        urls(&requests),
        ["https://bedrock.eu-central-1.amazonaws.com/foundation-models"]
    );
}
