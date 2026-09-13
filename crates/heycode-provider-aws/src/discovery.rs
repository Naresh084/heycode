//! `ListFoundationModels` wire shape, validation and normalization.
//!
//! The wire types below mirror the documented `ListFoundationModelsResponse`
//! exactly and carry no defaulting of their own: every optional member stays
//! `Option`, so "AWS did not publish this" survives as far as the normalized
//! row. Unknown JSON members are ignored rather than rejected — a new AWS
//! field must not take a working catalog down — while an *unrecognized value
//! inside a documented enumeration* is refused, because dropping it would
//! silently understate a retained list.
//!
//! One malformed row rejects the whole generation. A partial Bedrock catalog
//! is worse than none: an absent row reads as "this model does not exist in
//! this region", which is a claim no failed parse has established.
//!
//! Endpoint and shape references:
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_ListFoundationModels.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelSummary.html>
//! <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_FoundationModelLifecycle.html>

use std::collections::BTreeMap;

use chrono::DateTime;
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, ModelCapabilities, ModelDescriptor,
    ModelLifecycle, ModelLifecycleStatus, ModelPerformance, ModelPricing,
};
use serde::Deserialize;

use crate::model::{
    BedrockFoundationModel, BedrockInferenceType, BedrockLifecycle, BedrockLifecycleStatus,
    BedrockModality, BedrockModelId, branded_name, valid_model_arn,
};

/// Rows one generation may contain. Bedrock lists a few hundred models per
/// region; anything past this is a response we refuse rather than normalize.
const MAX_MODELS: usize = 2048;

/// Members one documented enumeration list may contain. The enumerations have
/// three values, so this only bounds a hostile response.
const MAX_ENUM_MEMBERS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListFoundationModelsResponse {
    #[serde(default)]
    model_summaries: Option<Vec<FoundationModelSummary>>,
}

/// `modelArn` and `modelId` are the only required members; everything else is
/// evidence that may be absent.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModelSummary {
    model_arn: String,
    model_id: String,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    provider_name: Option<String>,
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
    #[serde(default)]
    response_streaming_supported: Option<bool>,
    #[serde(default)]
    inference_types_supported: Option<Vec<String>>,
    #[serde(default)]
    model_lifecycle: Option<FoundationModelLifecycle>,
}

/// `status` is documented as required, so a lifecycle object without one is a
/// malformed row rather than an unknown phase.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModelLifecycle {
    status: String,
    #[serde(default)]
    start_of_life_time: Option<String>,
    #[serde(default)]
    legacy_time: Option<String>,
    #[serde(default)]
    public_extended_access_time: Option<String>,
    #[serde(default)]
    end_of_life_time: Option<String>,
}

/// Normalize one complete `ListFoundationModels` body.
///
/// # Errors
/// Returns [`CatalogFailureKind::InvalidResponse`] for a body that is not the
/// documented shape, that carries no usable rows, or that contains any row
/// this normalizer cannot represent without inventing a fact.
pub(crate) fn normalize(body: &[u8]) -> Result<Vec<BedrockFoundationModel>, CatalogFetchError> {
    let response: ListFoundationModelsResponse = serde_json::from_slice(body)
        .map_err(|_| invalid("Amazon Bedrock model list has an invalid JSON shape"))?;
    let summaries = response
        .model_summaries
        .ok_or_else(|| invalid("Amazon Bedrock model list response contains no model summaries"))?;
    if summaries.is_empty() {
        return Err(invalid("Amazon Bedrock model list is empty"));
    }
    if summaries.len() > MAX_MODELS {
        return Err(invalid("Amazon Bedrock model list is too large"));
    }
    let mut rows: BTreeMap<String, BedrockFoundationModel> = BTreeMap::new();
    for summary in summaries {
        let row = normalize_row(summary)?;
        if rows.insert(row.id().as_str().to_owned(), row).is_some() {
            return Err(invalid(
                "Amazon Bedrock model list contains duplicate model ids",
            ));
        }
    }
    Ok(rows.into_values().collect())
}

