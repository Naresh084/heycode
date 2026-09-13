//! PAWS01 live checks: Bedrock API key and the container credential endpoint.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use heycode_authorization_api_key::{ApiKeyValidationFailure, ApiKeyValidator};
use heycode_authorization_aws::{
    AWS_BEARER_TOKEN_BEDROCK_VAR, AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR,
    AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR, AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR, AwsBedrockApiKeyValidator, AwsCredentialSource,
    AwsCredentialStatus, AwsProfileResolution, AwsRegion, AwsRegionResolution, AwsRejection,
    AwsUndetermined, MapAwsHost, ProcessAwsHost, container_role_status,
};
use heycode_credentials::CredentialSecret;
use heycode_http::{HttpResponse, HttpService, TransportError};
use tokio_util::sync::CancellationToken;

use super::support::{Reply, TEST_BEDROCK_KEY, error_reply, json_reply, scripted_http, urls};

const CHECKED_AT: u64 = 1_700_000_000_000;

fn region() -> AwsRegion {
    AwsRegion::new("eu-central-1").unwrap()
}

fn credential_body() -> serde_json::Value {
    serde_json::json!({
        "AccessKeyId": "ASIAEXAMPLE",
        "SecretAccessKey": "container-secret-must-not-leak",
        "Token": "session-token-must-not-leak",
        "Expiration": "2030-01-01T00:00:00Z",
        "RoleArn": "arn:aws:iam::123456789012:role/task"
    })
}

#[tokio::test]
async fn bedrock_validation_sends_one_bearer_request_to_the_regional_control_plane() {
    let (http, requests) = scripted_http(vec![json_reply(
        200,
        serde_json::json!({"modelSummaries": []}),
    )]);
    AwsBedrockApiKeyValidator::new(http, Some(&region()))
        .validate(
            &CredentialSecret::new(TEST_BEDROCK_KEY),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *requests.lock().unwrap(),
        [(
            "https://bedrock.eu-central-1.amazonaws.com/foundation-models".to_owned(),
            vec![
                ("accept".to_owned(), "application/json".to_owned()),
                (
                    "authorization".to_owned(),
                    format!("Bearer {TEST_BEDROCK_KEY}")
                ),
            ]
        )]
    );
}

