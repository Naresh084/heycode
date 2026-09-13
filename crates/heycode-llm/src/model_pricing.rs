//! Normalized pricing and advisory performance evidence for a catalog row.
//!
//! Every value here is provider EVIDENCE. Absence is representable and always
//! means "the provider published nothing", never "free" and never "fast". A
//! price is stored as an exact integer in a named currency and a named billed
//! unit, so no float ever decides what a request costs.

use std::collections::BTreeMap;

use thiserror::Error;

/// Fractional decimal digits retained exactly by [`TokenPrice`]. An amount
/// carrying significant digits past this is rejected instead of rounded.
const PRICE_FRACTION_DIGITS: usize = 12;

/// Scale between one currency unit and the stored integer amount.
const PICO_UNITS_PER_CURRENCY_UNIT: u64 = 1_000_000_000_000;

/// Maximum retained source label for one normalized metadata capture.
const MAX_METADATA_SOURCE_BYTES: usize = 512;

/// Safe provenance validation failure. Rejected source text is never echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ModelProvenanceError {
    /// Source is blank, padded, oversized or contains control characters.
    #[error("model metadata source is invalid")]
    InvalidSource,
    /// Unix-millisecond capture instant is zero.
    #[error("model metadata has no capture instant")]
    MissingCaptureInstant,
}

/// Source and exact capture instant for one normalized metadata fact.
///
/// This value lives inside [`ModelPricing`] and [`ModelPerformance`], not only
/// on their enclosing catalog generation. A cache/display consumer can
/// therefore retain and show where each advisory fact came from even after it
/// is separated from the snapshot that carried it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelMetadataProvenance {
    source: String,
    captured_at_ms: u64,
}

impl ModelMetadataProvenance {
    /// Validate one safe source label and Unix-millisecond capture instant.
    ///
    /// Sources may be public URLs or stable implementation/benchmark labels.
    /// They are retained exactly after boundary validation.
    ///
    /// # Errors
    /// Blank, padded, oversized or control-bearing sources fail with
    /// [`ModelProvenanceError::InvalidSource`]. A zero capture instant fails
    /// with [`ModelProvenanceError::MissingCaptureInstant`].
    pub fn new(
        source: impl Into<String>,
        captured_at_ms: u64,
    ) -> Result<Self, ModelProvenanceError> {
        let source = source.into();
        if source.is_empty()
            || source.trim() != source
            || source.len() > MAX_METADATA_SOURCE_BYTES
            || source.chars().any(char::is_control)
        {
            return Err(ModelProvenanceError::InvalidSource);
        }
        if captured_at_ms == 0 {
            return Err(ModelProvenanceError::MissingCaptureInstant);
        }
        Ok(Self {
            source,
            captured_at_ms,
        })
    }

    /// Exact source label supplied by the normalizer.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Unix-millisecond instant this metadata was captured.
    #[must_use]
    pub const fn captured_at_ms(&self) -> u64 {
        self.captured_at_ms
    }
}

/// Safe pricing normalization failure. Messages never contain provider bodies.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PricingError {
    /// Currency code is not one this vocabulary represents.
    #[error("unknown price currency `{code}`")]
    UnknownCurrency {
        /// Rejected currency code exactly as published.
        code: String,
    },
    /// Unit name is not a billed quantity this vocabulary represents.
    #[error("unknown token price unit `{name}`")]
    UnknownUnit {
        /// Rejected unit name exactly as published.
        name: String,
    },
    /// Component name is not a billable component this vocabulary represents.
    #[error("unknown price component `{name}`")]
    UnknownComponent {
        /// Rejected component name exactly as published.
        name: String,
    },
    /// Amount is not a plain non-negative decimal number.
    #[error("price amount is not a plain decimal number")]
    MalformedAmount,
    /// Amount is negative; a published price is never below zero.
    #[error("price amount is negative")]
    NegativeAmount,
    /// Amount carries significant digits the exact representation cannot keep.
    #[error("price amount needs more than 12 fractional digits")]
    ExcessivePrecision,
    /// Amount is outside the plausible published range for its unit.
    #[error("price amount is outside the representable range for its unit")]
    AmountOutOfRange,
    /// One model published the same component twice.
    #[error("price component `{component}` is published more than once")]
    DuplicateComponent {
        /// Component published twice.
        component: &'static str,
    },
    /// One model mixed currencies or units across its components.
    #[error("model pricing mixes `{existing}` with `{added}`")]
    InconsistentDenomination {
        /// Denomination already established for this model.
        existing: String,
        /// Denomination that conflicts with it.
        added: String,
    },
    /// A component was added without source/capture provenance.
    #[error("model pricing has no source or capture instant")]
    MissingProvenance,
}

