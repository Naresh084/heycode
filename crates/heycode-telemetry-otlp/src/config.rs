//! Validated, wire-safe OTLP/HTTP settings.

use std::time::Duration;

use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_http::HttpRequest;
use heycode_settings::{
    SettingsApplies, SettingsDefinition, SettingsError, SettingsFieldPath, SettingsNamespace,
    SettingsSchema,
};
use heycode_telemetry::{Label, OtlpAttributeKey, OtlpEndpoint, OtlpResource};
use serde_json::{Value, json};

/// Official default OTLP/HTTP metrics endpoint.
pub const DEFAULT_METRICS_ENDPOINT: &str = "http://localhost:4318/v1/metrics";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_ATTEMPTS: usize = 3;
const DEFAULT_INITIAL_BACKOFF_MS: u64 = 100;
const DEFAULT_MAX_BACKOFF_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MAX_ATTEMPTS: usize = 5;
const MAX_BACKOFF_MS: u64 = 30_000;

/// How a resolved credential becomes one request-header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpAuthScheme {
    /// Send the secret bytes as the complete header value.
    Raw,
    /// Prefix the secret with `Bearer `.
    Bearer,
}

impl OtlpAuthScheme {
    /// Stable settings identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Bearer => "bearer",
        }
    }

    fn parse(value: &str) -> Result<Self, OtlpHttpConfigFault> {
        match value {
            "raw" => Ok(Self::Raw),
            "bearer" => Ok(Self::Bearer),
            _ => Err(OtlpHttpConfigFault::InvalidAuthentication),
        }
    }
}

/// Non-secret authentication configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpHttpAuthConfig {
    header: Label,
    credential_label: Label,
    query: CredentialQuery,
    scheme: OtlpAuthScheme,
}

impl OtlpHttpAuthConfig {
    /// Validated HTTP header name.
    #[must_use]
    pub const fn header(&self) -> &Label {
        &self.header
    }

    /// Credential query resolved for each exported batch.
    #[must_use]
    pub const fn query(&self) -> &CredentialQuery {
        &self.query
    }

    /// Safe credential identifier used by diagnostics.
    #[must_use]
    pub const fn credential_label(&self) -> &Label {
        &self.credential_label
    }

    /// Header-value construction mode.
    #[must_use]
    pub const fn scheme(&self) -> OtlpAuthScheme {
        self.scheme
    }
}

/// Fully resolved OTLP/HTTP JSON configuration.
#[derive(Debug, Clone)]
pub struct OtlpHttpConfig {
    endpoint: OtlpEndpoint,
    resource: OtlpResource,
    auth: Option<OtlpHttpAuthConfig>,
    timeout: Duration,
    max_request_bytes: usize,
    max_response_bytes: usize,
    max_attempts: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl OtlpHttpConfig {
    /// Parse one fully layered settings value.
    ///
    /// # Errors
    /// Returns a closed field-class failure without retaining rejected text.
    pub fn from_value(value: &Value) -> Result<Self, OtlpHttpConfigFault> {
        let object = exact_object(
            value,
            &[
                "protocol",
                "compression",
                "endpoint",
                "resource",
                "auth",
                "timeout_ms",
                "max_request_bytes",
                "max_response_bytes",
                "max_attempts",
                "initial_backoff_ms",
                "max_backoff_ms",
            ],
        )?;
        if object.get("protocol").and_then(Value::as_str) != Some("http/json") {
            return Err(OtlpHttpConfigFault::InvalidProtocol);
        }
        if object.get("compression").and_then(Value::as_str) != Some("none") {
            return Err(OtlpHttpConfigFault::InvalidCompression);
        }
        let endpoint = object
            .get("endpoint")
            .and_then(Value::as_str)
            .ok_or(OtlpHttpConfigFault::InvalidEndpoint)
            .and_then(|value| {
                OtlpEndpoint::parse(value).map_err(|_| OtlpHttpConfigFault::InvalidEndpoint)
            })?;
        let resource = parse_resource(
            object
                .get("resource")
                .ok_or(OtlpHttpConfigFault::InvalidResource)?,
        )?;
        let auth = parse_auth(
            object
                .get("auth")
                .ok_or(OtlpHttpConfigFault::InvalidAuthentication)?,
            &endpoint,
        )?;
        let timeout_ms = integer(object, "timeout_ms")?;
        if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
            return Err(OtlpHttpConfigFault::InvalidTimeout);
        }
        let max_request_bytes = usize_integer(object, "max_request_bytes")?;
        if max_request_bytes == 0 || max_request_bytes > DEFAULT_MAX_REQUEST_BYTES {
            return Err(OtlpHttpConfigFault::InvalidRequestLimit);
        }
        let max_response_bytes = usize_integer(object, "max_response_bytes")?;
        if max_response_bytes == 0 || max_response_bytes > DEFAULT_MAX_RESPONSE_BYTES {
            return Err(OtlpHttpConfigFault::InvalidResponseLimit);
        }
        let max_attempts = usize_integer(object, "max_attempts")?;
        let initial_backoff_ms = integer(object, "initial_backoff_ms")?;
        let max_backoff_ms = integer(object, "max_backoff_ms")?;
        if !(1..=MAX_ATTEMPTS).contains(&max_attempts)
            || initial_backoff_ms == 0
            || max_backoff_ms == 0
            || initial_backoff_ms > max_backoff_ms
            || max_backoff_ms > MAX_BACKOFF_MS
        {
            return Err(OtlpHttpConfigFault::InvalidRetryPolicy);
        }
        Ok(Self {
            endpoint,
            resource,
            auth,
            timeout: Duration::from_millis(timeout_ms),
            max_request_bytes,
            max_response_bytes,
            max_attempts,
            initial_backoff: Duration::from_millis(initial_backoff_ms),
            max_backoff: Duration::from_millis(max_backoff_ms),
        })
    }

