//! Live checks that turn a discovered AWS credential source into a verdict.

use async_trait::async_trait;
use heycode_authorization_api_key::{ApiKeyValidationFailure, ApiKeyValidator};
use heycode_credentials::CredentialSecret;
use heycode_http::{HttpRequest, HttpResponse, HttpService, TransportError};
use tokio_util::sync::CancellationToken;

use crate::{
    AwsCredentialSource, AwsCredentialStatus, AwsFileRead, AwsHost, AwsRegion, AwsRejection,
    AwsUndetermined,
};

/// Environment variable the Amazon Bedrock service reads an API key from.
pub const AWS_BEARER_TOKEN_BEDROCK_VAR: &str = "AWS_BEARER_TOKEN_BEDROCK";
/// Plain-text container authorization token.
pub const AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR: &str = "AWS_CONTAINER_AUTHORIZATION_TOKEN";
/// File holding the container authorization token.
pub const AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR: &str =
    "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE";
/// Documented Amazon ECS host the relative credential URI hangs off.
pub const ECS_CREDENTIAL_HOST: &str = "http://169.254.170.2";

/// Cap on a validation response body. Neither endpoint returns anything large,
/// and a check has no reason to buffer a megabyte to answer a yes/no.
const VALIDATION_RESPONSE_LIMIT: usize = 64 * 1024;

/// Live Amazon Bedrock API-key validator.
///
/// The key is a bearer token for the Bedrock control and runtime planes, so
/// `GET /foundation-models` on the regional control-plane endpoint is the
/// cheapest documented request that proves acceptance.
pub struct AwsBedrockApiKeyValidator {
    http: HttpService,
    endpoint: Option<String>,
}

impl AwsBedrockApiKeyValidator {
    /// Build a validator for one region.
    ///
    /// A `None` region yields a validator that cannot address an endpoint and
    /// says so. It deliberately does not substitute a default region: sending
    /// a caller's key to `us-east-1` because nothing said otherwise is a
    /// guess with an account boundary on the other side of it.
    #[must_use]
    pub fn new(http: HttpService, region: Option<&AwsRegion>) -> Self {
        Self {
            endpoint: region.map(|region| {
                format!(
                    "https://bedrock.{}.amazonaws.com/foundation-models",
                    region.as_str()
                )
            }),
            http,
        }
    }
}

#[async_trait]
impl ApiKeyValidator for AwsBedrockApiKeyValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        if cancellation.is_cancelled() {
            return Err(ApiKeyValidationFailure::Cancelled);
        }
        let Some(endpoint) = self.endpoint.as_ref() else {
            return Err(ApiKeyValidationFailure::Host);
        };
        let request = HttpRequest::get(endpoint)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| {
                request.header("authorization", &format!("Bearer {}", secret.expose()))
            })
            .map(|request| request.with_max_response_bytes(VALIDATION_RESPONSE_LIMIT));
        let Ok(request) = request else {
            return Err(ApiKeyValidationFailure::Host);
        };
        match self.http.send(request, cancellation).await {
            Ok(response) => classify_bedrock(&response),
            Err(error) => Err(map_transport_error(error)),
        }
    }
}

/// Project a closed API-key verdict onto the credential status vocabulary.
///
/// Only an explicit rejection becomes `Rejected`. Everything else the
/// taxonomy can report is a check that did not conclude, and stays
/// `Undetermined`.
#[must_use]
pub fn api_key_status(
    source: AwsCredentialSource,
    outcome: Result<(), ApiKeyValidationFailure>,
    checked_at_ms: u64,
) -> AwsCredentialStatus {
    match outcome {
        Ok(()) => AwsCredentialStatus::Valid {
            source,
            checked_at_ms,
        },
        Err(ApiKeyValidationFailure::Unauthorized) => AwsCredentialStatus::Rejected {
            source,
            reason: AwsRejection::Unauthorized,
        },
        Err(ApiKeyValidationFailure::Network) => AwsCredentialStatus::Undetermined {
            source: Some(source),
            reason: AwsUndetermined::ServiceUnreachable,
        },
        Err(ApiKeyValidationFailure::Cancelled) => AwsCredentialStatus::Undetermined {
            source: Some(source),
            reason: AwsUndetermined::Cancelled,
        },
        // `Model` is unreachable here because this validator asks for no
        // model entitlement; it shares `Host`'s "the endpoint did not answer
        // usefully" meaning either way.
        Err(ApiKeyValidationFailure::Host | ApiKeyValidationFailure::Model) => {
            AwsCredentialStatus::Undetermined {
                source: Some(source),
                reason: AwsUndetermined::UnusableResponse,
            }
        }
    }
}