/// Currency a provider publishes prices in. Only currencies with real provider
/// evidence are representable; an unrecognized code fails rather than defaulting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PriceCurrency {
    /// United States dollar.
    Usd,
}

impl PriceCurrency {
    /// Stable wire code for this currency.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
        }
    }

    /// Parse an exact currency code.
    ///
    /// # Errors
    /// Returns [`PricingError::UnknownCurrency`] for any code outside the
    /// represented set, including differently cased spellings.
    pub fn parse(code: &str) -> Result<Self, PricingError> {
        match code {
            "USD" => Ok(Self::Usd),
            other => Err(PricingError::UnknownCurrency {
                code: other.to_owned(),
            }),
        }
    }
}

/// The billed quantity a published token price applies to. Providers publish
/// either shape, so the descriptor keeps the one it was published in rather
/// than converting and losing the exact figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenPriceUnit {
    /// Price charged for one token.
    PerToken,
    /// Price charged for one million tokens.
    PerMillionTokens,
}

impl TokenPriceUnit {
    /// Stable wire name for this unit.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PerToken => "per_token",
            Self::PerMillionTokens => "per_million_tokens",
        }
    }

    /// Parse an exact unit name.
    ///
    /// # Errors
    /// Returns [`PricingError::UnknownUnit`] for any name outside the
    /// represented set.
    pub fn parse(name: &str) -> Result<Self, PricingError> {
        match name {
            "per_token" => Ok(Self::PerToken),
            "per_million_tokens" => Ok(Self::PerMillionTokens),
            other => Err(PricingError::UnknownUnit {
                name: other.to_owned(),
            }),
        }
    }

    /// Largest plausible published amount for this unit, in stored units.
    /// Both bounds are the same real ceiling of one million currency units
    /// per million tokens.
    const fn max_pico_units(self) -> u64 {
        match self {
            Self::PerToken => PICO_UNITS_PER_CURRENCY_UNIT,
            Self::PerMillionTokens => 1_000_000_000_000_000_000,
        }
    }
}

/// One exact published price: a currency, the quantity it is billed per, and
/// an exact integer amount. Constructing one is the only way to state a price,
/// so a bare number can never be mistaken for a cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenPrice {
    currency: PriceCurrency,
    unit: TokenPriceUnit,
    pico_units: u64,
}

impl TokenPrice {
    /// Parse an exact decimal amount as published by a provider catalog.
    ///
    /// Accepts plain decimal notation only (`0`, `0.0000001`, `12.5`).
    /// Exponent notation, signs, whitespace and any other spelling fail rather
    /// than being interpreted.
    ///
    /// # Errors
    /// Returns [`PricingError::NegativeAmount`] for a negative decimal,
    /// [`PricingError::MalformedAmount`] for anything that is not a plain
    /// decimal number, [`PricingError::ExcessivePrecision`] when significant
    /// digits fall past the exact representation, and
    /// [`PricingError::AmountOutOfRange`] for an implausible magnitude.
    pub fn parse_decimal(
        currency: PriceCurrency,
        unit: TokenPriceUnit,
        amount: &str,
    ) -> Result<Self, PricingError> {
        let pico_units = parse_pico_units(amount)?;
        Self::from_pico_units(currency, unit, pico_units)
    }

    /// Build a price from an already exact integer amount, where `pico_units`
    /// counts 10^-12 units of `currency` per one `unit`.
    ///
    /// # Errors
    /// Returns [`PricingError::AmountOutOfRange`] when the amount exceeds the
    /// plausible published maximum for `unit`.
    pub const fn from_pico_units(
        currency: PriceCurrency,
        unit: TokenPriceUnit,
        pico_units: u64,
    ) -> Result<Self, PricingError> {
        if pico_units > unit.max_pico_units() {
            return Err(PricingError::AmountOutOfRange);
        }
        Ok(Self {
            currency,
            unit,
            pico_units,
        })
    }

    /// Currency this amount is denominated in.
    #[must_use]
    pub const fn currency(self) -> PriceCurrency {
        self.currency
    }

    /// Quantity this amount is charged per.
    #[must_use]
    pub const fn unit(self) -> TokenPriceUnit {
        self.unit
    }

    /// Exact amount in 10^-12 units of [`Self::currency`], per [`Self::unit`].
    #[must_use]
    pub const fn pico_units(self) -> u64 {
        self.pico_units
    }

    /// Whether the provider published this component as free. This is evidence
    /// of zero cost, which is not the same as no published price at all.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.pico_units == 0
    }

    /// Denomination label used in normalization diagnostics.
    fn denomination(self) -> String {
        format!("{} {}", self.currency.code(), self.unit.name())
    }
}

