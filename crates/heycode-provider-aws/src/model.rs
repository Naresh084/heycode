//! Retained Amazon Bedrock foundation-model metadata.
//!
//! `ListFoundationModels` publishes four facts about a model that the CAT02
//! [`ModelDescriptor`] vocabulary has no field for — the lifecycle phase and
//! its instants, the two modality lists, whether the response may be streamed,
//! and which inference types the account may call the model through. Those
//! facts decide whether a model is usable, so normalization keeps them on
//! [`BedrockFoundationModel`] next to the descriptor it derives, rather than
//! discarding whatever CAT02 cannot name.
//!
//! Every wire constant below cites the AWS documentation it came from.

use heycode_llm::{CapabilitySupport, ModelDescriptor};

/// Documented maximum `modelId` length.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
const MAX_MODEL_ID_BYTES: usize = 140;

/// Documented `BrandedName` maximum, shared by `modelName` and `providerName`.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
const MAX_BRANDED_NAME_BYTES: usize = 20;

/// Cap on a `modelArn`. The documented ARN pattern cannot produce anything
/// close to this; the bound exists so a hostile response cannot retain an
/// unbounded string.
const MAX_MODEL_ARN_BYTES: usize = 512;

/// Validated Amazon Bedrock foundation-model id.
///
/// The id is request-facing: it becomes a path segment of a Bedrock runtime
/// URL and a field of a signed request, so its character set is closed here
/// once instead of at each call site.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BedrockModelId(String);

impl BedrockModelId {
    /// Validate one model id.
    ///
    /// Returns `None` for anything outside the documented pattern's alphabet
    /// and length, and for the shapes that would change the meaning of a URL
    /// the id is spliced into: a leading or trailing separator, or a `..`
    /// path-traversal sequence.
    ///
    /// The documented pattern is
    /// `[a-z0-9-]{1,63}[.]{1}[a-z0-9-]{1,63}([a-z0-9-]{1,63}[.]){0,2}[a-z0-9-]{1,63}([:][a-z0-9-]{1,63}){0,2}(/[a-z0-9]{12}|)`
    /// with a maximum length of 140.
    ///
    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
    #[must_use]
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        let bytes = value.as_bytes();
        let alphabet = bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || is_id_separator(*byte)
        });
        let bounded = (1..=MAX_MODEL_ID_BYTES).contains(&bytes.len());
        let anchored = bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric);
        // The documented pattern always separates a provider prefix from a
        // model family with one `.`, so an id without one is not a Bedrock id.
        let qualified = value.contains('.');
        // `.` and `/` are both inside the alphabet, so traversal has to be
        // excluded explicitly rather than assumed away by the charset.
        let traversal = value.contains("..");
        (alphabet && bounded && anchored && qualified && !traversal).then_some(Self(value))
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const fn is_id_separator(byte: u8) -> bool {
    matches!(byte, b'-' | b'.' | b':' | b'/')
}

/// Validate one `modelArn` against the documented ARN pattern.
///
/// The pattern is
/// `arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}::foundation-model/...`, so the
/// partition may be suffixed, the account-id field is always empty and the
/// resource is always a `foundation-model/` path. The trailing model portion
/// is not re-derived from `modelId`: the two documented patterns differ, and
/// inventing an equality the API never promised would reject valid rows.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
#[must_use]
pub(crate) fn valid_model_arn(value: &str) -> bool {
    if value.len() > MAX_MODEL_ARN_BYTES || value.chars().any(char::is_control) {
        return false;
    }
    // The resource segment legitimately contains `:` (a model version such as
    // `…-v2:0`), so only the five leading fields are split apart.
    let mut fields = value.splitn(6, ':');
    let arn = fields.next() == Some("arn");
    let partition = fields
        .next()
        .is_some_and(|field| field == "aws" || field.starts_with("aws-"));
    let service = fields.next() == Some("bedrock");
    let region = fields.next().is_some_and(|field| {
        (1..=20).contains(&field.len())
            && field
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    let account = fields.next() == Some("");
    let resource = fields.next().is_some_and(|field| {
        field
            .strip_prefix("foundation-model/")
            .is_some_and(|model| !model.is_empty())
    });
    arn && partition && service && region && account && resource
}

/// Normalize one `BrandedName` for publication.
///
/// Returns `None` when the value cannot be published as display metadata:
/// empty after trimming, longer than the documented maximum, or carrying a
/// control character. Trailing whitespace alone is trimmed rather than
/// rejected (GOTCHAS #116).
#[must_use]
pub(crate) fn branded_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let publishable = !trimmed.is_empty()
        && trimmed.len() <= MAX_BRANDED_NAME_BYTES
        && !trimmed.chars().any(char::is_control);
    publishable.then(|| trimmed.to_owned())
}

/// One documented Amazon Bedrock modality.
///
/// Valid values are `TEXT | IMAGE | EMBEDDING`.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BedrockModality {
    /// `TEXT`.
    Text,
    /// `IMAGE`.
    Image,
    /// `EMBEDDING`.
    Embedding,
}

