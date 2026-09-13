//! C11 — the request envelope: what a turn costs, measured contributor by
//! contributor, and priced only as well as it was measured.
//!
//! A single total hides the question that matters. "12,000 tokens" is useless
//! if 9,000 of it is provider state you cannot drop and 2,000 is an attachment
//! nobody counted. So an envelope keeps its contributors separate — system
//! prompt, messages, tools, provider state, attachments — and each carries its
//! own evidence.
//!
//! Two rules make the arithmetic honest, and they are the same rule CAT06
//! applies to prices:
//!
//! * **A total is only as good as its weakest contributor.** Exact plus
//!   estimated is estimated, never exact.
//! * **Unknown is not zero.** A contributor nothing could count makes the total
//!   a *lower bound*, and a lower bound can never be reported as a complete
//!   number. Summing an unknown as zero is how a context meter silently tells
//!   you there is room when there is not.

use std::fmt;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::inference::{InferenceInput, ResolvedCall};
use crate::model_pricing::{ModelPricing, PriceComponent, PriceCurrency, TokenPriceUnit};
use crate::token_count::{
    EstimationMethod, HeuristicTokenEstimator, TokenCount, TokenCountError, TokenCountFailureKind,
    TokenCountRefusal, TokenCountRequest, TokenCounterRegistry,
};
use crate::{ChatMessage, ChatRequest, Role, ToolSpec};

/// Which part of a request an envelope entry measures.
///
/// Declaration order is the stable presentation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EnvelopeContributor {
    /// The rendered system prompt.
    System,
    /// Project instructions, skills, and other attributed prompt guidance.
    Guidance,
    /// The transcript messages.
    Messages,
    /// Results returned by tools, separated from conversational messages.
    ToolResults,
    /// The offered tool definitions.
    Tools,
    /// Replayed provider-native state.
    ProviderState,
    /// Admitted image or document attachments.
    Attachments,
}

impl EnvelopeContributor {
    /// Stable name for diagnostics and display.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Guidance => "guidance",
            Self::Messages => "messages",
            Self::ToolResults => "tool_results",
            Self::Tools => "tools",
            Self::ProviderState => "provider_state",
            Self::Attachments => "attachments",
        }
    }
}

impl fmt::Display for EnvelopeContributor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Why a contributor could not be counted.
///
/// A reason is a closed class, never free text, so a caller can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncountedReason {
    /// No registered counter serves this provider and model.
    NoCounter,
    /// Every eligible counter declined this content.
    Refused,
    /// A counter's own operation failed.
    Failed,
    /// The content is a form no counter in this build can measure, such as
    /// image bytes without a vision tokenizer.
    Unmeasurable,
}

impl UncountedReason {
    /// Stable name for diagnostics and display.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoCounter => "no_counter",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Unmeasurable => "unmeasurable",
        }
    }
}

/// What is known about one contributor's token cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContributorTokens {
    /// The provider measured this contributor.
    Exact(u64),
    /// A local heuristic estimated it by the named method.
    Estimated(u64, EstimationMethod),
    /// Nothing could count it. The value is unknown, **not** zero.
    Uncounted(UncountedReason),
}

impl ContributorTokens {
    /// Adopt whatever evidence a completed count carries.
    #[must_use]
    pub fn from_count(count: &TokenCount) -> Self {
        match count {
            TokenCount::Exact(exact) => Self::Exact(exact.tokens()),
            TokenCount::Estimated(estimated) => {
                Self::Estimated(estimated.tokens(), estimated.method())
            }
        }
    }

    /// Counted tokens, or `None` when this contributor is uncounted.
    ///
    /// Deliberately an `Option`: there is no accessor that turns an uncounted
    /// contributor into a number, because that number would be a fiction.
    #[must_use]
    pub const fn counted(&self) -> Option<u64> {
        match self {
            Self::Exact(tokens) | Self::Estimated(tokens, _) => Some(*tokens),
            Self::Uncounted(_) => None,
        }
    }

    /// Whether the provider itself measured this contributor.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self, Self::Exact(_))
    }
}

/// One measured part of a request envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeEntry {
    contributor: EnvelopeContributor,
    tokens: ContributorTokens,
    refusals: Vec<TokenCountRefusal>,
}