#[tokio::test]
async fn bedrock_validation_without_a_region_fails_instead_of_guessing_one() {
    let (http, requests) = scripted_http(Vec::new());
    assert_eq!(
        AwsBedrockApiKeyValidator::new(http, None)
            .validate(
                &CredentialSecret::new(TEST_BEDROCK_KEY),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Host
    );
    assert!(
        urls(&requests).is_empty(),
        "no endpoint may be addressed without a configured region"
    );
}

#[tokio::test]
async fn bedrock_status_codes_map_onto_the_closed_safe_taxonomy() {
    for (status, expected) in [
        (401, ApiKeyValidationFailure::Unauthorized),
        (403, ApiKeyValidationFailure::Unauthorized),
        (404, ApiKeyValidationFailure::Host),
        (400, ApiKeyValidationFailure::Host),
        (418, ApiKeyValidationFailure::Host),
        (429, ApiKeyValidationFailure::Network),
        (500, ApiKeyValidationFailure::Network),
        (503, ApiKeyValidationFailure::Network),
    ] {
        let (http, _) = scripted_http(vec![error_reply(status)]);
        assert_eq!(
            AwsBedrockApiKeyValidator::new(http, Some(&region()))
                .validate(
                    &CredentialSecret::new(TEST_BEDROCK_KEY),
                    CancellationToken::new()
                )
                .await
                .unwrap_err(),
            expected,
            "HTTP {status}"
        );
    }

    let (http, _) = scripted_http(vec![Reply::Response(HttpResponse {
        headers: BTreeMap::new(),
        status: 200,
        content_type: Some("text/html".to_owned()),
        body: b"<html>captive-portal-must-not-leak</html>".to_vec(),
    })]);
    assert_eq!(
        AwsBedrockApiKeyValidator::new(http, Some(&region()))
            .validate(
                &CredentialSecret::new(TEST_BEDROCK_KEY),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Host,
        "a non-JSON 200 is a wrong endpoint, not an accepted key"
    );

    let (http, _) = scripted_http(vec![Reply::Failure(TransportError::Timeout)]);
    assert_eq!(
        AwsBedrockApiKeyValidator::new(http, Some(&region()))
            .validate(
                &CredentialSecret::new(TEST_BEDROCK_KEY),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Network
    );
}

#[tokio::test]
async fn cancellation_stops_bedrock_validation_before_any_request() {
    let (http, requests) = scripted_http(vec![json_reply(200, serde_json::json!({}))]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        AwsBedrockApiKeyValidator::new(http, Some(&region()))
            .validate(&CredentialSecret::new(TEST_BEDROCK_KEY), cancellation)
            .await
            .unwrap_err(),
        ApiKeyValidationFailure::Cancelled
    );
    assert!(urls(&requests).is_empty());
}

/// Hosted credential/region canary. The key is moved directly from the
/// process environment into `CredentialSecret`; no assertion or diagnostic
/// has a value-bearing field.
#[tokio::test]
async fn live_bedrock_api_key_validation_smoke() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1") {
        return;
    }
    if std::env::var_os(AWS_BEARER_TOKEN_BEDROCK_VAR)
        .filter(|value| !value.is_empty())
        .is_none()
    {
        return;
    }
    let host = ProcessAwsHost;
    let profile = AwsProfileResolution::resolve(&host);
    let resolution = AwsRegionResolution::resolve(&host, &profile);
    let Some(region) = resolution.region() else {
        return;
    };
    let secret = CredentialSecret::new(
        std::env::var(AWS_BEARER_TOKEN_BEDROCK_VAR)
            .expect("the checked Bedrock environment reference must remain readable"),
    );
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    AwsBedrockApiKeyValidator::new(
        HttpService::new(std::sync::Arc::new(transport)),
        Some(region),
    )
    .validate(&secret, CancellationToken::new())
    .await
    .expect("the configured Bedrock key and region must pass the control-plane check");
}

#[tokio::test]
async fn the_relative_container_uri_is_addressed_on_the_documented_ecs_host() {
    let host = MapAwsHost::new().with_var(
        AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
        "/v2/credentials/abc",
    );
    let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
    let status = container_role_status(
        &http,
        &host,
        AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
        CancellationToken::new(),
        CHECKED_AT,
    )
    .await;
    assert_eq!(
        status,
        AwsCredentialStatus::Valid {
            source: AwsCredentialSource::ContainerRole {
                variable: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR
            },
            checked_at_ms: CHECKED_AT,
        }
    );
    assert_eq!(
        urls(&requests),
        ["http://169.254.170.2/v2/credentials/abc".to_owned()]
    );
}

#[tokio::test]
async fn a_container_endpoint_outside_loopback_is_refused_before_the_token_is_sent() {
    for endpoint in [
        "http://credentials.example.com/creds",
        "http://10.0.0.5/creds",
        "http://[2001:db8::1]/creds",
        "ftp://localhost/creds",
        "not-a-url",
    ] {
        let host = MapAwsHost::new()
            .with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, endpoint)
            .with_var(AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR, "token-must-not-leak");
        let (http, requests) = scripted_http(Vec::new());
        let status = container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            CancellationToken::new(),
            CHECKED_AT,
        )
        .await;
        assert_eq!(
            status,
            AwsCredentialStatus::Rejected {
                source: AwsCredentialSource::ContainerRole {
                    variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR
                },
                reason: AwsRejection::UnsafeEndpoint,
            },
            "{endpoint} must not receive an authorization token"
        );
        assert!(urls(&requests).is_empty(), "{endpoint} was contacted");
    }

    for endpoint in [
        "http://localhost:8080/creds",
        "http://127.0.0.1/creds",
        "http://[::1]:8080/creds",
        "http://169.254.170.23/v1/credentials",
        "http://[fd00:ec2::23]/creds",
        "https://pod-identity.example.com/creds",
    ] {
        let host = MapAwsHost::new().with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, endpoint);
        let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
        let status = container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            CancellationToken::new(),
            CHECKED_AT,
        )
        .await;
        assert!(status.is_valid(), "{endpoint} should be reachable");
        assert_eq!(urls(&requests), [endpoint.to_owned()]);
    }
}

