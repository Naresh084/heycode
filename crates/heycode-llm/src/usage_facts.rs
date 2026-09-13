//! P12 — rate-limit and cost facts a provider actually reported.
//!
//! `/usage` has to show what is true, and a usage display that guesses is worse
//! than one that says "unknown": an operator throttles or budgets against it.
//! So two rules run through this module.
//!
//! * **Header names belong to providers, not here.** `heycode-llm` owns the
//!   vocabulary and the parsing; each provider supplies its own
//!   [`RateLimitHeaders`] mapping. Baking one provider's spelling into the
//!   shared layer would silently report nothing for every other provider while
//!   looking like it worked.
//! * **Reported and derived are different facts.** A cost the provider stated
//!   and a cost computed from a published price are both useful and must never
//!   be confused, because only one of them is authoritative.
//!
//! Unlike a catalog generation — where one malformed row rejects everything,
//! because a catalog is authoritative data — a rate-limit snapshot is advisory
//! telemetry. One unparseable header drops that single field rather than
//! discarding the fields that did parse.

use std::collections::BTreeMap;

use heycode_core::TokenUsage;

use crate::model_pricing::{ModelPricing, PriceComponent, PriceCurrency, TokenPriceUnit};

/// Which quota a rate-limit window describes.
///
/// Declaration order is the stable presentation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RateLimitScope {
    /// Requests per window.
    Requests,
    /// Input tokens per window.
    InputTokens,
    /// Output tokens per window.
    OutputTokens,
    /// A single combined quota, where a provider publishes one rather than
    /// separate request and token windows.
    Unified,
}

impl RateLimitScope {
    /// Stable name for diagnostics and display.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Requests => "requests",
            Self::InputTokens => "input_tokens",
            Self::OutputTokens => "output_tokens",
            Self::Unified => "unified",
        }
    }
}

/// One quota window. Every field is independently optional: a provider that
/// publishes only `remaining` yields exactly that, and absence is never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimitWindow {
    /// Total allowance for the window, when published.
    pub limit: Option<u64>,
    /// Allowance left, when published.
    pub remaining: Option<u64>,
    /// Unix-millisecond instant the window resets, when published.
    pub reset_at_ms: Option<u64>,
}

impl RateLimitWindow {
    /// Whether this window carries no evidence at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.limit.is_none() && self.remaining.is_none() && self.reset_at_ms.is_none()
    }
}

/// Which response headers one provider uses for one quota window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitHeaderNames {
    /// Header carrying the window's total allowance.
    pub limit: Option<&'static str>,
    /// Header carrying the remaining allowance.
    pub remaining: Option<&'static str>,
    /// Header carrying the reset instant, as RFC 3339 or as seconds-from-now.
    pub reset: Option<&'static str>,
}

/// A provider's complete rate-limit header vocabulary.
///
/// Providers declare their own spellings because there is no cross-provider
/// standard beyond `Retry-After`. Nothing is assumed on a provider's behalf.
#[derive(Debug, Clone, Default)]
pub struct RateLimitHeaders {
    windows: BTreeMap<RateLimitScope, RateLimitHeaderNames>,
}

impl RateLimitHeaders {
    /// An empty vocabulary: a provider that publishes nothing yields nothing.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Declare the headers one scope uses.
    #[must_use]
    pub fn with(mut self, scope: RateLimitScope, names: RateLimitHeaderNames) -> Self {
        self.windows.insert(scope, names);
        self
    }

    /// Whether this provider declared any window.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

/// What one response said about remaining quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    windows: BTreeMap<RateLimitScope, RateLimitWindow>,
    retry_after_ms: Option<u64>,
    observed_at_ms: u64,
}