impl BedrockModality {
    /// Parse one documented modality member.
    ///
    /// Returns `None` for any value outside the documented enumeration.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "TEXT" => Some(Self::Text),
            "IMAGE" => Some(Self::Image),
            "EMBEDDING" => Some(Self::Embedding),
            _ => None,
        }
    }

    /// Exact wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Image => "IMAGE",
            Self::Embedding => "EMBEDDING",
        }
    }
}

/// One documented Amazon Bedrock inference type.
///
/// Valid values are `ON_DEMAND | PROVISIONED`. The distinction decides whether
/// the account can call the model at all: a model without on-demand support
/// requires a provisioned-throughput purchase before any request succeeds.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BedrockInferenceType {
    /// `ON_DEMAND`.
    OnDemand,
    /// `PROVISIONED`.
    Provisioned,
}

impl BedrockInferenceType {
    /// Parse one documented inference-type member.
    ///
    /// Returns `None` for any value outside the documented enumeration.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ON_DEMAND" => Some(Self::OnDemand),
            "PROVISIONED" => Some(Self::Provisioned),
            _ => None,
        }
    }

    /// Exact wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnDemand => "ON_DEMAND",
            Self::Provisioned => "PROVISIONED",
        }
    }
}

/// Provider-published lifecycle phase of one model version.
///
/// Documented valid values are `ACTIVE | LEGACY`. A phase this normalizer does
/// not recognize stays [`BedrockLifecycleStatus::Unknown`]: the phase is a
/// single value whose vocabulary can say "not established", so a new AWS phase
/// is recorded as unproven instead of either failing the whole generation or
/// being promoted to a phase AWS did not publish.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelLifecycle.html>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BedrockLifecycleStatus {
    /// `ACTIVE` — the model version is available.
    Active,
    /// `LEGACY` — deprecated, still callable until end of life.
    Legacy,
    /// No lifecycle object was published, or its status is outside the
    /// documented enumeration.
    Unknown,
}

impl BedrockLifecycleStatus {
    /// Parse one documented lifecycle status.
    ///
    /// An unrecognized status is [`Self::Unknown`], never a phase.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "ACTIVE" => Self::Active,
            "LEGACY" => Self::Legacy,
            _ => Self::Unknown,
        }
    }
}

/// Retained Amazon Bedrock lifecycle evidence.
///
/// All four instants are optional in the API and are held in Unix
/// milliseconds. `None` means the instant was not published, never "now" and
/// never "never".
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelLifecycle.html>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BedrockLifecycle {
    /// Published phase, or [`BedrockLifecycleStatus::Unknown`].
    pub status: BedrockLifecycleStatus,
    /// `startOfLifeTime` — launch instant.
    pub start_of_life_at_ms: Option<u64>,
    /// `legacyTime` — instant the model enters the legacy phase.
    pub legacy_at_ms: Option<u64>,
    /// `publicExtendedAccessTime` — instant higher legacy pricing begins.
    pub public_extended_access_at_ms: Option<u64>,
    /// `endOfLifeTime` — instant the model stops being available.
    pub end_of_life_at_ms: Option<u64>,
}

