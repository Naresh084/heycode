//! C13 `/context` projection: what each contributor costs, and what the budget
//! can and cannot say.
//!
//! C11 measures an envelope; this explains one. The distinction that shapes
//! everything here is that an **uncounted contributor has no number**, so a
//! budget containing one is a lower bound and every derived quantity —
//! remaining headroom, percentage used — is a bound too. A projection that
//! rendered "62% used" from an incomplete measurement would be inventing the
//! missing tokens and hiding that it had.

use crate::{
    ContributorTokens, EnvelopeContributor, EnvelopeTotal, TokenCountRefusal, TokenEnvelope,
    UncountedReason,
};

/// How much of a model's window a measurement accounts for.
///
/// Deliberately has no plain `percent()` accessor. A caller must match, because
/// a percentage computed from a lower bound is a different claim from one
/// computed from a complete count, and the two must not look alike.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetUse {
    /// Every contributor was counted and the window is known.
    Measured {
        /// Tokens the request will occupy.
        used: u64,
        /// The model's input window.
        window: u64,
        /// Tokens still available. Saturates at zero rather than wrapping when
        /// a request already exceeds its window.
        remaining: u64,
    },
    /// At least one contributor is uncounted, so `used` is a floor and
    /// `remaining` is a ceiling: the real usage is this or more.
    AtLeast {
        /// Lower bound on tokens the request will occupy.
        used_at_least: u64,
        /// The model's input window.
        window: u64,
        /// Upper bound on tokens still available.
        remaining_at_most: u64,
    },
    /// The model publishes no input window, so no budget exists to divide into.
    /// Not zero headroom — no known headroom.
    WindowUnknown {
        /// What the measurement did establish.
        total: EnvelopeTotal,
    },
}

impl BudgetUse {
    /// True only when every contributor was counted and the window is known.
    #[must_use]
    pub const fn is_exact_enough_to_divide(&self) -> bool {
        matches!(self, Self::Measured { .. })
    }

    /// Whether the request is known to exceed its window.
    ///
    /// A lower bound past the window still proves an overflow — that is the one
    /// thing an incomplete measurement *can* prove, because uncounted
    /// contributors only add.
    #[must_use]
    pub const fn known_to_overflow(&self) -> bool {
        match self {
            Self::Measured { used, window, .. } => *used > *window,
            Self::AtLeast {
                used_at_least,
                window,
                ..
            } => *used_at_least > *window,
            Self::WindowUnknown { .. } => false,
        }
    }
}

/// One contributor's line in the explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributorLine {
    /// Which part of the request this is.
    pub contributor: EnvelopeContributor,
    /// What is known about its cost.
    pub tokens: ContributorTokens,
    /// Better-ranked counters that refused before the selected evidence.
    pub refusals: Vec<TokenCountRefusal>,
}

impl ContributorLine {
    /// Share of the counted total, when this contributor was counted **and**
    /// the total is complete.
    ///
    /// `None` for an uncounted contributor, and also `None` when any *other*
    /// contributor is uncounted: a share of a lower bound overstates every
    /// counted contributor, because the denominator is too small.
    #[must_use]
    pub fn share_of(&self, total: &EnvelopeTotal) -> Option<f64> {
        if !total.is_complete() {
            return None;
        }
        let counted = self.tokens.counted()?;
        let denominator = total.counted();
        if denominator == 0 {
            return None;
        }
        // Both are token counts bounded by a context window, so the conversion
        // is exact in f64 for every value this can hold.
        Some(counted as f64 / denominator as f64)
    }
}

/// The complete `/context` explanation for one request envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextProjection {
    lines: Vec<ContributorLine>,
    total: EnvelopeTotal,
    budget: BudgetUse,
}

impl ContextProjection {
    /// Explain one envelope against an optional model input window.
    ///
    /// `window` is `None` when the model publishes no context window — the
    /// catalog's honest answer for many providers — and that yields
    /// [`BudgetUse::WindowUnknown`] rather than a fabricated denominator.
    #[must_use]
    pub fn explain(envelope: &TokenEnvelope, window: Option<u64>) -> Self {
        let total = envelope.total();
        let lines = envelope
            .entries()
            .iter()
            .map(|entry| ContributorLine {
                contributor: entry.contributor(),
                tokens: entry.tokens().clone(),
                refusals: entry.refusals().to_vec(),
            })
            .collect();
        let budget = match (window, &total) {
            (None, _) => BudgetUse::WindowUnknown {
                total: total.clone(),
            },
            (Some(window), EnvelopeTotal::Exact(used) | EnvelopeTotal::Estimated(used)) => {
                BudgetUse::Measured {
                    used: *used,
                    window,
                    remaining: window.saturating_sub(*used),
                }
            }
            (Some(window), EnvelopeTotal::AtLeast { counted, .. }) => BudgetUse::AtLeast {
                used_at_least: *counted,
                window,
                remaining_at_most: window.saturating_sub(*counted),
            },
        };
        Self {
            lines,
            total,
            budget,
        }
    }

    /// Every contributor the envelope carried, in stable order.
    #[must_use]
    pub fn lines(&self) -> &[ContributorLine] {
        &self.lines
    }

    /// The envelope total, with its evidence intact.
    #[must_use]
    pub const fn total(&self) -> &EnvelopeTotal {
        &self.total
    }

    /// How the measurement relates to the model's window.
    #[must_use]
    pub const fn budget(&self) -> &BudgetUse {
        &self.budget
    }

    /// Contributors nothing could count, with the reason each was missed.
    ///
    /// A surface must show these: they are exactly the part of the context the
    /// user cannot see, and omitting them makes the rest look complete.
    #[must_use]
    pub fn uncounted(&self) -> Vec<(EnvelopeContributor, UncountedReason)> {
        self.lines
            .iter()
            .filter_map(|line| match &line.tokens {
                ContributorTokens::Uncounted(reason) => Some((line.contributor, *reason)),
                ContributorTokens::Exact(_) | ContributorTokens::Estimated(..) => None,
            })
            .collect()
    }

    /// Whether every contributor in the request was counted.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.total.is_complete()
    }
}