#[tokio::test]
async fn the_container_token_comes_from_the_variable_and_otherwise_from_its_file() {
    let inline = MapAwsHost::new()
        .with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c")
        .with_var(AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR, "inline-token")
        .with_var(AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR, "/var/run/token")
        .with_file("/var/run/token", "file-token\n");
    let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
    container_role_status(
        &http,
        &inline,
        AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
        CancellationToken::new(),
        CHECKED_AT,
    )
    .await;
    let sent = requests.lock().unwrap()[0].1.clone();
    assert!(sent.contains(&("authorization".to_owned(), "inline-token".to_owned())));

    let from_file = MapAwsHost::new()
        .with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c")
        .with_var(AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR, "/var/run/token")
        .with_file("/var/run/token", "file-token\n");
    let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
    container_role_status(
        &http,
        &from_file,
        AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
        CancellationToken::new(),
        CHECKED_AT,
    )
    .await;
    let sent = requests.lock().unwrap()[0].1.clone();
    assert!(sent.contains(&("authorization".to_owned(), "file-token".to_owned())));

    let none =
        MapAwsHost::new().with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c");
    let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
    container_role_status(
        &http,
        &none,
        AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
        CancellationToken::new(),
        CHECKED_AT,
    )
    .await;
    let sent = requests.lock().unwrap()[0].1.clone();
    assert!(
        sent.iter().all(|(name, _)| name != "authorization"),
        "no token configured means no authorization header"
    );
}

#[tokio::test]
async fn a_container_success_requires_both_credential_fields_in_the_response() {
    let host =
        MapAwsHost::new().with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c");
    for body in [
        serde_json::json!({"AccessKeyId": "ASIA"}),
        serde_json::json!({"SecretAccessKey": "s"}),
        serde_json::json!({"AccessKeyId": "", "SecretAccessKey": "s"}),
        serde_json::json!({"AccessKeyId": "ASIA", "SecretAccessKey": "  "}),
        serde_json::json!({"Message": "not credentials"}),
    ] {
        let (http, _) = scripted_http(vec![json_reply(200, body.clone())]);
        assert_eq!(
            container_role_status(
                &http,
                &host,
                AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
                CancellationToken::new(),
                CHECKED_AT,
            )
            .await,
            AwsCredentialStatus::Undetermined {
                source: Some(AwsCredentialSource::ContainerRole {
                    variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR
                }),
                reason: AwsUndetermined::UnusableResponse,
            },
            "{body} must not read as usable credentials"
        );
    }
}

#[tokio::test]
async fn an_unreachable_container_endpoint_never_becomes_valid_or_rejected() {
    let host =
        MapAwsHost::new().with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c");
    let source = AwsCredentialSource::ContainerRole {
        variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    };
    for (reply, expected) in [
        (error_reply(500), AwsUndetermined::ServiceUnreachable),
        (error_reply(429), AwsUndetermined::ServiceUnreachable),
        (
            Reply::Failure(TransportError::Network {
                message: "dns-detail-must-not-leak".to_owned(),
            }),
            AwsUndetermined::ServiceUnreachable,
        ),
        (
            Reply::Failure(TransportError::Timeout),
            AwsUndetermined::ServiceUnreachable,
        ),
        (error_reply(418), AwsUndetermined::UnusableResponse),
    ] {
        let (http, _) = scripted_http(vec![reply]);
        let status = container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            CancellationToken::new(),
            CHECKED_AT,
        )
        .await;
        assert_eq!(
            status,
            AwsCredentialStatus::Undetermined {
                source: Some(source.clone()),
                reason: expected,
            }
        );
        assert!(!status.is_valid() && !status.is_rejected());
    }

    let (http, _) = scripted_http(vec![error_reply(403)]);
    assert_eq!(
        container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            CancellationToken::new(),
            CHECKED_AT,
        )
        .await,
        AwsCredentialStatus::Rejected {
            source,
            reason: AwsRejection::Unauthorized,
        },
        "only an explicit refusal is a rejection"
    );
}