fn normalize_row(
    summary: FoundationModelSummary,
) -> Result<BedrockFoundationModel, CatalogFetchError> {
    let id = BedrockModelId::new(summary.model_id)
        .ok_or_else(|| invalid("Amazon Bedrock model list contains an invalid model id"))?;
    if !valid_model_arn(&summary.model_arn) {
        return Err(invalid(
            "Amazon Bedrock model list contains an invalid model ARN",
        ));
    }
    let display_name =
        match summary.model_name.as_deref() {
            Some(name) => Some(branded_name(name).ok_or_else(|| {
                invalid("Amazon Bedrock model list contains an invalid model name")
            })?),
            None => None,
        };
    let provider_name = match summary.provider_name.as_deref() {
        Some(name) => Some(branded_name(name).ok_or_else(|| {
            invalid("Amazon Bedrock model list contains an invalid provider name")
        })?),
        None => None,
    };
    let input_modalities = parse_modalities(summary.input_modalities.as_deref())?;
    let output_modalities = parse_modalities(summary.output_modalities.as_deref())?;
    let inference_types = parse_inference_types(summary.inference_types_supported.as_deref())?;
    let response_streaming = tri_state(summary.response_streaming_supported);
    let lifecycle = parse_lifecycle(summary.model_lifecycle.as_ref())?;

    let descriptor = ModelDescriptor {
        display_name: display_name.unwrap_or_else(|| id.as_str().to_owned()),
        id: id.as_str().to_owned(),
        // `ListFoundationModels` publishes no alias field. Cross-region
        // inference-profile ids come from a different API and are not aliases
        // this endpoint established.
        aliases: Vec::new(),
        created_at_ms: None,
        // The endpoint publishes no token limits at all, so the limits stay
        // absent rather than being back-filled from documentation.
        context_window: None,
        max_output_tokens: None,
        lifecycle: catalog_lifecycle(lifecycle),
        capabilities: capabilities(input_modalities.as_deref()),
        // Bedrock publishes prices in the pricing pages, not on this endpoint.
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    };

    Ok(BedrockFoundationModel {
        id,
        arn: summary.model_arn,
        provider_name,
        descriptor,
        input_modalities,
        output_modalities,
        response_streaming,
        inference_types,
        lifecycle,
    })
}

/// Project the two Bedrock modality lists onto CAT02 capability evidence.
///
/// Only image input has a counterpart in the shared vocabulary. Document input
/// stays [`CapabilitySupport::Unknown`] because the Bedrock modality
/// enumeration has no member that could express it — a vocabulary that cannot
/// say "document" is not evidence that documents are unsupported. Everything
/// else this endpoint says nothing about stays Unknown as well; protocol
/// compatibility is not model capability evidence (GOTCHAS #22).
fn capabilities(input_modalities: Option<&[BedrockModality]>) -> ModelCapabilities {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.image_input = match input_modalities {
        None => CapabilitySupport::Unknown,
        Some(modalities) if modalities.contains(&BedrockModality::Image) => {
            CapabilitySupport::Supported
        }
        Some(_) => CapabilitySupport::Unsupported,
    };
    capabilities
}

/// Project Bedrock lifecycle evidence onto the CAT02 lifecycle vocabulary.
///
/// `endOfLifeTime` becomes the retirement deadline for every phase, not only
/// for `LEGACY`: a cached `ACTIVE` row whose published deadline has since
/// passed must read as retired rather than as current (GOTCHAS #43).
fn catalog_lifecycle(lifecycle: BedrockLifecycle) -> ModelLifecycle {
    ModelLifecycle {
        status: match lifecycle.status {
            BedrockLifecycleStatus::Active => ModelLifecycleStatus::Stable,
            BedrockLifecycleStatus::Legacy => ModelLifecycleStatus::Deprecated,
            BedrockLifecycleStatus::Unknown => ModelLifecycleStatus::Unknown,
        },
        retirement_at_ms: lifecycle.end_of_life_at_ms,
        // The endpoint publishes no successor id, so none is invented.
        replacement_ids: Vec::new(),
    }
}

fn parse_lifecycle(
    lifecycle: Option<&FoundationModelLifecycle>,
) -> Result<BedrockLifecycle, CatalogFetchError> {
    let Some(lifecycle) = lifecycle else {
        return Ok(BedrockLifecycle::unknown());
    };
    Ok(BedrockLifecycle {
        status: BedrockLifecycleStatus::parse(&lifecycle.status),
        start_of_life_at_ms: parse_instant(lifecycle.start_of_life_time.as_deref())?,
        legacy_at_ms: parse_instant(lifecycle.legacy_time.as_deref())?,
        public_extended_access_at_ms: parse_instant(
            lifecycle.public_extended_access_time.as_deref(),
        )?,
        end_of_life_at_ms: parse_instant(lifecycle.end_of_life_time.as_deref())?,
    })
}

