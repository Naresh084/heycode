//! Provider-owned Amazon Bedrock Runtime request metadata.
//!
//! PAWS06 defines the safe, durable request intent that P07's shared Converse
//! adapter serializes. Keeping construction in the provider crate means the
//! shared protocol layer validates the wire without inventing AWS policy:
//!
//! - cache checkpoints are ordered as AWS processes them (`tools`, `system`,
//!   `messages`) and enforce the documented one-hour-before-five-minute rule;
//! - guardrail identifiers, versions, trace and streaming modes are validated
//!   against the ConverseStream API vocabulary;
//! - the source region and target kind distinguish foundation, geographic or
//!   global cross-Region, and application inference-profile routes without
//!   guessing destination Regions.
//!
//! [`BedrockRuntimeRequestMetadata::to_provider_option`] emits one
//! schema-v1 `bedrock/runtime-metadata` option. It is secret-free and redacted
//! in ordinary Debug through [`heycode_core::ProviderRequestOption`]. Agent invokes
//! the request-specific option hook after selected-model/native-route
//! resolution and before durable header admission; the shared Converse adapter
//! independently validates and serializes the same option. Root still has to
//! select exactly one AWS inference plugin and own user-facing policy settings.
//! Explicit cache intent additionally requires the concrete selected
//! [`heycode_llm::ModelDescriptor`] to publish `prompt_cache=Supported`;
//! Unsupported and Unknown fail distinctly before the option exists.
//!
//! Sources:
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_GuardrailStreamConfiguration.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-use.html>
//! <https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-support.html>

use std::collections::BTreeSet;

use heycode_authorization_aws::AwsRegion;
use heycode_core::ProviderRequestOption;
use heycode_llm::{CapabilitySupport, ModelDescriptor};

use crate::BEDROCK_PROVIDER;
use crate::model::BedrockModelId;

/// Stable evidence/failure class while resolving Bedrock request metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockMetadataErrorClass {
    /// Boundary data is structurally invalid.
    Invalid,
    /// Current model evidence explicitly denies the requested feature.
    Unsupported,
    /// Current model evidence does not prove the requested feature.
    Unproven,
}

/// Safe failure while building Bedrock request metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BedrockMetadataError {
    class: BedrockMetadataErrorClass,
    field: &'static str,
    message: &'static str,
}

impl BedrockMetadataError {
    const fn new(field: &'static str, message: &'static str) -> Self {
        Self {
            class: BedrockMetadataErrorClass::Invalid,
            field,
            message,
        }
    }

    const fn unsupported(field: &'static str, message: &'static str) -> Self {
        Self {
            class: BedrockMetadataErrorClass::Unsupported,
            field,
            message,
        }
    }

    const fn unproven(field: &'static str, message: &'static str) -> Self {
        Self {
            class: BedrockMetadataErrorClass::Unproven,
            field,
            message,
        }
    }

    /// Stable field that failed validation.
    #[must_use]
    pub const fn field(self) -> &'static str {
        self.field
    }

    /// Stable evidence/failure class.
    #[must_use]
    pub const fn class(self) -> BedrockMetadataErrorClass {
        self.class
    }
}

impl std::fmt::Display for BedrockMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.class {
            BedrockMetadataErrorClass::Invalid => write!(
                formatter,
                "invalid Amazon Bedrock metadata field `{}`: {}",
                self.field, self.message
            ),
            BedrockMetadataErrorClass::Unsupported => write!(
                formatter,
                "Amazon Bedrock metadata field `{}` does not support this request: {}",
                self.field, self.message
            ),
            BedrockMetadataErrorClass::Unproven => write!(
                formatter,
                "Amazon Bedrock metadata field `{}` is unproven: {}",
                self.field, self.message
            ),
        }
    }
}

impl std::error::Error for BedrockMetadataError {}

/// Converse content plane after which one cache checkpoint is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BedrockCachePlacement {
    /// After the complete `toolConfig.tools` list.
    Tools,
    /// After the complete `system` content list.
    System,
    /// At the end of the latest user message's content list.
    LatestUserMessage,
}

impl BedrockCachePlacement {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::System => "system",
            Self::LatestUserMessage => "latest_user_message",
        }
    }
}