impl EnvelopeEntry {
    /// Record one contributor's measurement.
    #[must_use]
    pub const fn new(contributor: EnvelopeContributor, tokens: ContributorTokens) -> Self {
        Self {
            contributor,
            tokens,
            refusals: Vec::new(),
        }
    }

    /// Retain better-ranked counters that refused before this measurement.
    ///
    /// A fallback is evidence and must remain visible to context/usage
    /// consumers rather than collapsing into the count that eventually won.
    #[must_use]
    pub fn with_refusals(mut self, refusals: Vec<TokenCountRefusal>) -> Self {
        self.refusals = refusals;
        self
    }

    /// Which part of the request this measures.
    #[must_use]
    pub const fn contributor(&self) -> EnvelopeContributor {
        self.contributor
    }

    /// What is known about its cost.
    #[must_use]
    pub const fn tokens(&self) -> &ContributorTokens {
        &self.tokens
    }

    /// Better-ranked counters that refused this contributor.
    #[must_use]
    pub fn refusals(&self) -> &[TokenCountRefusal] {
        &self.refusals
    }
}

/// The total token cost of a request envelope.
///
/// There is deliberately no plain `u64` total. A caller must decide what to do
/// about an incomplete measurement instead of being handed a number that looks
/// complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeTotal {
    /// Every contributor was measured by the provider.
    Exact(u64),
    /// Every contributor was counted, but at least one was estimated.
    Estimated(u64),
    /// At least one contributor could not be counted, so this is a **lower
    /// bound**: the real total is this or more, never less.
    AtLeast {
        /// Sum of the contributors that were counted.
        counted: u64,
        /// Contributors nothing could count, in presentation order.
        uncounted: Vec<(EnvelopeContributor, UncountedReason)>,
    },
}

impl EnvelopeTotal {
    /// The counted token sum. For [`Self::AtLeast`] this is a lower bound, not
    /// the total, which is why the variant must be matched to learn that.
    #[must_use]
    pub const fn counted(&self) -> u64 {
        match self {
            Self::Exact(tokens) | Self::Estimated(tokens) => *tokens,
            Self::AtLeast { counted, .. } => *counted,
        }
    }

    /// Whether every contributor was counted, exactly or by estimate.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        !matches!(self, Self::AtLeast { .. })
    }
}

/// What one envelope costs, priced from a model's published input price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeCost {
    /// Exact tokens at a published price.
    Exact {
        /// Cost in 10^-12 units of `currency`.
        pico_units: u128,
        /// Currency the price was published in.
        currency: PriceCurrency,
    },
    /// A published price applied to an estimated or lower-bound token count,
    /// so the cost carries that same weakness.
    AtLeast {
        /// Lower-bound cost in 10^-12 units of `currency`.
        pico_units: u128,
        /// Currency the price was published in.
        currency: PriceCurrency,
    },
    /// The model publishes no input price. Unknown cost is **not** free.
    Unpriced,
}

/// A request envelope: every contributor, with its own evidence.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenEnvelope {
    entries: Vec<EnvelopeEntry>,
}

/// Failure to construct an envelope from one live request.
///
/// Ordinary counter absence/refusal/failure is retained on the affected
/// contributor as [`ContributorTokens::Uncounted`]. Only caller cancellation
/// or an invalid provider/model target prevents publication of the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EnvelopeMeasurementError {
    /// The caller cancelled before all contributors settled.
    #[error("token envelope measurement was cancelled")]
    Cancelled,
    /// Provider/model identity was invalid before any counter ran.
    #[error("token envelope target is invalid")]
    InvalidTarget,
}

impl TokenEnvelope {
    /// An envelope with no contributors.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record one contributor.
    ///
    /// A contributor measured twice replaces its earlier entry, so an envelope
    /// can never double-count one part of a request.
    #[must_use]
    pub fn with(mut self, entry: EnvelopeEntry) -> Self {
        match self
            .entries
            .iter_mut()
            .find(|existing| existing.contributor == entry.contributor)
        {
            Some(existing) => *existing = entry,
            None => {
                self.entries.push(entry);
                self.entries.sort_by_key(EnvelopeEntry::contributor);
            }
        }
        self
    }