/// Parse one lifecycle instant into Unix milliseconds.
///
/// The Bedrock API model declares `timestampFormat: "iso8601"` for every
/// timestamp shape, so the wire value is an RFC 3339 date-time string. An
/// instant before the Unix epoch cannot describe a model lifecycle and is
/// refused rather than clamped.
fn parse_instant(value: Option<&str>) -> Result<Option<u64>, CatalogFetchError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let millis = DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|instant| instant.timestamp_millis())
        .and_then(|millis| u64::try_from(millis).ok())
        .ok_or_else(|| {
            invalid("Amazon Bedrock model list contains an invalid lifecycle instant")
        })?;
    Ok(Some(millis))
}

fn parse_modalities(
    values: Option<&[String]>,
) -> Result<Option<Vec<BedrockModality>>, CatalogFetchError> {
    parse_enum_list(values, BedrockModality::parse, "modality")
}

fn parse_inference_types(
    values: Option<&[String]>,
) -> Result<Option<Vec<BedrockInferenceType>>, CatalogFetchError> {
    parse_enum_list(values, BedrockInferenceType::parse, "inference type")
}

/// Parse one documented enumeration list, preserving the difference between an
/// unpublished list and a published empty one.
///
/// A member outside the documented enumeration fails the generation. The
/// alternative — dropping it — would publish a retained list that claims the
/// model supports less than AWS said, and there is no variant in which to
/// record "and one more thing we could not read".
fn parse_enum_list<T>(
    values: Option<&[String]>,
    parse: fn(&str) -> Option<T>,
    label: &'static str,
) -> Result<Option<Vec<T>>, CatalogFetchError> {
    let Some(values) = values else {
        return Ok(None);
    };
    if values.len() > MAX_ENUM_MEMBERS {
        return Err(CatalogFetchError::new(
            CatalogFailureKind::InvalidResponse,
            format!("Amazon Bedrock model list contains an oversized {label} list"),
        ));
    }
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        let member = parse(value).ok_or_else(|| {
            CatalogFetchError::new(
                CatalogFailureKind::InvalidResponse,
                format!("Amazon Bedrock model list contains an unrecognized {label}"),
            )
        })?;
        parsed.push(member);
    }
    Ok(Some(parsed))
}

/// An absent boolean is unproven evidence, never a default.
const fn tri_state(value: Option<bool>) -> CapabilitySupport {
    match value {
        Some(true) => CapabilitySupport::Supported,
        Some(false) => CapabilitySupport::Unsupported,
        None => CapabilitySupport::Unknown,
    }
}

pub(crate) fn invalid(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_lifecycle_instant_is_none_rather_than_an_epoch() {
        assert_eq!(parse_instant(None), Ok(None));
    }

    #[test]
    fn lifecycle_instants_parse_from_the_documented_iso8601_format() {
        assert_eq!(
            parse_instant(Some("2024-05-13T00:00:00Z")),
            Ok(Some(1_715_558_400_000))
        );
        assert_eq!(
            parse_instant(Some("2024-05-13T00:00:00.500Z")),
            Ok(Some(1_715_558_400_500))
        );
        assert_eq!(
            parse_instant(Some("2024-05-13T02:00:00+02:00")),
            Ok(Some(1_715_558_400_000))
        );
    }

    #[test]
    fn an_unparsable_or_pre_epoch_lifecycle_instant_is_refused() {
        for rejected in ["", "yesterday", "2024-05-13", "1969-12-31T23:59:59Z"] {
            let failure = parse_instant(Some(rejected)).expect_err("must be refused");
            assert_eq!(failure.kind(), CatalogFailureKind::InvalidResponse);
        }
    }

    #[test]
    fn an_unpublished_enumeration_list_stays_distinct_from_an_empty_one() {
        assert_eq!(parse_modalities(None), Ok(None));
        assert_eq!(parse_modalities(Some(&[])), Ok(Some(Vec::new())));
    }
}
