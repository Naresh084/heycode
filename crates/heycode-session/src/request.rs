//! Neutral durable request header/context snapshots.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Request snapshot validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// One field violated its durable boundary contract.
    #[error("invalid request snapshot field `{field}`: {message}")]
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Safe actionable detail.
        message: String,
    },
}

/// Secret-free resolved transport target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestTargetSnapshot {
    /// HTTP(S) base/version endpoint.
    Http {
        /// Validated base URL without embedded credentials.
        base_url: String,
    },
    /// SDK-managed cloud service.
    ManagedService {
        /// Stable service name.
        service: String,
        /// Optional region/location.
        location: Option<String>,
    },
}

/// Secret-free authentication binding class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestAuthenticationSnapshot {
    /// No authentication.
    None,
    /// Adapter instance owns a credential (legacy transition only).
    AdapterOwned,
    /// Operation-time non-secret credential reference.
    Credential {
        /// Reference only; never a secret value.
        reference: String,
    },
    /// Ambient SDK/workload identity.
    Ambient,
}

/// Whether a request was replayable, as recorded with it.
///
/// A retried request is a second identical dispatch, and whether that was
/// allowed is a property of the call the header already describes; without it
/// the durable record cannot say whether a duplicate was policy or a bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRetrySafetySnapshot {
    /// Automatic replay was disabled.
    Never,
    /// Only definitive provider failures could be replayed.
    DefinitiveFailuresOnly,
    /// The request was stateless and replayable until output began.
    StatelessPreOutput,
}

/// The retry policy that governed one dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRetrySnapshot {
    /// Attempts allowed, including the first.
    pub max_attempts: u8,
    /// What kind of failure could be replayed.
    pub safety: RequestRetrySafetySnapshot,
}

/// Explicit request options that affect provider-visible behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestOptionsSnapshot {
    /// Material input modality ids.
    pub input_modalities: Vec<String>,
    /// Exact reasoning effort id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Reasoning effort was materialized by adapter resolution.
    #[serde(default)]
    pub defaulted_reasoning_effort: bool,
    /// Structured output JSON Schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
    /// Resolved provider-native feature ids.
    pub native_features: Vec<String>,
    /// Selected logical native-tool implementations.
    #[serde(default)]
    pub native_tool_routes: Vec<heycode_core::NativeToolRoute>,
    /// Provider-owned schema-tagged request options.
    #[serde(default)]
    pub provider_options: Vec<heycode_core::ProviderRequestOption>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Effective output cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Output cap was materialized by adapter resolution.
    #[serde(default)]
    pub defaulted_max_output_tokens: bool,
    /// Stable request purpose id.
    pub purpose: String,
    /// Retry policy resolved for this dispatch. Absent in records written
    /// before the field existed — which is "not recorded", never "never".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RequestRetrySnapshot>,
}

/// Complete route/prompt/tool/options snapshot committed before dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestHeaderSnapshot {
    /// Hash-only configuration lineage; absent in older or auxiliary records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<crate::RequestConfigurationSnapshot>,
    /// Provider route id.
    pub provider: String,
    /// Canonical model id.
    pub model: String,
    /// Resolved wire protocol.
    pub protocol: heycode_core::ProviderProtocol,
    /// Resolved transport target.
    pub target: RequestTargetSnapshot,
    /// Secret-free authentication class/reference.
    pub authentication: RequestAuthenticationSnapshot,
    /// Full rendered system prompt, when non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// SHA-256 of the exact system bytes (empty bytes when absent).
    pub prompt_sha256: String,
    /// Complete model-visible client tool schemas in order.
    pub tools: Vec<heycode_core::ToolSpec>,
    /// Explicit effective request options.
    pub options: RequestOptionsSnapshot,
}