    /// Allocate a locally estimated system total between disjoint prompt byte spans.
    /// The total is preserved; both allocations remain estimates, not tokenizer counts.
    /// Uncounted or exact system measurements are left intact because byte allocation
    /// cannot preserve their evidence. Invalid attribution also leaves the envelope intact.
    #[must_use]
    pub fn with_guidance_attribution(mut self, total_bytes: usize, guidance_bytes: usize) -> Self {
        if total_bytes == 0
            || guidance_bytes > total_bytes
            || self
                .entries
                .iter()
                .any(|entry| entry.contributor == EnvelopeContributor::Guidance)
        {
            return self;
        }
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.contributor == EnvelopeContributor::System)
        else {
            return self;
        };
        let ContributorTokens::Estimated(total, method) = entry.tokens else {
            return self;
        };
        let guidance = ((u128::from(total) * guidance_bytes as u128) / total_bytes as u128) as u64;
        entry.tokens = ContributorTokens::Estimated(total - guidance, method);
        self.with(EnvelopeEntry::new(
            EnvelopeContributor::Guidance,
            ContributorTokens::Estimated(guidance, method),
        ))
    }

    /// Entries in stable contributor order.
    #[must_use]
    pub fn entries(&self) -> &[EnvelopeEntry] {
        &self.entries
    }

    /// The envelope total, weakened by its weakest contributor.
    #[must_use]
    pub fn total(&self) -> EnvelopeTotal {
        let mut counted: u64 = 0;
        let mut estimated = false;
        let mut uncounted = Vec::new();
        for entry in &self.entries {
            match &entry.tokens {
                ContributorTokens::Exact(tokens) => counted = counted.saturating_add(*tokens),
                ContributorTokens::Estimated(tokens, _) => {
                    estimated = true;
                    counted = counted.saturating_add(*tokens);
                }
                ContributorTokens::Uncounted(reason) => {
                    uncounted.push((entry.contributor, *reason));
                }
            }
        }
        if !uncounted.is_empty() {
            return EnvelopeTotal::AtLeast { counted, uncounted };
        }
        if estimated {
            EnvelopeTotal::Estimated(counted)
        } else {
            EnvelopeTotal::Exact(counted)
        }
    }

    /// Price this envelope with a model's published input price.
    ///
    /// An envelope is request input, so it is priced from
    /// [`PriceComponent::Input`]. Cached-input and output components describe
    /// different quantities and are deliberately not substituted for it.
    #[must_use]
    pub fn cost(&self, pricing: &ModelPricing) -> EnvelopeCost {
        let Some(price) = pricing.price(PriceComponent::Input) else {
            // A missing price is unknown, never free.
            return EnvelopeCost::Unpriced;
        };
        let total = self.total();
        let tokens = u128::from(total.counted());
        let per_unit = u128::from(price.pico_units());
        let pico_units = match price.unit() {
            TokenPriceUnit::PerToken => tokens.saturating_mul(per_unit),
            // Integer division truncates, which keeps a per-million price a
            // lower bound rather than rounding a cost upward.
            TokenPriceUnit::PerMillionTokens => {
                tokens.saturating_mul(per_unit).saturating_div(1_000_000)
            }
        };
        let currency = price.currency();
        match total {
            EnvelopeTotal::Exact(_) => EnvelopeCost::Exact {
                pico_units,
                currency,
            },
            // An estimate or a lower-bound token count cannot produce an exact
            // cost, however exact the published price is.
            EnvelopeTotal::Estimated(_) | EnvelopeTotal::AtLeast { .. } => EnvelopeCost::AtLeast {
                pico_units,
                currency,
            },
        }
    }
}

/// Measure all contributors of a strict resolved request.
///
/// System text and tool schemas use the explicit
/// local estimator: a provider endpoint cannot isolate those pieces without a
/// fabricated transcript. Transcript messages use the best registered counter.
/// Provider-native state and media are uncounted: serialized opaque state
/// does not establish the number of tokens replayed by the provider.
///
/// # Errors
/// [`EnvelopeMeasurementError::Cancelled`] when `cancellation` fires before
/// the complete envelope exists, or [`EnvelopeMeasurementError::InvalidTarget`]
/// for an invalid route identity.
pub async fn measure_resolved_call_envelope(
    registry: &TokenCounterRegistry,
    call: &ResolvedCall,
    cancellation: &CancellationToken,
) -> Result<TokenEnvelope, EnvelopeMeasurementError> {
    let mut system = call
        .system()
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    let mut messages = Vec::new();
    let mut has_provider_state = false;
    let mut has_attachments = false;
    for input in call.inputs() {
        match input {
            InferenceInput::Message(message) => {
                partition_message(message, &mut system, &mut messages, &mut has_attachments)
            }
            InferenceInput::ProviderState(_) => has_provider_state = true,
        }
    }
    measure_parts(
        registry,
        call.provider(),
        call.model(),
        &system,
        &messages,
        call.tools(),
        has_provider_state,
        has_attachments,
        cancellation,
    )
    .await
}