/// TTL encoded in a Converse `cachePoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockCacheTtl {
    /// Omit `ttl`, selecting the documented five-minute default.
    DefaultFiveMinutes,
    /// Explicit `ttl: "1h"`; callers still need model evidence for support.
    OneHour,
}

/// One validated high-level Converse cache checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BedrockCachePoint {
    placement: BedrockCachePlacement,
    ttl: BedrockCacheTtl,
}

impl BedrockCachePoint {
    /// Construct a checkpoint; cross-checkpoint rules are enforced by
    /// [`BedrockPromptCacheConfig::new`].
    #[must_use]
    pub const fn new(placement: BedrockCachePlacement, ttl: BedrockCacheTtl) -> Self {
        Self { placement, ttl }
    }

    /// Content plane receiving this checkpoint.
    #[must_use]
    pub const fn placement(self) -> BedrockCachePlacement {
        self.placement
    }

    /// Requested cache TTL.
    #[must_use]
    pub const fn ttl(self) -> BedrockCacheTtl {
        self.ttl
    }

    fn wire_value(self) -> serde_json::Value {
        let mut checkpoint = serde_json::json!({"type":"default"});
        if self.ttl == BedrockCacheTtl::OneHour {
            checkpoint["ttl"] = serde_json::json!("1h");
        }
        serde_json::json!({
            "placement":self.placement.wire_name(),
            "cachePoint":checkpoint,
        })
    }
}

/// Exact model-specific Converse prompt-cache evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockPromptCacheCapabilities {
    model_id: String,
    placements: BTreeSet<BedrockCachePlacement>,
    max_checkpoints: usize,
    one_hour_ttl: CapabilitySupport,
}

impl BedrockPromptCacheCapabilities {
    /// Bind placement/count/one-hour evidence to one canonical model target.
    ///
    /// # Errors
    /// Unsafe model ids, duplicate/empty placement sets and a zero or
    /// excessive checkpoint maximum are refused.
    pub fn new(
        model_id: impl Into<String>,
        placements: Vec<BedrockCachePlacement>,
        max_checkpoints: usize,
        one_hour_ttl: CapabilitySupport,
    ) -> Result<Self, BedrockMetadataError> {
        let model_id = model_id.into();
        if !valid_runtime_target(&model_id) {
            return Err(BedrockMetadataError::new(
                "model_id",
                "cache evidence target is not a valid Converse runtime target",
            ));
        }
        let unique = placements.iter().copied().collect::<BTreeSet<_>>();
        if unique.is_empty() || unique.len() != placements.len() {
            return Err(BedrockMetadataError::new(
                "cache_placements",
                "one or more unique content planes are required",
            ));
        }
        if !(1..=4).contains(&max_checkpoints) {
            return Err(BedrockMetadataError::new(
                "cache_max_checkpoints",
                "checkpoint maximum must be between one and four",
            ));
        }
        Ok(Self {
            model_id,
            placements: unique,
            max_checkpoints,
            one_hour_ttl,
        })
    }

    /// Canonical model target this evidence belongs to.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Whether the model accepts a checkpoint on this plane.
    #[must_use]
    pub fn supports_placement(&self, placement: BedrockCachePlacement) -> bool {
        self.placements.contains(&placement)
    }

    /// Exact per-request checkpoint maximum.
    #[must_use]
    pub const fn max_checkpoints(&self) -> usize {
        self.max_checkpoints
    }

    /// Evidence for the optional one-hour TTL.
    #[must_use]
    pub const fn one_hour_ttl(&self) -> CapabilitySupport {
        self.one_hour_ttl
    }
}

/// Explicit Converse prompt-cache checkpoints for one evidenced model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockPromptCacheConfig {
    points: Vec<BedrockCachePoint>,
    capabilities: BedrockPromptCacheCapabilities,
}

