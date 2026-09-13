//! C13: what a context budget may claim, and what it must refuse to claim.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::{
    BudgetUse, ContextProjection, ContributorTokens, EnvelopeContributor, EnvelopeEntry,
    EnvelopeTotal, EstimationMethod, TokenEnvelope, UncountedReason,
};

fn exact(contributor: EnvelopeContributor, tokens: u64) -> EnvelopeEntry {
    EnvelopeEntry::new(contributor, ContributorTokens::Exact(tokens))
}

fn estimated(contributor: EnvelopeContributor, tokens: u64) -> EnvelopeEntry {
    EnvelopeEntry::new(
        contributor,
        ContributorTokens::Estimated(tokens, EstimationMethod::Utf8ByteRatio),
    )
}

fn uncounted(contributor: EnvelopeContributor) -> EnvelopeEntry {
    EnvelopeEntry::new(
        contributor,
        ContributorTokens::Uncounted(UncountedReason::NoCounter),
    )
}

/// The row's acceptance: every contributor is explained, including the ones
/// nothing could count.
#[test]
fn every_contributor_appears_in_the_explanation() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 100))
        .with(exact(EnvelopeContributor::Messages, 400))
        .with(estimated(EnvelopeContributor::Tools, 60))
        .with(uncounted(EnvelopeContributor::Attachments));

    let projection = ContextProjection::explain(&envelope, Some(1_000));
    assert_eq!(projection.lines().len(), 4);
    let named: Vec<_> = projection
        .lines()
        .iter()
        .map(|line| line.contributor)
        .collect();
    assert!(named.contains(&EnvelopeContributor::Attachments));
    assert_eq!(
        projection.uncounted(),
        vec![(EnvelopeContributor::Attachments, UncountedReason::NoCounter)],
        "an uncounted contributor is exactly what a user cannot otherwise see"
    );
}

/// A complete measurement may divide; the numbers are the real ones.
#[test]
fn a_complete_measurement_reports_used_and_remaining_exactly() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 100))
        .with(exact(EnvelopeContributor::Messages, 400));

    let projection = ContextProjection::explain(&envelope, Some(1_000));
    assert_eq!(
        *projection.budget(),
        BudgetUse::Measured {
            used: 500,
            window: 1_000,
            remaining: 500
        }
    );
    assert!(projection.budget().is_exact_enough_to_divide());
    assert!(projection.is_complete());
}

/// The central rule: one uncounted contributor turns *every* derived quantity
/// into a bound. `used` becomes a floor and `remaining` becomes a ceiling,
/// because the missing tokens can only add.
#[test]
fn one_uncounted_contributor_makes_the_whole_budget_a_bound() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 100))
        .with(uncounted(EnvelopeContributor::Attachments));

    let projection = ContextProjection::explain(&envelope, Some(1_000));
    assert_eq!(
        *projection.budget(),
        BudgetUse::AtLeast {
            used_at_least: 100,
            window: 1_000,
            remaining_at_most: 900
        }
    );
    assert!(!projection.budget().is_exact_enough_to_divide());
    assert!(!projection.is_complete());
}

/// A share of a lower bound overstates every counted contributor, because the
/// denominator is too small. So no line reports a share while any contributor
/// is uncounted — including the lines that were themselves counted exactly.
#[test]
fn no_contributor_reports_a_share_of_an_incomplete_total() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 250))
        .with(uncounted(EnvelopeContributor::Attachments));

    let projection = ContextProjection::explain(&envelope, Some(1_000));
    for line in projection.lines() {
        assert_eq!(
            line.share_of(projection.total()),
            None,
            "{:?} must not report a share of a lower bound",
            line.contributor
        );
    }
}

#[test]
fn a_counted_contributor_reports_its_share_of_a_complete_total() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 250))
        .with(exact(EnvelopeContributor::Messages, 750));

    let projection = ContextProjection::explain(&envelope, Some(4_000));
    let system = projection
        .lines()
        .iter()
        .find(|line| line.contributor == EnvelopeContributor::System)
        .unwrap();
    assert_eq!(system.share_of(projection.total()), Some(0.25));
}

/// A model that publishes no window has no budget to divide into. That is not
/// zero headroom — it is no known headroom, and the measurement still stands.
#[test]
fn an_unpublished_window_yields_no_budget_rather_than_zero_headroom() {
    let envelope = TokenEnvelope::new().with(exact(EnvelopeContributor::System, 100));

    let projection = ContextProjection::explain(&envelope, None);
    assert_eq!(
        *projection.budget(),
        BudgetUse::WindowUnknown {
            total: EnvelopeTotal::Exact(100)
        }
    );
    assert!(!projection.budget().known_to_overflow());
    assert!(!projection.budget().is_exact_enough_to_divide());
}

/// The one thing an incomplete measurement *can* prove: uncounted contributors
/// only add, so a lower bound past the window is a real overflow.
#[test]
fn a_lower_bound_past_the_window_still_proves_an_overflow() {
    let over = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::Messages, 5_000))
        .with(uncounted(EnvelopeContributor::Attachments));
    assert!(
        ContextProjection::explain(&over, Some(4_000))
            .budget()
            .known_to_overflow()
    );

    let under = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::Messages, 100))
        .with(uncounted(EnvelopeContributor::Attachments));
    assert!(
        !ContextProjection::explain(&under, Some(4_000))
            .budget()
            .known_to_overflow(),
        "an incomplete measurement under the window proves nothing either way"
    );
}

/// A request already over its window reports no remaining headroom rather than
/// wrapping to an enormous one.
#[test]
fn an_overflowing_request_saturates_remaining_at_zero() {
    let envelope = TokenEnvelope::new().with(exact(EnvelopeContributor::Messages, 9_000));
    let projection = ContextProjection::explain(&envelope, Some(4_000));
    assert_eq!(
        *projection.budget(),
        BudgetUse::Measured {
            used: 9_000,
            window: 4_000,
            remaining: 0
        }
    );
    assert!(projection.budget().known_to_overflow());
}

/// An estimated contributor is still counted, so the budget divides — but the
/// total keeps its weaker evidence so a surface can say so.
#[test]
fn an_estimate_still_divides_but_the_total_remembers_it_was_estimated() {
    let envelope = TokenEnvelope::new()
        .with(exact(EnvelopeContributor::System, 100))
        .with(estimated(EnvelopeContributor::Messages, 300));

    let projection = ContextProjection::explain(&envelope, Some(1_000));
    assert!(projection.is_complete());
    assert_eq!(*projection.total(), EnvelopeTotal::Estimated(400));
    assert!(matches!(projection.budget(), BudgetUse::Measured { .. }));
}

#[test]
fn an_empty_envelope_explains_nothing_rather_than_claiming_zero_use() {
    let projection = ContextProjection::explain(&TokenEnvelope::new(), Some(1_000));
    assert!(projection.lines().is_empty());
    assert!(projection.uncounted().is_empty());
    // Zero counted tokens with no contributors is a real, complete measurement
    // of an empty request — but no line can claim a share of it.
    assert_eq!(*projection.total(), EnvelopeTotal::Exact(0));
}