/// Parse a plain decimal string into exact 10^-12 units.
fn parse_pico_units(amount: &str) -> Result<u64, PricingError> {
    let (digits, negative) = match amount.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (amount, false),
    };
    let (integer, fraction) = match digits.split_once('.') {
        Some((integer, fraction)) => (integer, fraction),
        None => (digits, ""),
    };
    if integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.contains('.') && fraction.is_empty())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(PricingError::MalformedAmount);
    }
    if negative {
        return Err(PricingError::NegativeAmount);
    }

    let kept = PRICE_FRACTION_DIGITS;
    if fraction.len() > kept && fraction[kept..].bytes().any(|byte| byte != b'0') {
        return Err(PricingError::ExcessivePrecision);
    }
    let mut scaled = String::with_capacity(integer.len() + kept);
    scaled.push_str(integer);
    scaled.push_str(&fraction[..fraction.len().min(kept)]);
    for _ in fraction.len()..kept {
        scaled.push('0');
    }
    scaled
        .parse::<u64>()
        .map_err(|_| PricingError::AmountOutOfRange)
}

/// A distinct component of one request that providers bill separately.
///
/// Declaration order is the stable iteration and wire order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PriceComponent {
    /// Ordinary input tokens that were not served from a prompt cache.
    Input,
    /// Generated output tokens.
    Output,
    /// Input tokens served from a provider prompt cache.
    CachedInput,
    /// Input tokens written into a provider prompt cache.
    CacheWrite,
    /// Provider-billed internal reasoning tokens.
    Reasoning,
}

impl PriceComponent {
    /// Stable wire name for this component.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::CachedInput => "cached_input",
            Self::CacheWrite => "cache_write",
            Self::Reasoning => "reasoning",
        }
    }

    /// Parse an exact component name.
    ///
    /// # Errors
    /// Returns [`PricingError::UnknownComponent`] for any name outside the
    /// represented set. Provider-specific field names are the provider's own
    /// mapping concern and are not accepted here.
    pub fn parse(name: &str) -> Result<Self, PricingError> {
        match name {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            "cached_input" => Ok(Self::CachedInput),
            "cache_write" => Ok(Self::CacheWrite),
            "reasoning" => Ok(Self::Reasoning),
            other => Err(PricingError::UnknownComponent {
                name: other.to_owned(),
            }),
        }
    }
}

/// Normalized published pricing for one model.
///
/// A component is present only when the provider published it. A missing
/// component reads back as `None` and must never be treated as zero: unknown
/// cost is not free cost. All present components share one currency and one
/// billed unit, so a normalized row cannot silently mix denominations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelPricing {
    components: BTreeMap<PriceComponent, TokenPrice>,
    provenance: Option<ModelMetadataProvenance>,
}

impl ModelPricing {
    /// Pricing for a model whose provider published no price evidence.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            components: BTreeMap::new(),
            provenance: None,
        }
    }

    /// Start one captured pricing fact with its first published component.
    #[must_use]
    pub fn captured(
        provenance: ModelMetadataProvenance,
        component: PriceComponent,
        price: TokenPrice,
    ) -> Self {
        Self {
            components: BTreeMap::from([(component, price)]),
            provenance: Some(provenance),
        }
    }

    /// Add one published component.
    ///
    /// # Errors
    /// Returns [`PricingError::DuplicateComponent`] when the component was
    /// already published, and [`PricingError::InconsistentDenomination`] when
    /// its currency or unit disagrees with the components already present.
    /// [`PricingError::MissingProvenance`] is returned when called on
    /// [`Self::unknown`] instead of a value started with [`Self::captured`].
    pub fn with(
        mut self,
        component: PriceComponent,
        price: TokenPrice,
    ) -> Result<Self, PricingError> {
        if self.provenance.is_none() {
            return Err(PricingError::MissingProvenance);
        }
        if let Some(existing) = self.components.values().next()
            && (existing.currency() != price.currency() || existing.unit() != price.unit())
        {
            return Err(PricingError::InconsistentDenomination {
                existing: existing.denomination(),
                added: price.denomination(),
            });
        }
        if self.components.insert(component, price).is_some() {
            return Err(PricingError::DuplicateComponent {
                component: component.name(),
            });
        }
        Ok(self)
    }

    /// Published price for one component, or `None` when the provider
    /// published none. `None` means unknown, never free.
    #[must_use]
    pub fn price(&self, component: PriceComponent) -> Option<&TokenPrice> {
        self.components.get(&component)
    }

    /// Whether the provider published no price evidence at all.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.components.is_empty()
    }

    /// Source and capture instant for published components.
    ///
    /// Unknown pricing has no provenance; every non-empty value constructed by
    /// this API has one.
    #[must_use]
    pub const fn provenance(&self) -> Option<&ModelMetadataProvenance> {
        self.provenance.as_ref()
    }

    /// Published components in stable [`PriceComponent`] order.
    pub fn components(&self) -> impl Iterator<Item = (PriceComponent, &TokenPrice)> {
        self.components
            .iter()
            .map(|(component, price)| (*component, price))
    }

    /// Currency shared by every published component, if any exist.
    #[must_use]
    pub fn currency(&self) -> Option<PriceCurrency> {
        self.components
            .values()
            .next()
            .map(|price| price.currency())
    }

    /// Billed unit shared by every published component, if any exist.
    #[must_use]
    pub fn unit(&self) -> Option<TokenPriceUnit> {
        self.components.values().next().map(|price| price.unit())
    }
}