    /// Metrics endpoint used as-is.
    #[must_use]
    pub const fn endpoint(&self) -> &OtlpEndpoint {
        &self.endpoint
    }

    /// Screened OTLP resource.
    #[must_use]
    pub const fn resource(&self) -> &OtlpResource {
        &self.resource
    }

    /// Optional non-secret credential binding.
    #[must_use]
    pub const fn auth(&self) -> Option<&OtlpHttpAuthConfig> {
        self.auth.as_ref()
    }

    /// Per-attempt deadline.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Maximum serialized request bytes.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Maximum accepted response bytes.
    #[must_use]
    pub const fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// Total attempts including the initial request.
    #[must_use]
    pub const fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// First retry-delay ceiling.
    #[must_use]
    pub const fn initial_backoff(&self) -> Duration {
        self.initial_backoff
    }

    /// Largest retry delay, including `Retry-After` clamping.
    #[must_use]
    pub const fn max_backoff(&self) -> Duration {
        self.max_backoff
    }
}

/// Closed configuration failure that never carries rejected values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OtlpHttpConfigFault {
    /// Top-level object or key set is invalid.
    InvalidShape,
    /// Only explicit `http/json` is supported.
    InvalidProtocol,
    /// Only uncompressed requests are supported.
    InvalidCompression,
    /// Endpoint is not a screened absolute HTTP(S) metrics endpoint.
    InvalidEndpoint,
    /// Resource labels are invalid or credential-shaped.
    InvalidResource,
    /// Authentication is malformed or carries a literal secret.
    InvalidAuthentication,
    /// Attempt timeout is outside the bounded range.
    InvalidTimeout,
    /// Request-size cap is outside the bounded range.
    InvalidRequestLimit,
    /// Response-size cap is outside the bounded range.
    InvalidResponseLimit,
    /// Attempt count or backoff policy is invalid.
    InvalidRetryPolicy,
}

impl std::fmt::Display for OtlpHttpConfigFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidShape => "OTLP settings shape is invalid",
            Self::InvalidProtocol => "OTLP protocol must be http/json",
            Self::InvalidCompression => "OTLP compression must be none",
            Self::InvalidEndpoint => "OTLP metrics endpoint is invalid",
            Self::InvalidResource => "OTLP resource labels are invalid",
            Self::InvalidAuthentication => "OTLP authentication reference is invalid",
            Self::InvalidTimeout => "OTLP timeout is invalid",
            Self::InvalidRequestLimit => "OTLP request size limit is invalid",
            Self::InvalidResponseLimit => "OTLP response size limit is invalid",
            Self::InvalidRetryPolicy => "OTLP retry policy is invalid",
        })
    }
}

impl std::error::Error for OtlpHttpConfigFault {}

/// Settings namespace for the opt-in provider.
///
/// # Errors
/// The static namespace passes through normal settings validation.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("telemetry-otlp")
}