impl BedrockPromptCacheConfig {
    /// Validate, canonicalize and order one to three checkpoints.
    ///
    /// heycode currently exposes one stable breakpoint per representable content
    /// plane. AWS permits model-specific counts (including multiple message
    /// checkpoints); supporting those needs a richer durable message selector,
    /// not an ambiguous duplicate placement.
    ///
    /// # Errors
    /// Empty/duplicate placements, model-evidence placement/count/TTL gaps,
    /// or a one-hour checkpoint that AWS would process after a five-minute
    /// checkpoint are refused.
    pub fn new(
        mut points: Vec<BedrockCachePoint>,
        capabilities: BedrockPromptCacheCapabilities,
    ) -> Result<Self, BedrockMetadataError> {
        if points.is_empty() || points.len() > 3 {
            return Err(BedrockMetadataError::new(
                "cache_points",
                "one to three checkpoints are required",
            ));
        }
        if points.len() > capabilities.max_checkpoints {
            return Err(BedrockMetadataError::unsupported(
                "cache_points",
                "requested checkpoints exceed selected-model evidence",
            ));
        }
        points.sort_by_key(|point| point.placement);
        let mut placements = BTreeSet::new();
        let mut saw_default_five_minutes = false;
        for point in &points {
            if !placements.insert(point.placement) {
                return Err(BedrockMetadataError::new(
                    "cache_points",
                    "each content plane may appear at most once",
                ));
            }
            if !capabilities.supports_placement(point.placement) {
                return Err(BedrockMetadataError::unsupported(
                    "cache_placements",
                    "selected-model evidence denies the requested content plane",
                ));
            }
            match point.ttl {
                BedrockCacheTtl::DefaultFiveMinutes => saw_default_five_minutes = true,
                BedrockCacheTtl::OneHour
                    if capabilities.one_hour_ttl == CapabilitySupport::Unsupported =>
                {
                    return Err(BedrockMetadataError::unsupported(
                        "cache_ttl",
                        "selected-model evidence denies the one-hour TTL",
                    ));
                }
                BedrockCacheTtl::OneHour
                    if capabilities.one_hour_ttl == CapabilitySupport::Unknown =>
                {
                    return Err(BedrockMetadataError::unproven(
                        "cache_ttl",
                        "selected-model evidence does not prove the one-hour TTL",
                    ));
                }
                BedrockCacheTtl::OneHour if saw_default_five_minutes => {
                    return Err(BedrockMetadataError::new(
                        "cache_points",
                        "one-hour checkpoints must precede five-minute checkpoints",
                    ));
                }
                BedrockCacheTtl::OneHour => {}
            }
        }
        Ok(Self {
            points,
            capabilities,
        })
    }

    /// Canonical AWS processing order: tools, system, messages.
    #[must_use]
    pub fn points(&self) -> &[BedrockCachePoint] {
        &self.points
    }

    /// Exact model-specific capability evidence used at construction.
    #[must_use]
    pub const fn capabilities(&self) -> &BedrockPromptCacheCapabilities {
        &self.capabilities
    }

    fn wire_value(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.points
                .iter()
                .copied()
                .map(BedrockCachePoint::wire_value)
                .collect(),
        )
    }
}

/// ConverseStream guardrail trace behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockGuardrailTrace {
    /// `enabled`.
    Enabled,
    /// `disabled`.
    Disabled,
    /// `enabled_full`.
    EnabledFull,
}

impl BedrockGuardrailTrace {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::EnabledFull => "enabled_full",
        }
    }
}

/// ConverseStream guardrail processing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockGuardrailStreamMode {
    /// `sync`: assess chunks before returning them.
    Sync,
    /// `async`: stream while assessment continues; sensitive-information
    /// masking is not supported by AWS in this mode.
    Async,
}

impl BedrockGuardrailStreamMode {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }
}

/// Guardrail applied to one ConverseStream request.
#[derive(Clone, PartialEq, Eq)]
pub struct BedrockGuardrailConfig {
    identifier: String,
    version: String,
    trace: Option<BedrockGuardrailTrace>,
    stream_mode: Option<BedrockGuardrailStreamMode>,
}