/// Run the unsigned container credential check.
///
/// This is the one chain source with a documented endpoint that answers
/// without a SigV4 signature, so it is the one chain source this crate can
/// turn into a verdict on its own.
pub async fn container_role_status(
    http: &HttpService,
    host: &dyn AwsHost,
    variable: &'static str,
    cancellation: CancellationToken,
    checked_at_ms: u64,
) -> AwsCredentialStatus {
    let source = AwsCredentialSource::ContainerRole { variable };
    if cancellation.is_cancelled() {
        return undetermined(source, AwsUndetermined::Cancelled);
    }
    let Some(configured) = host.var(variable) else {
        return AwsCredentialStatus::Absent;
    };
    let url = match container_endpoint(variable, &configured) {
        Ok(url) => url,
        Err(reason) => return AwsCredentialStatus::Rejected { source, reason },
    };
    let mut request = match HttpRequest::get(&url)
        .and_then(|request| request.header("accept", "application/json"))
        .map(|request| request.with_max_response_bytes(VALIDATION_RESPONSE_LIMIT))
    {
        Ok(request) => request,
        Err(_) => {
            return AwsCredentialStatus::Rejected {
                source,
                reason: AwsRejection::UnsafeEndpoint,
            };
        }
    };
    match container_token(host) {
        ContainerToken::Unconfigured => {}
        ContainerToken::Resolved(token) => match request.header("authorization", token.expose()) {
            Ok(next) => request = next,
            Err(_) => return undetermined(source, AwsUndetermined::UnusableResponse),
        },
        // A token source that yields nothing is a determinate misconfiguration.
        // Sending the request anyway would collect a 403 and report the role as
        // rejected, which blames AWS for a local mistake.
        ContainerToken::Missing => {
            return AwsCredentialStatus::Rejected {
                source,
                reason: AwsRejection::IncompleteConfiguration,
            };
        }
    }
    match http.send(request, cancellation).await {
        Ok(response) => classify_container(source, &response, checked_at_ms),
        Err(error) => match map_transport_error(error) {
            ApiKeyValidationFailure::Unauthorized => AwsCredentialStatus::Rejected {
                source,
                reason: AwsRejection::Unauthorized,
            },
            ApiKeyValidationFailure::Cancelled => undetermined(source, AwsUndetermined::Cancelled),
            ApiKeyValidationFailure::Network => {
                undetermined(source, AwsUndetermined::ServiceUnreachable)
            }
            ApiKeyValidationFailure::Host | ApiKeyValidationFailure::Model => {
                undetermined(source, AwsUndetermined::UnusableResponse)
            }
        },
    }
}

fn undetermined(source: AwsCredentialSource, reason: AwsUndetermined) -> AwsCredentialStatus {
    AwsCredentialStatus::Undetermined {
        source: Some(source),
        reason,
    }
}