impl RequestHeaderSnapshot {
    /// Construct and validate one complete durable request header.
    ///
    /// # Errors
    /// Blank route/target/options, unsafe target shape, invalid/duplicate tool
    /// schemas, non-finite sampling or invalid limits.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        protocol: heycode_core::ProviderProtocol,
        target: RequestTargetSnapshot,
        authentication: RequestAuthenticationSnapshot,
        system: Option<String>,
        tools: Vec<heycode_core::ToolSpec>,
        options: RequestOptionsSnapshot,
    ) -> Result<Self, SnapshotError> {
        let prompt_sha256 = prompt_sha256(system.as_deref().unwrap_or(""));
        let snapshot = Self {
            configuration: None,
            provider: provider.into(),
            model: model.into(),
            protocol,
            target,
            authentication,
            system,
            prompt_sha256,
            tools,
            options,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Revalidate a deserialized/mutated snapshot including prompt hash.
    ///
    /// # Errors
    /// Any field no longer satisfies the constructor contract.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        non_blank("provider", &self.provider)?;
        non_blank("model", &self.model)?;
        match &self.target {
            RequestTargetSnapshot::Http { base_url } => {
                let authority = base_url
                    .strip_prefix("http://")
                    .or_else(|| base_url.strip_prefix("https://"))
                    .and_then(|remainder| remainder.split(['/', '?', '#']).next());
                if base_url.chars().any(char::is_whitespace)
                    || authority.is_none_or(|value| value.is_empty() || value.contains('@'))
                {
                    return invalid(
                        "target",
                        "HTTP target must be absolute http(s), whitespace-free and credential-free",
                    );
                }
            }
            RequestTargetSnapshot::ManagedService { service, location } => {
                non_blank("target", service)?;
                if let Some(location) = location {
                    non_blank("target", location)?;
                }
            }
        }
        if let RequestAuthenticationSnapshot::Credential { reference } = &self.authentication {
            non_blank("authentication", reference)?;
        }
        if self.system.as_deref() == Some("") {
            return invalid(
                "system",
                "empty system prompt must be represented as absent",
            );
        }
        let expected = prompt_sha256(self.system.as_deref().unwrap_or(""));
        if self.prompt_sha256 != expected {
            return invalid(
                "prompt_sha256",
                "hash does not match exact system prompt bytes",
            );
        }
        if self.prompt_sha256.len() != 64
            || !self
                .prompt_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return invalid("prompt_sha256", "hash must be lowercase SHA-256 hex");
        }
        let mut tool_names = BTreeSet::new();
        for tool in &self.tools {
            non_blank("tools", &tool.name)?;
            if !tool.parameters.is_object() || !tool_names.insert(tool.name.as_str()) {
                return invalid(
                    "tools",
                    "tool names must be unique and parameters must be schema objects",
                );
            }
        }
        self.options.validate_for_provider(&self.provider)?;
        if let Some(configuration) = &self.configuration {
            configuration.validate(self)?;
        }
        Ok(())
    }
}

impl RequestOptionsSnapshot {
    fn validate_for_provider(&self, provider: &str) -> Result<(), SnapshotError> {
        if self.input_modalities.is_empty() {
            return invalid("input_modalities", "at least one modality is required");
        }
        unique_non_blank("input_modalities", &self.input_modalities)?;
        if let Some(effort) = &self.reasoning_effort {
            non_blank("reasoning_effort", effort)?;
        }
        if self
            .structured_output
            .as_ref()
            .is_some_and(|schema| !schema.is_object())
        {
            return invalid("structured_output", "JSON Schema must be an object");
        }
        unique_non_blank("native_features", &self.native_features)?;
        let mut native_logical = BTreeSet::new();
        let mut prior_native_logical: Option<&str> = None;
        for route in &self.native_tool_routes {
            route
                .validate()
                .map_err(|error| SnapshotError::InvalidField {
                    field: "native_tool_routes",
                    message: error.to_string(),
                })?;
            if !native_logical.insert(route.logical())
                || prior_native_logical.is_some_and(|prior| prior >= route.logical())
            {
                return invalid(
                    "native_tool_routes",
                    "logical routes must be unique and sorted",
                );
            }
            prior_native_logical = Some(route.logical());
        }
        let mut option_kinds = BTreeSet::new();
        for option in &self.provider_options {
            option
                .validate()
                .map_err(|error| SnapshotError::InvalidField {
                    field: "provider_options",
                    message: error.to_string(),
                })?;
            if option.provider() != provider || !option_kinds.insert(option.kind()) {
                return invalid(
                    "provider_options",
                    "option owners must match the request provider and kinds must be unique",
                );
            }
        }
        if self.temperature.is_some_and(|value| !value.is_finite()) {
            return invalid("temperature", "temperature must be finite");
        }
        if self.max_output_tokens == Some(0) {
            return invalid("max_output_tokens", "output cap must be positive");
        }
        non_blank("purpose", &self.purpose)
    }
}

/// One request contributor, retained for diagnostics after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContributorSnapshot {
    /// Stable contributor name; each contributor appears at most once.
    pub contributor: String,
    /// Measurement evidence, preserving unknown separately from zero.
    pub measurement: RequestContributorMeasurement,
    /// Bounded diagnostic reasons from counters that declined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<String>,
}

/// Durable measurement evidence without a dependency on an inference adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestContributorMeasurement {
    /// Provider measured tokens.
    Exact {
        /// Measured token count.
        tokens: u64,
    },
    /// Explicitly approximate tokens.
    Estimated {
        /// Approximate token count.
        tokens: u64,
        /// Stable estimation method.
        method: String,
    },
    /// No reliable token measurement is available.
    Uncounted {
        /// Stable reason measurement is unavailable.
        reason: String,
    },
}