impl BedrockGuardrailConfig {
    /// Validate the documented guardrail identifier and version patterns.
    ///
    /// # Errors
    /// Unsafe/unsupported identifiers or versions are refused without echoing
    /// the rejected value.
    pub fn new(
        identifier: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, BedrockMetadataError> {
        let identifier = identifier.into();
        let version = version.into();
        if !valid_guardrail_identifier(&identifier) {
            return Err(BedrockMetadataError::new(
                "guardrail_identifier",
                "identifier does not match the ConverseStream pattern",
            ));
        }
        if !valid_guardrail_version(&version) {
            return Err(BedrockMetadataError::new(
                "guardrail_version",
                "version must be DRAFT or an integer from 1 to 99999999",
            ));
        }
        Ok(Self {
            identifier,
            version,
            trace: None,
            stream_mode: None,
        })
    }

    /// Attach exact trace behavior.
    #[must_use]
    pub const fn with_trace(mut self, trace: BedrockGuardrailTrace) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Attach exact streaming assessment behavior.
    #[must_use]
    pub const fn with_stream_mode(mut self, mode: BedrockGuardrailStreamMode) -> Self {
        self.stream_mode = Some(mode);
        self
    }

    /// Validated guardrail id or ARN.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Validated guardrail version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Optional trace behavior.
    #[must_use]
    pub const fn trace(&self) -> Option<BedrockGuardrailTrace> {
        self.trace
    }

    /// Optional stream-processing mode.
    #[must_use]
    pub const fn stream_mode(&self) -> Option<BedrockGuardrailStreamMode> {
        self.stream_mode
    }

    fn wire_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "guardrailIdentifier":self.identifier,
            "guardrailVersion":self.version,
        });
        if let Some(trace) = self.trace {
            value["trace"] = serde_json::json!(trace.wire_name());
        }
        if let Some(mode) = self.stream_mode {
            value["streamProcessingMode"] = serde_json::json!(mode.wire_name());
        }
        value
    }
}

impl std::fmt::Debug for BedrockGuardrailConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BedrockGuardrailConfig")
            .field("identifier", &"[REDACTED]")
            .field("version", &self.version)
            .field("trace", &self.trace)
            .field("stream_mode", &self.stream_mode)
            .finish()
    }
}

fn valid_guardrail_identifier(value: &str) -> bool {
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        return false;
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return true;
    }
    let mut fields = value.splitn(6, ':');
    fields.next() == Some("arn")
        && fields.next().is_some_and(valid_aws_partition)
        && fields.next() == Some("bedrock")
        && fields.next().is_some_and(valid_arn_region)
        && fields.next().is_some_and(valid_account_id)
        && fields.next().is_some_and(|resource| {
            resource.strip_prefix("guardrail/").is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        })
}

fn valid_aws_partition(value: &str) -> bool {
    value == "aws"
        || value.strip_prefix("aws-").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_arn_region(value: &str) -> bool {
    (1..=20).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_account_id(value: &str) -> bool {
    value.len() == 12 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_guardrail_version(value: &str) -> bool {
    value == "DRAFT"
        || ((1..=8).contains(&value.len())
            && value.as_bytes().first().is_some_and(|byte| *byte != b'0')
            && value.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Kind of resource placed in ConverseStream's `modelId` URI label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockInferenceTargetKind {
    /// Foundation model id or ARN.
    FoundationModel,
    /// System-defined geographic/global cross-Region inference profile.
    CrossRegionInferenceProfile,
    /// Account-owned application inference profile used for attribution.
    ApplicationInferenceProfile,
    /// Another documented Converse target class (custom/provisioned/prompt,
    /// marketplace or endpoint). No narrower claim is made.
    Other,
}

impl BedrockInferenceTargetKind {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::FoundationModel => "foundation_model",
            Self::CrossRegionInferenceProfile => "cross_region_inference_profile",
            Self::ApplicationInferenceProfile => "application_inference_profile",
            Self::Other => "other",
        }
    }
}

/// Residency scope of a system-defined cross-Region inference profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockCrossRegionScope {
    /// Geographic profile such as `us.`, `eu.`, `apac.` or `us-gov.`.
    Geographic,
    /// `global.` profile; destination membership may change over time.
    Global,
}

impl BedrockCrossRegionScope {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Geographic => "geographic",
            Self::Global => "global",
        }
    }
}

/// Secret-free route facts for one concrete request target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockRouteMetadata {
    source_region: AwsRegion,
    target_kind: BedrockInferenceTargetKind,
    cross_region_scope: Option<BedrockCrossRegionScope>,
}

impl BedrockRouteMetadata {
    /// Classify a validated Converse target without retaining or duplicating
    /// it in the option; the canonical model id is already in the request
    /// header snapshot.
    ///
    /// # Errors
    /// Empty, oversized, control-bearing or URI-unsafe target values fail.
    pub fn new(source_region: &AwsRegion, target: &str) -> Result<Self, BedrockMetadataError> {
        if !valid_runtime_target(target) {
            return Err(BedrockMetadataError::new(
                "model_id",
                "target is empty, oversized or unsafe for a runtime URI label",
            ));
        }
        let (target_kind, cross_region_scope) = classify_runtime_target(target);
        Ok(Self {
            source_region: source_region.clone(),
            target_kind,
            cross_region_scope,
        })
    }