impl BedrockLifecycle {
    /// Lifecycle evidence for a row that published no lifecycle object.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            status: BedrockLifecycleStatus::Unknown,
            start_of_life_at_ms: None,
            legacy_at_ms: None,
            public_extended_access_at_ms: None,
            end_of_life_at_ms: None,
        }
    }
}

/// One normalized Amazon Bedrock foundation model.
///
/// The row keeps both halves of normalization: the CAT02 [`ModelDescriptor`]
/// the shared registry consumes, and the Bedrock facts that vocabulary cannot
/// express. Nothing here is provider-secret; the type is safe to log.
/// Fields are crate-private: only [`crate::discovery`] may build a row, and
/// only after every value on it has passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedrockFoundationModel {
    pub(crate) id: BedrockModelId,
    pub(crate) arn: String,
    pub(crate) provider_name: Option<String>,
    pub(crate) descriptor: ModelDescriptor,
    pub(crate) input_modalities: Option<Vec<BedrockModality>>,
    pub(crate) output_modalities: Option<Vec<BedrockModality>>,
    pub(crate) response_streaming: CapabilitySupport,
    pub(crate) inference_types: Option<Vec<BedrockInferenceType>>,
    pub(crate) lifecycle: BedrockLifecycle,
}

impl BedrockFoundationModel {
    /// Validated model id.
    #[must_use]
    pub const fn id(&self) -> &BedrockModelId {
        &self.id
    }

    /// Validated model ARN. It names the partition and region the row was
    /// listed from, which the model id alone does not.
    #[must_use]
    pub fn arn(&self) -> &str {
        &self.arn
    }

    /// Published `providerName`, when the row carried a publishable one.
    #[must_use]
    pub fn provider_name(&self) -> Option<&str> {
        self.provider_name.as_deref()
    }