/// Measure all contributors of a compatibility [`ChatRequest`].
///
/// The request has no lossless provider-state plane, so that contributor is
/// exactly zero. Media is removed from the countable message clone and
/// represented independently as unmeasurable rather than disappearing.
///
/// # Errors
/// [`EnvelopeMeasurementError::Cancelled`] when `cancellation` fires before
/// the complete envelope exists, or [`EnvelopeMeasurementError::InvalidTarget`]
/// for an invalid route identity.
pub async fn measure_chat_request_envelope(
    registry: &TokenCounterRegistry,
    provider: &str,
    request: &ChatRequest,
    cancellation: &CancellationToken,
) -> Result<TokenEnvelope, EnvelopeMeasurementError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut has_attachments = false;
    for message in &request.messages {
        partition_message(message, &mut system, &mut messages, &mut has_attachments);
    }
    measure_parts(
        registry,
        provider,
        &request.model,
        &system,
        &messages,
        request.tools.as_deref().unwrap_or(&[]),
        false,
        has_attachments,
        cancellation,
    )
    .await
}

/// Measure a hidden audio request while preserving audio as an explicitly
/// uncounted attachment contributor.
///
/// # Errors
/// Cancellation or invalid provider/model identity prevents publication.
pub async fn measure_experimental_audio_envelope(
    registry: &TokenCounterRegistry,
    provider: &str,
    request: &crate::ExperimentalAudioRequest,
    cancellation: &CancellationToken,
) -> Result<TokenEnvelope, EnvelopeMeasurementError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut ignored_media_flag = false;
    for message in &request.base().messages {
        partition_message(message, &mut system, &mut messages, &mut ignored_media_flag);
    }
    measure_parts(
        registry,
        provider,
        &request.base().model,
        &system,
        &messages,
        request.base().tools.as_deref().unwrap_or(&[]),
        false,
        true,
        cancellation,
    )
    .await
}

fn partition_message(
    message: &ChatMessage,
    system: &mut Vec<String>,
    messages: &mut Vec<ChatMessage>,
    has_attachments: &mut bool,
) {
    *has_attachments |= !message.images.is_empty() || !message.documents.is_empty();
    if message.role == Role::System {
        if !message.content.is_empty() {
            system.push(message.content.clone());
        }
        return;
    }
    let mut countable = message.clone();
    countable.images.clear();
    countable.documents.clear();
    messages.push(countable);
}

#[allow(clippy::too_many_arguments)]
async fn measure_parts(
    registry: &TokenCounterRegistry,
    provider: &str,
    model: &str,
    system: &[String],
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    has_provider_state: bool,
    has_attachments: bool,
    cancellation: &CancellationToken,
) -> Result<TokenEnvelope, EnvelopeMeasurementError> {
    ensure_live(cancellation)?;
    let system_tokens = estimate_texts(provider, model, system)?;
    ensure_live(cancellation)?;
    let (tool_results, conversation): (Vec<_>, Vec<_>) = messages
        .iter()
        .cloned()
        .partition(|message| message.role == Role::Tool);
    let (message_tokens, message_refusals) =
        measure_messages(registry, provider, model, &conversation, cancellation).await?;
    let result_tokens = if tool_results.is_empty() {
        ContributorTokens::Exact(0)
    } else {
        let mut result_request = request(provider, model)?;
        for result in &tool_results {
            result_request = result_request.with_message(result);
        }
        local_estimate(&result_request)
    };
    ensure_live(cancellation)?;
    let tool_tokens = estimate_tools(provider, model, tools)?;
    ensure_live(cancellation)?;
    let state_tokens = if has_provider_state {
        ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    } else {
        ContributorTokens::Exact(0)
    };
    ensure_live(cancellation)?;
    let attachment_tokens = if has_attachments {
        ContributorTokens::Uncounted(UncountedReason::Unmeasurable)
    } else {
        ContributorTokens::Exact(0)
    };
    Ok(TokenEnvelope::new()
        .with(EnvelopeEntry::new(
            EnvelopeContributor::System,
            system_tokens,
        ))
        .with(
            EnvelopeEntry::new(EnvelopeContributor::Messages, message_tokens)
                .with_refusals(message_refusals),
        )
        .with(EnvelopeEntry::new(
            EnvelopeContributor::ToolResults,
            result_tokens,
        ))
        .with(EnvelopeEntry::new(EnvelopeContributor::Tools, tool_tokens))
        .with(EnvelopeEntry::new(
            EnvelopeContributor::ProviderState,
            state_tokens,
        ))
        .with(EnvelopeEntry::new(
            EnvelopeContributor::Attachments,
            attachment_tokens,
        )))
}