    /// Region whose runtime endpoint receives the request.
    #[must_use]
    pub const fn source_region(&self) -> &AwsRegion {
        &self.source_region
    }

    /// Classified target resource.
    #[must_use]
    pub const fn target_kind(&self) -> BedrockInferenceTargetKind {
        self.target_kind
    }

    /// Geographic/global scope for a system-defined cross-Region profile.
    #[must_use]
    pub const fn cross_region_scope(&self) -> Option<BedrockCrossRegionScope> {
        self.cross_region_scope
    }

    fn wire_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "source_region":self.source_region.as_str(),
            "target_kind":self.target_kind.wire_name(),
        });
        if let Some(scope) = self.cross_region_scope {
            value["cross_region_scope"] = serde_json::json!(scope.wire_name());
        }
        value
    }
}

fn valid_runtime_target(value: &str) -> bool {
    if !(1..=2048).contains(&value.len())
        || value.chars().any(char::is_control)
        || value.contains("..")
    {
        return false;
    }
    if value.starts_with("arn:") {
        return valid_bedrock_runtime_arn(value) || valid_sagemaker_endpoint_arn(value);
    }
    !value.contains('/')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b':'))
}

fn valid_bedrock_runtime_arn(value: &str) -> bool {
    let mut fields = value.splitn(6, ':');
    if fields.next() != Some("arn")
        || !fields.next().is_some_and(valid_aws_partition)
        || fields.next() != Some("bedrock")
        || !fields.next().is_some_and(valid_arn_region)
    {
        return false;
    }
    let Some(account) = fields.next() else {
        return false;
    };
    let Some(resource) = fields.next() else {
        return false;
    };
    if let Some(model) = resource.strip_prefix("foundation-model/") {
        return account.is_empty() && BedrockModelId::new(model).is_some();
    }
    if !valid_account_id(account) {
        return false;
    }
    if let Some(custom) = resource.strip_prefix("custom-model/") {
        return valid_custom_model_resource(custom);
    }
    for prefix in [
        "imported-model/",
        "provisioned-model/",
        "custom-model-deployment/",
    ] {
        if let Some(id) = resource.strip_prefix(prefix) {
            return valid_fixed_resource_id(id);
        }
    }
    for prefix in ["inference-profile/", "application-inference-profile/"] {
        if let Some(id) = resource.strip_prefix(prefix) {
            return valid_profile_resource_id(id);
        }
    }
    if let Some(prompt) = resource.strip_prefix("prompt/") {
        return valid_prompt_resource(prompt);
    }
    ["prompt-router/", "default-prompt-router/"]
        .iter()
        .find_map(|prefix| resource.strip_prefix(prefix))
        .is_some_and(valid_profile_resource_id)
}

fn valid_sagemaker_endpoint_arn(value: &str) -> bool {
    let mut fields = value.splitn(6, ':');
    fields.next() == Some("arn")
        && fields.next() == Some("aws")
        && fields.next() == Some("sagemaker")
        && fields.next().is_some_and(valid_arn_region)
        && fields.next().is_some_and(valid_account_id)
        && fields.next().is_some_and(|resource| {
            resource.strip_prefix("endpoint/").is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        })
}

fn valid_custom_model_resource(value: &str) -> bool {
    value.split_once('/').is_some_and(|(model, id)| {
        BedrockModelId::new(model).is_some() && valid_fixed_resource_id(id)
    })
}

fn valid_fixed_resource_id(value: &str) -> bool {
    value.len() == 12
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn valid_profile_resource_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':' | b'.'))
}