impl RateLimitSnapshot {
    /// Parse one provider's declared headers out of a bounded response-header
    /// map, using `observed_at_ms` as the instant relative resets resolve
    /// against.
    ///
    /// Header keys are matched case-insensitively. An absent or unparseable
    /// header drops that one field; it never invents a value and never
    /// discards the fields that did parse.
    #[must_use]
    pub fn parse(
        headers: &BTreeMap<String, String>,
        names: &RateLimitHeaders,
        observed_at_ms: u64,
    ) -> Self {
        let lookup = |name: Option<&'static str>| -> Option<&str> {
            let name = name?;
            headers.get(&name.to_ascii_lowercase()).map(String::as_str)
        };
        let mut windows = BTreeMap::new();
        for (scope, header_names) in &names.windows {
            let window = RateLimitWindow {
                limit: lookup(header_names.limit).and_then(parse_count),
                remaining: lookup(header_names.remaining).and_then(parse_count),
                reset_at_ms: lookup(header_names.reset)
                    .and_then(|value| parse_reset(value, observed_at_ms)),
            };
            // A window with no parsed field is absence, not an empty quota.
            if !window.is_empty() {
                windows.insert(*scope, window);
            }
        }
        Self {
            windows,
            // `Retry-After` is the one cross-provider standard (RFC 9110), so
            // it is read without a provider declaring it.
            retry_after_ms: headers
                .get("retry-after")
                .and_then(|value| parse_reset(value, observed_at_ms))
                .map(|reset| reset.saturating_sub(observed_at_ms)),
            observed_at_ms,
        }
    }

    /// Windows in stable scope order.
    pub fn windows(&self) -> impl Iterator<Item = (RateLimitScope, RateLimitWindow)> + '_ {
        self.windows.iter().map(|(scope, window)| (*scope, *window))
    }

    /// One window's evidence, when the provider published any of it.
    #[must_use]
    pub fn window(&self, scope: RateLimitScope) -> Option<RateLimitWindow> {
        self.windows.get(&scope).copied()
    }

    /// Milliseconds the provider asked the caller to wait, when it said so.
    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// Instant this snapshot describes. A rate-limit fact without its instant
    /// is unusable, so it is required rather than optional.
    #[must_use]
    pub const fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }

    /// Whether the provider published nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.retry_after_ms.is_none()
    }
}

/// What one completed request cost.
///
/// `Reported` and `Derived` are deliberately distinct: only the first is the
/// provider's own statement, and a `/usage` display that merged them would
/// present arithmetic as authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestCost {
    /// The provider stated this cost itself.
    Reported {
        /// Cost in 10^-12 units of `currency`.
        pico_units: u128,
        /// Currency the provider reported in.
        currency: PriceCurrency,
    },
    /// Computed from published per-token prices and reported usage. Accurate
    /// only insofar as the published price is current and complete.
    Derived {
        /// Cost in 10^-12 units of `currency`.
        pico_units: u128,
        /// Currency the price was published in.
        currency: PriceCurrency,
    },
    /// Neither reported nor derivable. Unknown cost is **not** zero.
    Unknown,
}

impl RequestCost {
    /// Take a provider's own statement.
    #[must_use]
    pub const fn reported(pico_units: u128, currency: PriceCurrency) -> Self {
        Self::Reported {
            pico_units,
            currency,
        }
    }

    /// Derive a cost from published prices and reported usage.
    ///
    /// Requires **both** the input and output prices: charging output tokens at
    /// the input rate, or ignoring them, would understate the bill. A model
    /// missing either component yields [`Self::Unknown`].
    #[must_use]
    pub fn derive(usage: TokenUsage, pricing: &ModelPricing) -> Self {
        let (Some(input), Some(output)) = (
            pricing.price(PriceComponent::Input),
            pricing.price(PriceComponent::Output),
        ) else {
            return Self::Unknown;
        };
        Self::Derived {
            pico_units: component_cost(usage.prompt_tokens, input)
                .saturating_add(component_cost(usage.completion_tokens, output)),
            currency: input.currency(),
        }
    }