/// Resolve the container credential URL, refusing an endpoint that would send
/// the authorization token somewhere it does not belong.
fn container_endpoint(variable: &'static str, configured: &str) -> Result<String, AwsRejection> {
    if variable == crate::AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR {
        if !configured.starts_with('/') {
            return Err(AwsRejection::IncompleteConfiguration);
        }
        return Ok(format!("{ECS_CREDENTIAL_HOST}{configured}"));
    }
    let Some(rest) = configured.strip_prefix("http://") else {
        return configured
            .starts_with("https://")
            .then(|| configured.to_owned())
            .ok_or(AwsRejection::UnsafeEndpoint);
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // An IPv6 authority keeps its brackets and ends at `]`; only a port
        // may follow, so splitting on `:` first would cut the address apart.
        Some(inside) => inside
            .split_once(']')
            .map_or(authority, |(address, _)| &authority[..address.len() + 2]),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let loopback = host == "localhost"
        || host == "[::1]"
        || host.starts_with("127.")
        || host.starts_with("169.254.")
        || host == "[fd00:ec2::23]";
    if loopback {
        Ok(configured.to_owned())
    } else {
        Err(AwsRejection::UnsafeEndpoint)
    }
}

/// Outcome of resolving the container authorization token.
enum ContainerToken {
    /// Nothing names a token; the endpoint is contacted without one.
    Unconfigured,
    /// A token was resolved. It is carried redacted and exposed only at the
    /// header it is written into.
    Resolved(CredentialSecret),
    /// A token source is named but produced nothing usable.
    Missing,
}

/// Read the container authorization token: the plain-text variable first, then
/// the file the documented alternative names.
fn container_token(host: &dyn AwsHost) -> ContainerToken {
    if let Some(value) = host.var(AWS_CONTAINER_AUTHORIZATION_TOKEN_VAR) {
        return ContainerToken::Resolved(CredentialSecret::new(value));
    }
    let Some(path) = host.var(AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE_VAR) else {
        return ContainerToken::Unconfigured;
    };
    match host.read_file(std::path::Path::new(&path)) {
        AwsFileRead::Found(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                ContainerToken::Missing
            } else {
                ContainerToken::Resolved(CredentialSecret::new(trimmed))
            }
        }
        AwsFileRead::Absent | AwsFileRead::Unreadable => ContainerToken::Missing,
    }
}

fn classify_bedrock(response: &HttpResponse) -> Result<(), ApiKeyValidationFailure> {
    match response.status {
        200..=299 => {
            if is_json(response) {
                Ok(())
            } else {
                Err(ApiKeyValidationFailure::Host)
            }
        }
        401 | 403 => Err(ApiKeyValidationFailure::Unauthorized),
        429 | 500..=599 => Err(ApiKeyValidationFailure::Network),
        _ => Err(ApiKeyValidationFailure::Host),
    }
}

fn classify_container(
    source: AwsCredentialSource,
    response: &HttpResponse,
    checked_at_ms: u64,
) -> AwsCredentialStatus {
    match response.status {
        200..=299 => {
            if container_body_carries_credentials(&response.body) {
                AwsCredentialStatus::Valid {
                    source,
                    checked_at_ms,
                }
            } else {
                undetermined(source, AwsUndetermined::UnusableResponse)
            }
        }
        401 | 403 => AwsCredentialStatus::Rejected {
            source,
            reason: AwsRejection::Unauthorized,
        },
        429 | 500..=599 => undetermined(source, AwsUndetermined::ServiceUnreachable),
        _ => undetermined(source, AwsUndetermined::UnusableResponse),
    }
}

/// Decide whether the endpoint returned usable credentials.
///
/// The response body is the one place in this crate where AWS secret material
/// arrives, so the deserializer is written to answer *presence* and keep no
/// value: [`NonEmptyField`] discards the string it inspects instead of storing
/// it, and the response itself is dropped by the caller.
fn container_body_carries_credentials(body: &[u8]) -> bool {
    serde_json::from_slice::<ContainerCredentials>(body)
        .is_ok_and(|parsed| parsed.access_key_id.0 && parsed.secret_access_key.0)
}

#[derive(serde::Deserialize)]
struct ContainerCredentials {
    #[serde(rename = "AccessKeyId")]
    access_key_id: NonEmptyField,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: NonEmptyField,
}

/// A JSON string reduced to "was it non-empty" at parse time.
struct NonEmptyField(bool);

impl<'de> serde::Deserialize<'de> for NonEmptyField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Presence;
        impl serde::de::Visitor<'_> for Presence {
            type Value = NonEmptyField;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(NonEmptyField(!value.trim().is_empty()))
            }
        }
        deserializer.deserialize_str(Presence)
    }
}

fn is_json(response: &HttpResponse) -> bool {
    response.content_type.as_deref().is_some_and(|value| {
        value == "application/json"
            || value.starts_with("application/json;")
            || value.ends_with("+json")
    })
}

fn map_transport_error(error: TransportError) -> ApiKeyValidationFailure {
    match error {
        TransportError::Cancelled => ApiKeyValidationFailure::Cancelled,
        TransportError::Http {
            status: 401 | 403, ..
        } => ApiKeyValidationFailure::Unauthorized,
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            ApiKeyValidationFailure::Network
        }
        TransportError::Network { .. } | TransportError::Timeout => {
            ApiKeyValidationFailure::Network
        }
        _ => ApiKeyValidationFailure::Host,
    }
}