/// Restart-applied, wire-exposed settings definition.
///
/// # Errors
/// Static schema/path validation failures.
pub fn settings_definition() -> Result<SettingsDefinition, SettingsError> {
    let schema = SettingsSchema::new(
        json!({
            "type":"object",
            "additionalProperties":false,
            "properties":{
                "protocol":{"type":"string","enum":["http/json"]},
                "compression":{"type":"string","enum":["none"]},
                "endpoint":{"type":"string"},
                "resource":{
                    "type":"object",
                    "additionalProperties":false,
                    "properties":{
                        "service_name":{"type":"string"},
                        "service_namespace":{"type":["string","null"]},
                        "deployment_environment":{"type":["string","null"]}
                    }
                },
                "auth":{
                    "type":["object","null"],
                    "additionalProperties":false,
                    "properties":{
                        "header":{"type":"string"},
                        "credential_reference":{"type":"string"},
                        "credential_kind":{"type":"string"},
                        "scheme":{"type":"string","enum":["raw","bearer"]}
                    }
                },
                "timeout_ms":{"type":"integer","minimum":1,"maximum":MAX_TIMEOUT_MS},
                "max_request_bytes":{"type":"integer","minimum":1,"maximum":DEFAULT_MAX_REQUEST_BYTES},
                "max_response_bytes":{"type":"integer","minimum":1,"maximum":DEFAULT_MAX_RESPONSE_BYTES},
                "max_attempts":{"type":"integer","minimum":1,"maximum":MAX_ATTEMPTS},
                "initial_backoff_ms":{"type":"integer","minimum":1,"maximum":MAX_BACKOFF_MS},
                "max_backoff_ms":{"type":"integer","minimum":1,"maximum":MAX_BACKOFF_MS}
            }
        }),
        default_value(),
        |value| {
            OtlpHttpConfig::from_value(value)
                .map(|_| ())
                .map_err(|fault| fault.to_string())
        },
    )?
    .with_wire_exposure();
    let schema = [
        "protocol",
        "compression",
        "endpoint",
        "resource.service_name",
        "resource.service_namespace",
        "resource.deployment_environment",
        "auth",
        "timeout_ms",
        "max_request_bytes",
        "max_response_bytes",
        "max_attempts",
        "initial_backoff_ms",
        "max_backoff_ms",
    ]
    .into_iter()
    .try_fold(schema, |schema, path| {
        SettingsFieldPath::new(path).map(|path| schema.with_public_path(path))
    })?;
    Ok(SettingsDefinition::new(settings_namespace()?, schema)
        .with_applies(SettingsApplies::Restart))
}

pub(crate) fn default_value() -> Value {
    json!({
        "protocol":"http/json",
        "compression":"none",
        "endpoint":DEFAULT_METRICS_ENDPOINT,
        "resource":{
            "service_name":"heycode",
            "service_namespace":null,
            "deployment_environment":null
        },
        "auth":null,
        "timeout_ms":DEFAULT_TIMEOUT_MS,
        "max_request_bytes":DEFAULT_MAX_REQUEST_BYTES,
        "max_response_bytes":DEFAULT_MAX_RESPONSE_BYTES,
        "max_attempts":DEFAULT_MAX_ATTEMPTS,
        "initial_backoff_ms":DEFAULT_INITIAL_BACKOFF_MS,
        "max_backoff_ms":DEFAULT_MAX_BACKOFF_MS
    })
}

fn parse_resource(value: &Value) -> Result<OtlpResource, OtlpHttpConfigFault> {
    let object = exact_object(
        value,
        &[
            "service_name",
            "service_namespace",
            "deployment_environment",
        ],
    )?;
    let service_name = label(object, "service_name")?;
    let mut resource = OtlpResource::new(service_name);
    let version =
        Label::new(env!("CARGO_PKG_VERSION")).map_err(|_| OtlpHttpConfigFault::InvalidResource)?;
    resource = resource
        .with_attribute(OtlpAttributeKey::ServiceVersion, version)
        .map_err(|_| OtlpHttpConfigFault::InvalidResource)?;
    for (field, key) in [
        ("service_namespace", OtlpAttributeKey::ServiceNamespace),
        (
            "deployment_environment",
            OtlpAttributeKey::DeploymentEnvironment,
        ),
    ] {
        if let Some(value) = optional_label(object, field)? {
            resource = resource
                .with_attribute(key, value)
                .map_err(|_| OtlpHttpConfigFault::InvalidResource)?;
        }
    }
    Ok(resource)
}