    /// Derive a cache-aware cost from an exact provider partition.
    ///
    /// Cache reads/writes require their matching published prices when used.
    /// A response without an explicit uncached partition cannot be priced when
    /// cache activity is non-zero, because provider subsets may overlap.
    #[must_use]
    pub fn derive_detailed(
        usage: heycode_core::ProviderCacheUsage,
        pricing: &ModelPricing,
    ) -> Self {
        if usage.reported_cache_write_tokens().is_none() {
            return Self::Unknown;
        }
        let (Some(input), Some(output)) = (
            pricing.price(PriceComponent::Input),
            pricing.price(PriceComponent::Output),
        ) else {
            return Self::Unknown;
        };
        let ordinary = match usage.uncached_input_tokens() {
            Some(tokens) => tokens,
            None if usage.cache_read_tokens() == 0 && usage.cache_write_tokens() == 0 => {
                usage.input_tokens()
            }
            None => return Self::Unknown,
        };
        let mut pico_units = component_cost(ordinary, input)
            .saturating_add(component_cost(usage.output_tokens(), output));
        if usage.cache_read_tokens() > 0 {
            let Some(price) = pricing.price(PriceComponent::CachedInput) else {
                return Self::Unknown;
            };
            pico_units =
                pico_units.saturating_add(component_cost(usage.cache_read_tokens(), price));
        }
        if usage.cache_write_tokens() > 0 {
            let Some(price) = pricing.price(PriceComponent::CacheWrite) else {
                return Self::Unknown;
            };
            pico_units =
                pico_units.saturating_add(component_cost(usage.cache_write_tokens(), price));
        }
        Self::Derived {
            pico_units,
            currency: input.currency(),
        }
    }

    /// Cost in pico-units, or `None` when unknown.
    ///
    /// Deliberately an `Option`: an unknown cost has no number, and returning
    /// zero would read as free.
    #[must_use]
    pub const fn pico_units(&self) -> Option<u128> {
        match self {
            Self::Reported { pico_units, .. } | Self::Derived { pico_units, .. } => {
                Some(*pico_units)
            }
            Self::Unknown => None,
        }
    }

    /// Whether the provider itself stated this cost.
    #[must_use]
    pub const fn is_reported(&self) -> bool {
        matches!(self, Self::Reported { .. })
    }
}

fn component_cost(tokens: u64, price: &crate::model_pricing::TokenPrice) -> u128 {
    let tokens = u128::from(tokens);
    let per_unit = u128::from(price.pico_units());
    match price.unit() {
        TokenPriceUnit::PerToken => tokens.saturating_mul(per_unit),
        // Truncating keeps a derived cost a lower bound rather than rounding a
        // bill upward.
        TokenPriceUnit::PerMillionTokens => {
            tokens.saturating_mul(per_unit).saturating_div(1_000_000)
        }
    }
}