#[tokio::test]
async fn cancellation_stops_the_container_check_before_any_request() {
    let host =
        MapAwsHost::new().with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, "http://localhost/c");
    let (http, requests) = scripted_http(vec![json_reply(200, credential_body())]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
            cancellation,
            CHECKED_AT,
        )
        .await,
        AwsCredentialStatus::Undetermined {
            source: Some(AwsCredentialSource::ContainerRole {
                variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR
            }),
            reason: AwsUndetermined::Cancelled,
        }
    );
    assert!(urls(&requests).is_empty());
}

#[test]
fn a_status_is_valid_only_when_a_check_proved_it() {
    let source = AwsCredentialSource::ContainerRole {
        variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    };
    let valid = AwsCredentialStatus::Valid {
        source: source.clone(),
        checked_at_ms: CHECKED_AT,
    };
    let undetermined = AwsCredentialStatus::Undetermined {
        source: Some(source.clone()),
        reason: AwsUndetermined::ServiceUnreachable,
    };
    let rejected = AwsCredentialStatus::Rejected {
        source,
        reason: AwsRejection::Unauthorized,
    };
    assert!(valid.is_valid() && !valid.is_rejected());
    assert!(
        !undetermined.is_valid() && !undetermined.is_rejected(),
        "an undetermined check is neither a pass nor a failure"
    );
    assert!(rejected.is_rejected() && !rejected.is_valid());
    assert!(!AwsCredentialStatus::Absent.is_valid() && !AwsCredentialStatus::Absent.is_rejected());
    assert_eq!(
        [
            AwsCredentialStatus::Absent.code(),
            undetermined.code(),
            valid.code(),
            rejected.code()
        ],
        ["absent", "undetermined", "valid", "rejected"]
    );
}

#[tokio::test]
async fn a_named_but_unusable_token_file_is_a_rejection_not_an_unauthenticated_request() {
    let endpoint = "http://localhost/c";
    let base = || {
        MapAwsHost::new()
            .with_var(AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR, endpoint)
            .with_var(AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR, "/var/run/token")
    };
    for (label, host) in [
        ("absent", base()),
        ("unreadable", base().with_unreadable_file("/var/run/token")),
        ("empty", base().with_file("/var/run/token", "   \n")),
    ] {
        let (http, requests) = scripted_http(Vec::new());
        assert_eq!(
            container_role_status(
                &http,
                &host,
                AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
                CancellationToken::new(),
                CHECKED_AT,
            )
            .await,
            AwsCredentialStatus::Rejected {
                source: AwsCredentialSource::ContainerRole {
                    variable: AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR
                },
                reason: AwsRejection::IncompleteConfiguration,
            },
            "a {label} token file must not become a blamed-on-AWS 403"
        );
        assert!(
            urls(&requests).is_empty(),
            "{label} token file was sent anyway"
        );
    }
}

#[tokio::test]
async fn a_relative_container_uri_that_is_not_a_path_is_incomplete_configuration() {
    let host = MapAwsHost::new().with_var(
        AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
        "169.254.170.2/v2/credentials",
    );
    let (http, requests) = scripted_http(Vec::new());
    assert_eq!(
        container_role_status(
            &http,
            &host,
            AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
            CancellationToken::new(),
            CHECKED_AT,
        )
        .await,
        AwsCredentialStatus::Rejected {
            source: AwsCredentialSource::ContainerRole {
                variable: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR
            },
            reason: AwsRejection::IncompleteConfiguration,
        }
    );
    assert!(urls(&requests).is_empty());
}