fn parse_auth(
    value: &Value,
    endpoint: &OtlpEndpoint,
) -> Result<Option<OtlpHttpAuthConfig>, OtlpHttpConfigFault> {
    if value.is_null() {
        return Ok(None);
    }
    let object = exact_object(
        value,
        &[
            "header",
            "credential_reference",
            "credential_kind",
            "scheme",
        ],
    )?;
    let header = label(object, "header").map_err(|_| OtlpHttpConfigFault::InvalidAuthentication)?;
    HttpRequest::post(endpoint.as_address(), Vec::new())
        .and_then(|request| request.header(header.as_str(), "validation"))
        .map_err(|_| OtlpHttpConfigFault::InvalidAuthentication)?;
    let reference_raw = object
        .get("credential_reference")
        .and_then(Value::as_str)
        .ok_or(OtlpHttpConfigFault::InvalidAuthentication)?;
    let reference = CredentialReference::new(reference_raw)
        .map_err(|_| OtlpHttpConfigFault::InvalidAuthentication)?;
    let credential_label =
        Label::new(reference.as_str()).map_err(|_| OtlpHttpConfigFault::InvalidAuthentication)?;
    let kind = object
        .get("credential_kind")
        .and_then(Value::as_str)
        .ok_or(OtlpHttpConfigFault::InvalidAuthentication)
        .and_then(|value| {
            CredentialKind::new(value).map_err(|_| OtlpHttpConfigFault::InvalidAuthentication)
        })?;
    let scheme = object
        .get("scheme")
        .and_then(Value::as_str)
        .ok_or(OtlpHttpConfigFault::InvalidAuthentication)
        .and_then(OtlpAuthScheme::parse)?;
    Ok(Some(OtlpHttpAuthConfig {
        header,
        credential_label,
        query: CredentialQuery::new(reference, kind),
        scheme,
    }))
}

fn exact_object<'a>(
    value: &'a Value,
    keys: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, OtlpHttpConfigFault> {
    let object = value.as_object().ok_or(OtlpHttpConfigFault::InvalidShape)?;
    if object.len() != keys.len() || object.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(OtlpHttpConfigFault::InvalidShape);
    }
    Ok(object)
}

fn label(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Label, OtlpHttpConfigFault> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(OtlpHttpConfigFault::InvalidResource)
        .and_then(|value| Label::new(value).map_err(|_| OtlpHttpConfigFault::InvalidResource))
}

fn optional_label(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<Label>, OtlpHttpConfigFault> {
    match object.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Label::new(value)
            .map(Some)
            .map_err(|_| OtlpHttpConfigFault::InvalidResource),
        _ => Err(OtlpHttpConfigFault::InvalidResource),
    }
}

fn integer(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<u64, OtlpHttpConfigFault> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(OtlpHttpConfigFault::InvalidShape)
}

fn usize_integer(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<usize, OtlpHttpConfigFault> {
    integer(object, field)
        .and_then(|value| usize::try_from(value).map_err(|_| OtlpHttpConfigFault::InvalidShape))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_explicit_otlp_http_json_metrics_facts() {
        let config = OtlpHttpConfig::from_value(&default_value()).unwrap();
        assert_eq!(config.endpoint().as_address(), DEFAULT_METRICS_ENDPOINT);
        assert_eq!(config.timeout(), Duration::from_secs(10));
        assert_eq!(config.max_request_bytes(), 64 * 1024 * 1024);
        assert_eq!(config.max_response_bytes(), 4 * 1024 * 1024);
        assert_eq!(config.max_attempts(), 3);
        assert!(config.auth().is_none());
    }

    #[test]
    fn auth_is_reference_only_and_rejects_a_literal_secret() {
        let mut value = default_value();
        value["auth"] = json!({
            "header":"authorization",
            "credential_reference":"telemetry/collector",
            "credential_kind":"api-key",
            "scheme":"bearer"
        });
        let config = OtlpHttpConfig::from_value(&value).unwrap();
        let auth = config.auth().unwrap();
        assert_eq!(auth.query().reference.as_str(), "telemetry/collector");
        assert_eq!(auth.scheme(), OtlpAuthScheme::Bearer);

        value["auth"]["credential_reference"] = json!("sk-ant-api03-0123456789abcdef");
        assert_eq!(
            OtlpHttpConfig::from_value(&value).unwrap_err(),
            OtlpHttpConfigFault::InvalidAuthentication
        );
    }

    #[test]
    fn query_userinfo_and_unknown_fields_fail_without_echoing_them() {
        let canary = "sk-ant-api03-0123456789abcdef";
        let mut value = default_value();
        value["endpoint"] = json!(format!("https://collector.test/v1/metrics?token={canary}"));
        let fault = OtlpHttpConfig::from_value(&value).unwrap_err();
        assert_eq!(fault, OtlpHttpConfigFault::InvalidEndpoint);
        assert!(!format!("{fault:?} {fault}").contains(canary));

        let mut value = default_value();
        value["unexpected"] = json!(canary);
        assert_eq!(
            OtlpHttpConfig::from_value(&value).unwrap_err(),
            OtlpHttpConfigFault::InvalidShape
        );
    }

    #[test]
    fn settings_are_restart_applied_and_wire_exposure_is_provable() {
        let mut context = heycode_core::Context::new();
        let service = heycode_settings::SettingsService::new(Default::default());
        let snapshot = service
            .register(&context, settings_definition().unwrap())
            .unwrap();
        assert_eq!(snapshot.applies(), SettingsApplies::Restart);
        assert!(snapshot.wire_exposed());
        drop(snapshot);
        context.shutdown();
    }
}
