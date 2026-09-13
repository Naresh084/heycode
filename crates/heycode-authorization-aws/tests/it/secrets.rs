//! PAWS01 "without secrets": nothing a report can reach carries credential
//! material, an account id, a role ARN or an Identity Center start URL.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_authorization_aws::{
    AWS_ACCESS_KEY_ID_VAR, AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR,
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR, AWS_REGION_VAR, AWS_SECRET_ACCESS_KEY_VAR,
    AwsAuthReport, AwsAuthService, MapAwsHost,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    Reply, TEST_BEDROCK_KEY, bedrock_query, credentials_with, error_reply, json_reply,
    scripted_http,
};

const CONFIG: &str = "/home/dev/.aws/config";
const CREDENTIALS: &str = "/home/dev/.aws/credentials";

/// Every string a report must never contain.
const FORBIDDEN: [&str; 8] = [
    TEST_BEDROCK_KEY,
    "file-secret-must-not-leak",
    "env-secret-must-not-leak",
    "container-secret-must-not-leak",
    "container-token-must-not-leak",
    "provider-body-must-not-leak",
    "123456789012",
    "acme.awsapps.com",
];

fn assert_no_secret(report: &AwsAuthReport) {
    let debug = format!("{report:?}");
    let json = serde_json::to_string(report).unwrap();
    for needle in FORBIDDEN {
        assert!(!debug.contains(needle), "Debug leaked {needle}:\n{debug}");
        assert!(!json.contains(needle), "JSON leaked {needle}:\n{json}");
    }
}

async fn report(host: MapAwsHost, replies: Vec<Reply>) -> AwsAuthReport {
    let credentials = credentials_with(TEST_BEDROCK_KEY);
    let (http, _) = scripted_http(replies);
    AwsAuthService::new(
        http,
        credentials.service.clone(),
        Arc::new(host),
        bedrock_query(),
    )
    .report(CancellationToken::new())
    .await
}

#[tokio::test]
async fn a_report_over_secret_bearing_shared_files_publishes_none_of_their_values() {
    let host = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_REGION_VAR, "us-east-1")
        .with_file(
            CREDENTIALS,
            "[default]\naws_access_key_id = AKIAEXAMPLE\naws_secret_access_key = file-secret-must-not-leak\n",
        )
        .with_file(
            CONFIG,
            "[default]\nregion = us-east-1\nrole_arn = arn:aws:iam::123456789012:role/admin\nsso_start_url = https://acme.awsapps.com/start\n",
        );
    let report = report(host, vec![error_reply(401)]).await;
    assert!(report.chain.source().is_some(), "provenance is still named");
    assert_no_secret(&report);
}

#[tokio::test]
async fn a_report_over_secret_bearing_environment_variables_publishes_none_of_their_values() {
    let host = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_REGION_VAR, "us-east-1")
        .with_var(AWS_ACCESS_KEY_ID_VAR, "AKIAEXAMPLE")
        .with_var(AWS_SECRET_ACCESS_KEY_VAR, "env-secret-must-not-leak");
    let report = report(host, vec![json_reply(200, serde_json::json!({}))]).await;
    assert_no_secret(&report);
}

#[tokio::test]
async fn the_container_credential_response_never_reaches_the_status_it_proves() {
    let host = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_REGION_VAR, "us-east-1")
        .with_var(
            AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
            "/v2/credentials/task",
        )
        .with_var(
            AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR,
            "container-token-must-not-leak",
        );
    let report = report(
        host,
        vec![
            json_reply(200, serde_json::json!({"modelSummaries": []})),
            json_reply(
                200,
                serde_json::json!({
                    "AccessKeyId": "ASIAEXAMPLE",
                    "SecretAccessKey": "container-secret-must-not-leak",
                    "Token": "container-secret-must-not-leak",
                    "Expiration": "2030-01-01T00:00:00Z",
                    "RoleArn": "arn:aws:iam::123456789012:role/task"
                }),
            ),
        ],
    )
    .await;
    assert!(
        report.chain.is_valid(),
        "the response did prove the container role works"
    );
    assert_no_secret(&report);
}

#[tokio::test]
async fn provenance_names_the_location_and_the_status_json_has_no_value_field() {
    let host = MapAwsHost::new()
        .with_home("/home/dev")
        .with_var(AWS_REGION_VAR, "eu-west-1")
        .with_var(AWS_ACCESS_KEY_ID_VAR, "AKIAEXAMPLE")
        .with_var(AWS_SECRET_ACCESS_KEY_VAR, "env-secret-must-not-leak");
    let report = report(host, vec![json_reply(200, serde_json::json!({}))]).await;
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        json.contains(AWS_ACCESS_KEY_ID_VAR),
        "the variable name is the safe half of provenance: {json}"
    );
    assert!(
        json.contains("AWS_BEARER_TOKEN_BEDROCK"),
        "the credential reference is a name, not a value: {json}"
    );
    assert_no_secret(&report);
}