    /// CAT02 projection of this row.
    #[must_use]
    pub const fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }

    /// Take the CAT02 projection, dropping the Bedrock-only facts.
    #[must_use]
    pub fn into_descriptor(self) -> ModelDescriptor {
        self.descriptor
    }

    /// Published input modalities. `None` means the list was not published,
    /// which is a different fact from a published empty list.
    #[must_use]
    pub fn input_modalities(&self) -> Option<&[BedrockModality]> {
        self.input_modalities.as_deref()
    }

    /// Published output modalities, kept separate from the input list.
    /// `None` means the list was not published.
    #[must_use]
    pub fn output_modalities(&self) -> Option<&[BedrockModality]> {
        self.output_modalities.as_deref()
    }

    /// Whether `ConverseStream`-style streaming is supported. An absent
    /// `responseStreamingSupported` is [`CapabilitySupport::Unknown`], never
    /// an assumption inherited from the provider.
    #[must_use]
    pub const fn response_streaming(&self) -> CapabilitySupport {
        self.response_streaming
    }

    /// Published inference types. `None` means the list was not published.
    #[must_use]
    pub fn inference_types(&self) -> Option<&[BedrockInferenceType]> {
        self.inference_types.as_deref()
    }

    /// Whether this account can call the model without buying provisioned
    /// throughput.
    ///
    /// A published list that omits `ON_DEMAND` is explicit evidence of
    /// absence; an unpublished list is [`CapabilitySupport::Unknown`].
    #[must_use]
    pub fn on_demand(&self) -> CapabilitySupport {
        match self.inference_types.as_deref() {
            None => CapabilitySupport::Unknown,
            Some(types) if types.contains(&BedrockInferenceType::OnDemand) => {
                CapabilitySupport::Supported
            }
            Some(_) => CapabilitySupport::Unsupported,
        }
    }

    /// Retained lifecycle evidence, including every published instant.
    #[must_use]
    pub const fn lifecycle(&self) -> BedrockLifecycle {
        self.lifecycle
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_outside_the_documented_alphabet_are_refused() {
        assert!(BedrockModelId::new("anthropic.claude-sonnet-4-20250514-v1:0").is_some());
        assert!(BedrockModelId::new("amazon.titan-embed-text-v2:0").is_some());
        assert!(BedrockModelId::new("meta.llama3-70b-instruct-v1:0").is_some());
        for rejected in [
            "",
            "amazon titan",
            "Amazon.Titan",
            "amazon.titan?x=1",
            "amazon.titan#frag",
            "amazon.titan%2f",
            "amazon.titan&x",
            "no-dot-anywhere",
            ".amazon.titan",
            "amazon.titan.",
            "amazon.titan/../../secrets",
            "amazon..titan",
        ] {
            assert!(
                BedrockModelId::new(rejected).is_none(),
                "`{rejected}` must be refused"
            );
        }
    }

    #[test]
    fn model_ids_longer_than_the_documented_maximum_are_refused() {
        let stem = "a".repeat(MAX_MODEL_ID_BYTES - 2);
        assert!(BedrockModelId::new(format!("{stem}.b")).is_some());
        assert!(BedrockModelId::new(format!("{stem}a.b")).is_none());
    }

    #[test]
    fn model_arns_outside_the_documented_pattern_are_refused() {
        assert!(valid_model_arn(
            "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-20250514-v1:0"
        ));
        assert!(valid_model_arn(
            "arn:aws-us-gov:bedrock:us-gov-west-1::foundation-model/amazon.titan-text-v1"
        ));
        for rejected in [
            "",
            "not-an-arn",
            "arn:gcp:bedrock:us-east-1::foundation-model/amazon.titan-text-v1",
            "arn:aws:s3:us-east-1::foundation-model/amazon.titan-text-v1",
            "arn:aws:bedrock:us-east-1:123456789012:foundation-model/amazon.titan-text-v1",
            "arn:aws:bedrock:us-east-1::custom-model/amazon.titan-text-v1",
            "arn:aws:bedrock:us-east-1::foundation-model/",
            "arn:aws:bedrock:US-EAST-1::foundation-model/amazon.titan-text-v1",
        ] {
            assert!(!valid_model_arn(rejected), "`{rejected}` must be refused");
        }
    }

    #[test]
    fn branded_names_are_trimmed_and_control_free_within_the_documented_bound() {
        assert_eq!(
            branded_name("Claude Sonnet 4  "),
            Some("Claude Sonnet 4".to_owned())
        );
        assert_eq!(branded_name(""), None);
        assert_eq!(branded_name("   "), None);
        assert_eq!(branded_name("Claude\u{7}Sonnet"), None);
        assert!(branded_name(&"n".repeat(MAX_BRANDED_NAME_BYTES)).is_some());
        assert_eq!(branded_name(&"n".repeat(MAX_BRANDED_NAME_BYTES + 1)), None);
    }

    #[test]
    fn an_unrecognized_lifecycle_status_parses_to_unknown_rather_than_a_phase() {
        assert_eq!(
            BedrockLifecycleStatus::parse("ACTIVE"),
            BedrockLifecycleStatus::Active
        );
        assert_eq!(
            BedrockLifecycleStatus::parse("LEGACY"),
            BedrockLifecycleStatus::Legacy
        );
        for unrecognized in ["PREVIEW", "active", "RETIRED", ""] {
            assert_eq!(
                BedrockLifecycleStatus::parse(unrecognized),
                BedrockLifecycleStatus::Unknown,
                "`{unrecognized}` must not become a phase"
            );
        }
    }

    #[test]
    fn documented_enumerations_round_trip_and_reject_everything_else() {
        for modality in [
            BedrockModality::Text,
            BedrockModality::Image,
            BedrockModality::Embedding,
        ] {
            assert_eq!(BedrockModality::parse(modality.as_str()), Some(modality));
        }
        for rejected in ["text", "AUDIO", "VIDEO", ""] {
            assert_eq!(BedrockModality::parse(rejected), None);
        }
        for inference in [
            BedrockInferenceType::OnDemand,
            BedrockInferenceType::Provisioned,
        ] {
            assert_eq!(
                BedrockInferenceType::parse(inference.as_str()),
                Some(inference)
            );
        }
        for rejected in ["on_demand", "INFERENCE_PROFILE", ""] {
            assert_eq!(BedrockInferenceType::parse(rejected), None);
        }
    }
}