fn valid_prompt_resource(value: &str) -> bool {
    let (id, version) = value
        .split_once(':')
        .map_or((value, None), |(id, version)| (id, Some(version)));
    id.len() == 10
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && version.is_none_or(|version| {
            (1..=5).contains(&version.len()) && version.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn classify_runtime_target(
    value: &str,
) -> (BedrockInferenceTargetKind, Option<BedrockCrossRegionScope>) {
    if let Some(scope) = cross_region_scope(value) {
        return (
            BedrockInferenceTargetKind::CrossRegionInferenceProfile,
            Some(scope),
        );
    }
    if arn_resource(value, "application-inference-profile/").is_some() {
        return (
            BedrockInferenceTargetKind::ApplicationInferenceProfile,
            None,
        );
    }
    if let Some(resource) = arn_resource(value, "inference-profile/") {
        if let Some(scope) = cross_region_scope(resource) {
            return (
                BedrockInferenceTargetKind::CrossRegionInferenceProfile,
                Some(scope),
            );
        }
        return (
            BedrockInferenceTargetKind::ApplicationInferenceProfile,
            None,
        );
    }
    if value.contains(":foundation-model/") || BedrockModelId::new(value).is_some() {
        return (BedrockInferenceTargetKind::FoundationModel, None);
    }
    (BedrockInferenceTargetKind::Other, None)
}

fn arn_resource<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    value
        .strip_prefix("arn:")
        .and_then(|_| value.split_once(marker))
        .map(|(_, resource)| resource)
        .filter(|resource| !resource.is_empty())
}

fn cross_region_scope(value: &str) -> Option<BedrockCrossRegionScope> {
    if value.starts_with("global.") {
        return Some(BedrockCrossRegionScope::Global);
    }
    ["us.", "eu.", "apac.", "us-gov."]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        .then_some(BedrockCrossRegionScope::Geographic)
}

/// Optional cache/guardrail intent plus route facts for one Converse request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockRuntimeRequestMetadata {
    prompt_cache: Option<BedrockPromptCacheConfig>,
    guardrail: Option<BedrockGuardrailConfig>,
}

impl BedrockRuntimeRequestMetadata {
    /// Construct provider-owned request metadata. Child values are already
    /// validated by their constructors.
    #[must_use]
    pub const fn new(
        prompt_cache: Option<BedrockPromptCacheConfig>,
        guardrail: Option<BedrockGuardrailConfig>,
    ) -> Self {
        Self {
            prompt_cache,
            guardrail,
        }
    }

    /// Configured explicit cache checkpoints.
    #[must_use]
    pub const fn prompt_cache(&self) -> Option<&BedrockPromptCacheConfig> {
        self.prompt_cache.as_ref()
    }

    /// Configured request guardrail.
    #[must_use]
    pub const fn guardrail(&self) -> Option<&BedrockGuardrailConfig> {
        self.guardrail.as_ref()
    }

    /// Build the durable schema-v1 `bedrock/runtime-metadata` option for one
    /// concrete source-region/model pair.
    ///
    /// # Errors
    /// Invalid target identity, Unsupported/Unknown cache evidence or an
    /// unexpected generic option-boundary refusal fails before durable
    /// request admission.
    pub fn to_provider_option(
        &self,
        source_region: &AwsRegion,
        model: &ModelDescriptor,
    ) -> Result<ProviderRequestOption, BedrockMetadataError> {
        if self.prompt_cache.is_some() {
            match model.capabilities.prompt_cache {
                CapabilitySupport::Supported => {}
                CapabilitySupport::Unsupported => {
                    return Err(BedrockMetadataError::unsupported(
                        "prompt_cache",
                        "selected model evidence explicitly denies prompt caching",
                    ));
                }
                CapabilitySupport::Unknown => {
                    return Err(BedrockMetadataError::unproven(
                        "prompt_cache",
                        "selected model evidence does not prove prompt caching",
                    ));
                }
            }
        }
        if self
            .prompt_cache
            .as_ref()
            .is_some_and(|cache| cache.capabilities.model_id != model.id)
        {
            return Err(BedrockMetadataError::unproven(
                "prompt_cache",
                "cache capability evidence belongs to a different selected model",
            ));
        }
        let route = BedrockRouteMetadata::new(source_region, &model.id)?;
        let mut data = serde_json::json!({"route":route.wire_value()});
        if let Some(prompt_cache) = &self.prompt_cache {
            data["cache_points"] = prompt_cache.wire_value();
        }
        if let Some(guardrail) = &self.guardrail {
            data["guardrailConfig"] = guardrail.wire_value();
        }
        ProviderRequestOption::new(BEDROCK_PROVIDER, "runtime-metadata", data).map_err(|_| {
            BedrockMetadataError::new(
                "provider_option",
                "validated runtime metadata could not enter the generic option boundary",
            )
        })
    }
}