/// Parse a non-negative integer header value.
fn parse_count(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

/// Parse a reset header as either RFC 3339 or seconds-from-now.
///
/// Providers publish both spellings, so both are accepted; anything else drops
/// the field rather than being coerced into a plausible instant.
fn parse_reset(value: &str, observed_at_ms: u64) -> Option<u64> {
    let value = value.trim();
    if let Ok(instant) = chrono::DateTime::parse_from_rfc3339(value) {
        return u64::try_from(instant.timestamp_millis()).ok();
    }
    // A bare number is seconds from now, per RFC 9110's `Retry-After`
    // delta-seconds form.
    let seconds: u64 = value.parse().ok()?;
    Some(observed_at_ms.saturating_add(seconds.saturating_mul(1_000)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model_pricing::{ModelMetadataProvenance, TokenPrice};

    const NOW: u64 = 1_787_752_741_000;

    fn provenance() -> ModelMetadataProvenance {
        ModelMetadataProvenance::new("test:usage-facts", NOW).unwrap()
    }

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
            .collect()
    }

    /// The exact `anthropic-ratelimit-unified-*` spellings and `retry-after`,
    /// read from the shipped Claude Code 2.1.250 implementation.
    fn anthropic_headers() -> RateLimitHeaders {
        RateLimitHeaders::none().with(
            RateLimitScope::Unified,
            RateLimitHeaderNames {
                limit: None,
                remaining: Some("anthropic-ratelimit-unified-utilization"),
                reset: Some("anthropic-ratelimit-unified-reset"),
            },
        )
    }

    #[test]
    fn a_provider_that_declares_nothing_reports_nothing() {
        // The shared layer must not assume any provider's header spelling.
        let snapshot = RateLimitSnapshot::parse(
            &headers(&[("anthropic-ratelimit-unified-reset", "30")]),
            &RateLimitHeaders::none(),
            NOW,
        );
        assert!(snapshot.is_empty());
        assert!(snapshot.window(RateLimitScope::Unified).is_none());
        assert_eq!(snapshot.observed_at_ms(), NOW);
    }

    #[test]
    fn declared_headers_parse_case_insensitively_in_both_reset_spellings() {
        let relative = RateLimitSnapshot::parse(
            &headers(&[
                ("Anthropic-RateLimit-Unified-Utilization", "12"),
                ("Anthropic-RateLimit-Unified-Reset", "30"),
            ]),
            &anthropic_headers(),
            NOW,
        );
        let window = relative.window(RateLimitScope::Unified).unwrap();
        assert_eq!(window.remaining, Some(12));
        assert_eq!(window.reset_at_ms, Some(NOW + 30_000));
        assert_eq!(window.limit, None, "an undeclared field stays absent");

        let absolute = RateLimitSnapshot::parse(
            &headers(&[("anthropic-ratelimit-unified-reset", "2026-08-29T00:00:00Z")]),
            &anthropic_headers(),
            NOW,
        );
        assert_eq!(
            absolute
                .window(RateLimitScope::Unified)
                .unwrap()
                .reset_at_ms,
            Some(1_787_961_600_000)
        );
    }

    #[test]
    fn one_unparseable_header_drops_only_that_field() {
        // A rate-limit snapshot is advisory telemetry, not authoritative data:
        // discarding the fields that parsed would lose real information.
        let snapshot = RateLimitSnapshot::parse(
            &headers(&[
                ("anthropic-ratelimit-unified-utilization", "not-a-number"),
                ("anthropic-ratelimit-unified-reset", "45"),
            ]),
            &anthropic_headers(),
            NOW,
        );
        let window = snapshot.window(RateLimitScope::Unified).unwrap();
        assert_eq!(window.remaining, None, "malformed is absent, never zero");
        assert_eq!(window.reset_at_ms, Some(NOW + 45_000));
    }

    #[test]
    fn retry_after_is_read_without_a_provider_declaring_it() {
        // RFC 9110 is the one cross-provider standard here.
        let snapshot = RateLimitSnapshot::parse(
            &headers(&[("Retry-After", "20")]),
            &RateLimitHeaders::none(),
            NOW,
        );
        assert_eq!(snapshot.retry_after_ms(), Some(20_000));
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn a_window_with_nothing_parsed_is_absent_rather_than_an_empty_quota() {
        let snapshot = RateLimitSnapshot::parse(&headers(&[]), &anthropic_headers(), NOW);
        assert!(
            snapshot.window(RateLimitScope::Unified).is_none(),
            "an unpublished window must not read as a zero quota"
        );
        assert!(snapshot.is_empty());
    }

    fn priced(input: &str, output: &str) -> ModelPricing {
        let price = |amount: &str| {
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, amount).unwrap()
        };
        ModelPricing::captured(provenance(), PriceComponent::Input, price(input))
            .with(PriceComponent::Output, price(output))
            .unwrap()
    }

    #[test]
    fn a_derived_cost_is_never_presented_as_a_reported_one() {
        let usage = TokenUsage {
            prompt_tokens: 1_000,
            completion_tokens: 100,
        };
        let derived = RequestCost::derive(usage, &priced("0.000000075", "0.00000025"));
        // 1000 x 75_000 + 100 x 250_000
        assert_eq!(
            derived,
            RequestCost::Derived {
                pico_units: 75_000_000 + 25_000_000,
                currency: PriceCurrency::Usd
            }
        );
        assert!(!derived.is_reported(), "arithmetic is not authority");

        let reported = RequestCost::reported(99, PriceCurrency::Usd);
        assert!(reported.is_reported());
        assert_ne!(
            reported,
            RequestCost::Derived {
                pico_units: 99,
                currency: PriceCurrency::Usd
            },
            "equal numbers with different provenance are different facts"
        );
    }

    #[test]
    fn a_partially_priced_model_yields_unknown_rather_than_a_half_cost() {
        let usage = TokenUsage {
            prompt_tokens: 1_000,
            completion_tokens: 100,
        };
        // Input-only pricing would silently bill output at zero.
        let input_only = ModelPricing::captured(
            provenance(),
            PriceComponent::Input,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerToken, "0.000000075")
                .unwrap(),
        );
        assert_eq!(
            RequestCost::derive(usage, &input_only),
            RequestCost::Unknown
        );
        assert_eq!(
            RequestCost::derive(usage, &ModelPricing::unknown()),
            RequestCost::Unknown
        );
        assert_eq!(
            RequestCost::Unknown.pico_units(),
            None,
            "unknown is not zero"
        );
    }

    #[test]
    fn detailed_cache_cost_requires_partition_and_every_used_price() {
        let price = |pico_units| {
            TokenPrice::from_pico_units(PriceCurrency::Usd, TokenPriceUnit::PerToken, pico_units)
                .unwrap()
        };
        let pricing = ModelPricing::captured(provenance(), PriceComponent::Input, price(10))
            .with(PriceComponent::Output, price(20))
            .unwrap()
            .with(PriceComponent::CachedInput, price(2))
            .unwrap()
            .with(PriceComponent::CacheWrite, price(5))
            .unwrap();
        let partitioned = heycode_core::ProviderCacheUsage::new(100, 20, 30, 10)
            .unwrap()
            .with_uncached_input_tokens(60)
            .unwrap();
        assert_eq!(
            RequestCost::derive_detailed(partitioned, &pricing),
            RequestCost::Derived {
                pico_units: 1_110,
                currency: PriceCurrency::Usd,
            }
        );

        let ambiguous = heycode_core::ProviderCacheUsage::new(100, 20, 30, 10).unwrap();
        assert_eq!(
            RequestCost::derive_detailed(ambiguous, &pricing),
            RequestCost::Unknown,
            "cache subsets without an uncached partition cannot be priced safely"
        );
        let missing_cache_write =
            ModelPricing::captured(provenance(), PriceComponent::Input, price(10))
                .with(PriceComponent::Output, price(20))
                .unwrap()
                .with(PriceComponent::CachedInput, price(2))
                .unwrap();
        assert_eq!(
            RequestCost::derive_detailed(partitioned, &missing_cache_write),
            RequestCost::Unknown
        );
    }

    #[test]
    fn a_provider_declares_no_rate_limit_headers_until_it_verifies_them() {
        use crate::{Provider, ProviderInfo};

        struct Unverified;
        impl Provider for Unverified {
            fn info(&self) -> ProviderInfo {
                ProviderInfo {
                    name: "unverified".to_owned(),
                    default_model: "none".to_owned(),
                }
            }
            fn stream(&self, _request: crate::ChatRequest) -> crate::ChunkStream {
                Box::pin(futures::stream::empty())
            }
        }
        // The default must declare nothing: names that were not verified
        // against the provider would make a usage display show a guess.
        assert!(Unverified.rate_limit_headers().is_empty());
        let snapshot = RateLimitSnapshot::parse(
            &headers(&[("anthropic-ratelimit-unified-reset", "30")]),
            &Unverified.rate_limit_headers(),
            NOW,
        );
        assert!(
            snapshot.window(RateLimitScope::Unified).is_none(),
            "a provider that declared nothing must report nothing, even when a \
             header it did not claim happens to be present"
        );
    }

    #[test]
    fn a_per_million_price_derives_without_rounding_a_bill_upward() {
        let pricing = ModelPricing::captured(
            provenance(),
            PriceComponent::Input,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerMillionTokens, "3.0")
                .unwrap(),
        )
        .with(
            PriceComponent::Output,
            TokenPrice::parse_decimal(PriceCurrency::Usd, TokenPriceUnit::PerMillionTokens, "15.0")
                .unwrap(),
        )
        .unwrap();
        let cost = RequestCost::derive(
            TokenUsage {
                prompt_tokens: 1_000,
                completion_tokens: 1_000,
            },
            &pricing,
        );
        assert_eq!(
            cost.pico_units(),
            Some(3_000_000_000 + 15_000_000_000),
            "per-million prices scale exactly"
        );
        // A single token at a per-million price truncates down, never up.
        let single = RequestCost::derive(
            TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 0,
            },
            &pricing,
        );
        assert_eq!(single.pico_units(), Some(3_000_000));
    }
}