fn estimate_texts(
    provider: &str,
    model: &str,
    texts: &[String],
) -> Result<ContributorTokens, EnvelopeMeasurementError> {
    if texts.is_empty() {
        return Ok(ContributorTokens::Exact(0));
    }
    let mut request = request(provider, model)?;
    for text in texts {
        request = request.with_text(text);
    }
    Ok(local_estimate(&request))
}

fn estimate_tools(
    provider: &str,
    model: &str,
    tools: &[ToolSpec],
) -> Result<ContributorTokens, EnvelopeMeasurementError> {
    if tools.is_empty() {
        return Ok(ContributorTokens::Exact(0));
    }
    let mut request = request(provider, model)?;
    for tool in tools {
        request = request.with_tool(tool);
    }
    Ok(local_estimate(&request))
}

async fn measure_messages(
    registry: &TokenCounterRegistry,
    provider: &str,
    model: &str,
    messages: &[ChatMessage],
    cancellation: &CancellationToken,
) -> Result<(ContributorTokens, Vec<TokenCountRefusal>), EnvelopeMeasurementError> {
    if messages.is_empty() {
        return Ok((ContributorTokens::Exact(0), Vec::new()));
    }
    let mut request = request(provider, model)?;
    for message in messages {
        request = request.with_message(message);
    }
    match registry.count(&request, cancellation).await {
        Ok(outcome) => Ok((
            ContributorTokens::from_count(outcome.count()),
            outcome.refused().to_vec(),
        )),
        Err(TokenCountError::NoCounter { .. }) => Ok((
            ContributorTokens::Uncounted(UncountedReason::NoCounter),
            Vec::new(),
        )),
        Err(TokenCountError::Unsupported { refusals, .. }) => Ok((
            ContributorTokens::Uncounted(UncountedReason::Refused),
            refusals,
        )),
        Err(TokenCountError::Cancelled { .. }) => Err(EnvelopeMeasurementError::Cancelled),
        Err(
            TokenCountError::Invalid { .. }
            | TokenCountError::DuplicateCounter { .. }
            | TokenCountError::CounterFailed { .. }
            | TokenCountError::RegistryUnavailable,
        ) => Ok((
            ContributorTokens::Uncounted(UncountedReason::Failed),
            Vec::new(),
        )),
    }
}

fn local_estimate(request: &TokenCountRequest<'_>) -> ContributorTokens {
    match HeuristicTokenEstimator::new().estimate(request) {
        Ok(estimate) => ContributorTokens::Estimated(estimate.tokens(), estimate.method()),
        Err(failure) => match failure.kind() {
            TokenCountFailureKind::Unsupported => {
                ContributorTokens::Uncounted(UncountedReason::Refused)
            }
            TokenCountFailureKind::Failed | TokenCountFailureKind::Cancelled => {
                ContributorTokens::Uncounted(UncountedReason::Failed)
            }
        },
    }
}

fn request<'a>(
    provider: &'a str,
    model: &'a str,
) -> Result<TokenCountRequest<'a>, EnvelopeMeasurementError> {
    TokenCountRequest::new(provider, model).map_err(|_| EnvelopeMeasurementError::InvalidTarget)
}