/// Safe performance normalization failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PerformanceError {
    /// An observation was recorded with nothing actually observed.
    #[error("observed performance contains no measurement")]
    NoObservation,
    /// Latency is zero or implausibly large.
    #[error("observed first-token latency is outside the plausible range")]
    InvalidLatency,
    /// Throughput is zero or implausibly large.
    #[error("observed output throughput is outside the plausible range")]
    InvalidThroughput,
}

/// Largest plausible observed time to first token, in milliseconds.
const MAX_FIRST_TOKEN_LATENCY_MS: u32 = 3_600_000;

/// Largest plausible observed output throughput, in milli-tokens per second.
const MAX_OUTPUT_THROUGHPUT_MILLI_TPS: u32 = 100_000_000;

/// Advisory observed performance evidence for one model.
///
/// This is a measurement someone took, not a guarantee the provider made, and
/// it is frequently absent. Request resolution MUST NOT read it: capability,
/// lifecycle and limit decisions are made from
/// [`ModelCapabilities`](crate::ModelCapabilities) and
/// [`ModelLifecycle`](crate::ModelLifecycle) alone. Use it for display and
/// operator choice only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelPerformance {
    first_token_latency_ms: Option<u32>,
    output_throughput_milli_tps: Option<u32>,
    provenance: Option<ModelMetadataProvenance>,
}

impl ModelPerformance {
    /// Performance for a model with no observed evidence.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            first_token_latency_ms: None,
            output_throughput_milli_tps: None,
            provenance: None,
        }
    }

    /// Record one observation with its exact source and capture instant.
    ///
    /// # Errors
    /// Returns [`PerformanceError::NoObservation`] when neither measurement is
    /// present (use [`Self::unknown`] instead), and
    /// [`PerformanceError::InvalidLatency`] /
    /// [`PerformanceError::InvalidThroughput`] for zero or implausible values.
    pub fn observed(
        provenance: ModelMetadataProvenance,
        first_token_latency_ms: Option<u32>,
        output_throughput_milli_tps: Option<u32>,
    ) -> Result<Self, PerformanceError> {
        if first_token_latency_ms.is_none() && output_throughput_milli_tps.is_none() {
            return Err(PerformanceError::NoObservation);
        }
        if let Some(latency) = first_token_latency_ms
            && (latency == 0 || latency > MAX_FIRST_TOKEN_LATENCY_MS)
        {
            return Err(PerformanceError::InvalidLatency);
        }
        if let Some(throughput) = output_throughput_milli_tps
            && (throughput == 0 || throughput > MAX_OUTPUT_THROUGHPUT_MILLI_TPS)
        {
            return Err(PerformanceError::InvalidThroughput);
        }
        Ok(Self {
            first_token_latency_ms,
            output_throughput_milli_tps,
            provenance: Some(provenance),
        })
    }

    /// Observed time to first token in milliseconds, when measured.
    #[must_use]
    pub const fn first_token_latency_ms(&self) -> Option<u32> {
        self.first_token_latency_ms
    }

    /// Observed output throughput in milli-tokens per second, when measured.
    #[must_use]
    pub const fn output_throughput_milli_tokens_per_second(&self) -> Option<u32> {
        self.output_throughput_milli_tps
    }

    /// Unix-millisecond instant the observation was collected, when present.
    #[must_use]
    pub fn observed_at_ms(&self) -> Option<u64> {
        self.provenance
            .as_ref()
            .map(ModelMetadataProvenance::captured_at_ms)
    }

    /// Source and capture instant for this observation.
    #[must_use]
    pub const fn provenance(&self) -> Option<&ModelMetadataProvenance> {
        self.provenance.as_ref()
    }

    /// Whether no performance evidence was observed.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        self.provenance.is_none()
    }
}