/// Correctness-sensitive capacity/catalog evidence for one request id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContextSnapshot {
    /// Measured contributors for this request, absent in legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributors: Option<Vec<RequestContributorSnapshot>>,
    /// Optional measured request budget for faithful context display on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Box<heycode_core::ContextBudget>>,
    /// Lifecycle/capability comparison instant in Unix milliseconds.
    pub effective_at_ms: u64,
    /// Combined context capacity when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Model maximum output tokens when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Exact catalog generation used, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_revision: Option<u64>,
    /// Catalog generation commit timestamp, paired with revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_fetched_at_ms: Option<u64>,
}

impl RequestContextSnapshot {
    /// Construct validated capacity/catalog context.
    ///
    /// # Errors
    /// Zero capacities/revision/timestamp or unpaired catalog evidence.
    pub fn new(
        context_window: Option<u64>,
        max_output_tokens: Option<u64>,
        catalog_revision: Option<u64>,
        catalog_fetched_at_ms: Option<u64>,
        effective_at_ms: u64,
    ) -> Result<Self, SnapshotError> {
        let snapshot = Self {
            budget: None,
            contributors: None,
            effective_at_ms,
            context_window,
            max_output_tokens,
            catalog_revision,
            catalog_fetched_at_ms,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub(crate) fn validate(&self) -> Result<(), SnapshotError> {
        if let Some(contributors) = &self.contributors {
            let mut seen = std::collections::BTreeSet::new();
            for entry in contributors {
                if !matches!(
                    entry.contributor.as_str(),
                    "system"
                        | "guidance"
                        | "messages"
                        | "tools"
                        | "tool_results"
                        | "provider_state"
                        | "attachments"
                ) || !seen.insert(&entry.contributor)
                    || entry.refusals.len() > 64
                    || entry
                        .refusals
                        .iter()
                        .any(|reason| reason.len() > 4096 || reason.chars().any(char::is_control))
                {
                    return invalid(
                        "contributors",
                        "invalid or duplicate contributor diagnostics",
                    );
                }
                match &entry.measurement {
                    RequestContributorMeasurement::Estimated { method, .. }
                        if !matches!(method.as_str(), "utf8_byte_ratio" | "provider_tokenizer") =>
                    {
                        return invalid("contributors", "unknown estimation method");
                    }
                    RequestContributorMeasurement::Uncounted { reason }
                        if !matches!(
                            reason.as_str(),
                            "no_counter" | "refused" | "failed" | "unmeasurable"
                        ) =>
                    {
                        return invalid("contributors", "unknown uncounted reason");
                    }
                    _ => {}
                }
            }
            if ![
                "system",
                "messages",
                "tools",
                "tool_results",
                "provider_state",
                "attachments",
            ]
            .iter()
            .all(|name| seen.contains(&name.to_string()))
            {
                return invalid("contributors", "complete contributor inventory is required");
            }
        }
        if let Some(budget) = &self.budget
            && (budget.window == Some(0)
                || budget.model.trim().is_empty()
                || budget.provider.trim().is_empty()
                || budget
                    .window
                    .zip(self.context_window)
                    .is_some_and(|(effective, model)| effective > model))
        {
            return invalid(
                "budget",
                "budget must name a route and respect the model capacity",
            );
        }
        if self.effective_at_ms == 0 {
            return invalid("effective_at_ms", "comparison instant must be positive");
        }
        if self.context_window == Some(0) {
            return invalid("context_window", "context capacity must be positive");
        }
        if self.max_output_tokens == Some(0) {
            return invalid("max_output_tokens", "model output maximum must be positive");
        }
        if self.catalog_revision.is_some() != self.catalog_fetched_at_ms.is_some() {
            return invalid(
                "catalog_revision",
                "catalog revision and fetched timestamp must be present together",
            );
        }
        if self.catalog_revision == Some(0) {
            return invalid("catalog_revision", "catalog revision must be positive");
        }
        if self.catalog_fetched_at_ms == Some(0) {
            return invalid(
                "catalog_fetched_at_ms",
                "catalog timestamp must be positive",
            );
        }
        Ok(())
    }
}

fn prompt_sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn unique_non_blank(field: &'static str, values: &[String]) -> Result<(), SnapshotError> {
    let mut seen = BTreeSet::new();
    for value in values {
        non_blank(field, value)?;
        if !seen.insert(value.as_str()) {
            return invalid(field, "values must be unique");
        }
    }
    Ok(())
}

fn non_blank(field: &'static str, value: &str) -> Result<(), SnapshotError> {
    if value.is_empty() || value.trim() != value {
        invalid(
            field,
            "value must be non-blank with no surrounding whitespace",
        )
    } else {
        Ok(())
    }
}

fn invalid<T>(field: &'static str, message: impl Into<String>) -> Result<T, SnapshotError> {
    Err(SnapshotError::InvalidField {
        field,
        message: message.into(),
    })
}