fn ensure_live(cancellation: &CancellationToken) -> Result<(), EnvelopeMeasurementError> {
    if cancellation.is_cancelled() {
        Err(EnvelopeMeasurementError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model_pricing::{ModelMetadataProvenance, TokenPrice};

    fn provenance() -> ModelMetadataProvenance {
        ModelMetadataProvenance::new("test:token-meter", 1).unwrap()
    }

    fn entry(contributor: EnvelopeContributor, tokens: ContributorTokens) -> EnvelopeEntry {
        EnvelopeEntry::new(contributor, tokens)
    }

    fn usd_per_token(amount: &str) -> ModelPricing {
        ModelPricing::captured(
            provenance(),
            PriceComponent::Input,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, amount)
                .unwrap(),
        )
    }

    #[test]
    fn guidance_allocation_preserves_total_and_unknown_evidence() {
        let original = TokenEnvelope::new().with(entry(
            EnvelopeContributor::System,
            ContributorTokens::Estimated(17, EstimationMethod::Utf8ByteRatio),
        ));
        let split = original.clone().with_guidance_attribution(65, 41);
        assert_eq!(split.total(), original.total());
        assert_eq!(split.clone().with_guidance_attribution(65, 41), split);
        assert_eq!(
            split.entries()[0].tokens(),
            &ContributorTokens::Estimated(7, EstimationMethod::Utf8ByteRatio)
        );
        assert_eq!(
            split.entries()[1].tokens(),
            &ContributorTokens::Estimated(10, EstimationMethod::Utf8ByteRatio)
        );
        assert_eq!(original.clone().with_guidance_attribution(0, 0), original);
        assert_eq!(original.clone().with_guidance_attribution(10, 11), original);
        let unknown = TokenEnvelope::new().with(entry(
            EnvelopeContributor::System,
            ContributorTokens::Uncounted(UncountedReason::Unmeasurable),
        ));
        assert_eq!(unknown.clone().with_guidance_attribution(10, 5), unknown);
    }

    #[test]
    fn a_total_is_only_as_good_as_its_weakest_contributor() {
        let exact = TokenEnvelope::new()
            .with(entry(
                EnvelopeContributor::System,
                ContributorTokens::Exact(100),
            ))
            .with(entry(
                EnvelopeContributor::Messages,
                ContributorTokens::Exact(400),
            ));
        assert_eq!(exact.total(), EnvelopeTotal::Exact(500));

        // One estimate makes the whole total an estimate.
        let mixed = exact.clone().with(entry(
            EnvelopeContributor::Tools,
            ContributorTokens::Estimated(50, EstimationMethod::Utf8ByteRatio),
        ));
        assert_eq!(mixed.total(), EnvelopeTotal::Estimated(550));
        assert!(mixed.total().is_complete());
    }

    #[test]
    fn an_uncounted_contributor_makes_the_total_a_lower_bound_never_zero() {
        let envelope = TokenEnvelope::new()
            .with(entry(
                EnvelopeContributor::Messages,
                ContributorTokens::Exact(400),
            ))
            .with(entry(
                EnvelopeContributor::Attachments,
                ContributorTokens::Uncounted(UncountedReason::Unmeasurable),
            ));
        let total = envelope.total();
        // The counted part is still 400 — the attachment is not summed as
        // zero, it is named as missing.
        assert_eq!(
            total,
            EnvelopeTotal::AtLeast {
                counted: 400,
                uncounted: vec![(
                    EnvelopeContributor::Attachments,
                    UncountedReason::Unmeasurable
                )],
            }
        );
        assert!(!total.is_complete(), "a lower bound is not a total");
        assert_eq!(total.counted(), 400);
    }

    #[test]
    fn an_uncounted_contributor_cannot_be_read_as_a_number() {
        let uncounted = ContributorTokens::Uncounted(UncountedReason::NoCounter);
        assert_eq!(uncounted.counted(), None);
        assert!(!uncounted.is_exact());
        // The only way to a number is through Some(..), so a caller must
        // handle absence rather than receive a zero.
        assert_eq!(ContributorTokens::Exact(7).counted(), Some(7));
        assert_eq!(
            ContributorTokens::Estimated(7, EstimationMethod::Utf8ByteRatio).counted(),
            Some(7)
        );
        assert!(!ContributorTokens::Estimated(7, EstimationMethod::Utf8ByteRatio).is_exact());
    }

    #[test]
    fn contributors_stay_ordered_and_are_never_double_counted() {
        let envelope = TokenEnvelope::new()
            .with(entry(
                EnvelopeContributor::Attachments,
                ContributorTokens::Exact(9),
            ))
            .with(entry(
                EnvelopeContributor::System,
                ContributorTokens::Exact(1),
            ))
            .with(entry(
                EnvelopeContributor::Tools,
                ContributorTokens::Exact(3),
            ));
        assert_eq!(
            envelope
                .entries()
                .iter()
                .map(|entry| entry.contributor())
                .collect::<Vec<_>>(),
            vec![
                EnvelopeContributor::System,
                EnvelopeContributor::Tools,
                EnvelopeContributor::Attachments,
            ]
        );
        // Re-measuring one contributor replaces it rather than adding again.
        let remeasured = envelope.with(entry(
            EnvelopeContributor::Tools,
            ContributorTokens::Exact(5),
        ));
        assert_eq!(remeasured.entries().len(), 3);
        assert_eq!(remeasured.total(), EnvelopeTotal::Exact(1 + 5 + 9));
    }

    #[test]
    fn cost_carries_the_weakness_of_the_token_count() {
        let pricing = usd_per_token("0.000000075");
        let exact = TokenEnvelope::new().with(entry(
            EnvelopeContributor::Messages,
            ContributorTokens::Exact(1_000),
        ));
        // 1000 tokens x 75_000 pico-units.
        assert_eq!(
            exact.cost(&pricing),
            EnvelopeCost::Exact {
                pico_units: 75_000_000,
                currency: PriceCurrency::Usd
            }
        );

        // An exact price applied to an estimate is still not an exact cost.
        let estimated = TokenEnvelope::new().with(entry(
            EnvelopeContributor::Messages,
            ContributorTokens::Estimated(1_000, EstimationMethod::Utf8ByteRatio),
        ));
        assert_eq!(
            estimated.cost(&pricing),
            EnvelopeCost::AtLeast {
                pico_units: 75_000_000,
                currency: PriceCurrency::Usd
            }
        );

        // A lower-bound token count produces a lower-bound cost.
        let partial = estimated.with(entry(
            EnvelopeContributor::Attachments,
            ContributorTokens::Uncounted(UncountedReason::Unmeasurable),
        ));
        assert!(matches!(
            partial.cost(&pricing),
            EnvelopeCost::AtLeast { .. }
        ));
    }

    #[test]
    fn an_unpriced_model_costs_unknown_not_zero() {
        let envelope = TokenEnvelope::new().with(entry(
            EnvelopeContributor::Messages,
            ContributorTokens::Exact(1_000),
        ));
        assert_eq!(
            envelope.cost(&ModelPricing::unknown()),
            EnvelopeCost::Unpriced
        );

        // A model that publishes only an OUTPUT price does not thereby price
        // its input; substituting one for the other would misreport cost.
        let output_only = ModelPricing::captured(
            provenance(),
            PriceComponent::Output,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, "0.00000025")
                .unwrap(),
        );
        assert_eq!(envelope.cost(&output_only), EnvelopeCost::Unpriced);
    }

    #[test]
    fn a_per_million_price_is_applied_without_rounding_a_cost_upward() {
        let pricing = ModelPricing::captured(
            provenance(),
            PriceComponent::Input,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerMillionTokens, "3.0")
                .unwrap(),
        );
        // 3.0 USD per million tokens is 3_000_000_000_000 pico-units per
        // million, so 1000 tokens costs 3_000_000_000 pico-units.
        let envelope = TokenEnvelope::new().with(entry(
            EnvelopeContributor::Messages,
            ContributorTokens::Exact(1_000),
        ));
        assert_eq!(
            envelope.cost(&pricing),
            EnvelopeCost::Exact {
                pico_units: 3_000_000_000,
                currency: PriceCurrency::Usd
            }
        );
        // A sub-token-fraction cost truncates down rather than up.
        let tiny = TokenEnvelope::new().with(entry(
            EnvelopeContributor::Messages,
            ContributorTokens::Exact(0),
        ));
        assert_eq!(
            tiny.cost(&pricing),
            EnvelopeCost::Exact {
                pico_units: 0,
                currency: PriceCurrency::Usd
            }
        );
    }

    #[test]
    fn an_empty_envelope_is_exactly_zero_not_unknown() {
        // Nothing to measure is a different statement from "could not measure".
        let envelope = TokenEnvelope::new();
        assert_eq!(envelope.total(), EnvelopeTotal::Exact(0));
        assert!(envelope.total().is_complete());
    }
}
