//! Model-specific context budget calculation.

use crate::EnvelopeTotal;
use heycode_core::{ContextActivity, ContextBudget, ContextConfidence, ContextLimitSource};

/// Resolve one budget. A zero configured window means model-driven limits.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn context_budget(
    provider: String,
    model: String,
    total: &EnvelopeTotal,
    model_window: Option<u64>,
    configured_window: u64,
    output_reserve: u64,
    threshold_ratio: f32,
    auto_compact: bool,
) -> ContextBudget {
    let configured = (configured_window > 0).then_some(configured_window);
    let (window, limit_source) = match (model_window.filter(|w| *w > 0), configured) {
        (Some(model), Some(cap)) if cap < model => (Some(cap), ContextLimitSource::ConfiguredCap),
        (Some(model), _) => (Some(model), ContextLimitSource::Model),
        (None, Some(cap)) => (Some(cap), ContextLimitSource::ConfiguredFallback),
        (None, None) => (None, ContextLimitSource::Unknown),
    };
    let confidence = match total {
        EnvelopeTotal::Exact(_) => ContextConfidence::Exact,
        EnvelopeTotal::Estimated(_) => ContextConfidence::Estimated,
        EnvelopeTotal::AtLeast { .. } => ContextConfidence::AtLeast,
    };
    let safety_margin = window.map_or(0, |w| (w / 100).min(4096));
    let usable_input = window.map(|w| {
        w.saturating_sub(output_reserve)
            .saturating_sub(safety_margin)
    });
    let ratio = if threshold_ratio.is_finite() && threshold_ratio > 0.0 {
        threshold_ratio.min(1.0)
    } else {
        0.8
    };
    let compact_at = window
        .zip(usable_input)
        .map(|(w, usable)| ((w as f64 * f64::from(ratio)) as u64).min(usable));
    ContextBudget {
        provider,
        model,
        used: total.counted(),
        confidence,
        window,
        limit_source,
        output_reserve,
        safety_margin,
        usable_input,
        compact_at,
        auto_compact,
        activity: ContextActivity::Ready,
        before_compaction: None,
        after_compaction: None,
        projected: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(window: Option<u64>, cap: u64, total: EnvelopeTotal) -> ContextBudget {
        context_budget(
            "provider".into(),
            "model".into(),
            &total,
            window,
            cap,
            8192,
            0.8,
            true,
        )
    }

    #[test]
    fn active_model_window_controls_pressure_instead_of_fixed_default() {
        let small = budget(Some(128_000), 0, EnvelopeTotal::Exact(110_000));
        let large = budget(Some(1_048_576), 0, EnvelopeTotal::Exact(110_000));
        assert!(small.should_compact());
        assert!(!large.should_compact());
        assert_eq!(large.window, Some(1_048_576));
        assert_eq!(large.limit_source, ContextLimitSource::Model);
    }

    #[test]
    fn reserves_can_trigger_before_the_configured_percentage() {
        let budget = context_budget(
            "p".into(),
            "m".into(),
            &EnvelopeTotal::Exact(75_000),
            Some(100_000),
            0,
            30_000,
            0.8,
            true,
        );
        assert_eq!(budget.usable_input, Some(69_000));
        assert_eq!(budget.compact_at, Some(69_000));
        assert!(budget.exceeds_usable_input());
        assert_eq!(budget.remaining(), Some(0));
    }

    #[test]
    fn explicit_caps_cannot_inflate_published_capacity() {
        assert_eq!(
            budget(Some(100_000), 200_000, EnvelopeTotal::Exact(1)).window,
            Some(100_000)
        );
        let capped = budget(Some(100_000), 50_000, EnvelopeTotal::Exact(1));
        assert_eq!(capped.window, Some(50_000));
        assert_eq!(capped.limit_source, ContextLimitSource::ConfiguredCap);
        assert_eq!(
            budget(None, 50_000, EnvelopeTotal::Exact(1)).limit_source,
            ContextLimitSource::ConfiguredFallback
        );
    }

    #[test]
    fn unknown_capacity_and_incomplete_counts_remain_explicit() {
        let unknown = budget(None, 0, EnvelopeTotal::Estimated(4_000));
        assert_eq!(unknown.window, None);
        assert_eq!(unknown.remaining(), None);
        assert!(!unknown.should_compact());
        let partial = budget(
            Some(100_000),
            0,
            EnvelopeTotal::AtLeast {
                counted: 4_000,
                uncounted: vec![],
            },
        );
        assert_eq!(partial.confidence, ContextConfidence::AtLeast);
    }

    #[test]
    fn disabled_auto_compaction_does_not_disable_capacity_guard() {
        let mut budget = budget(Some(100_000), 0, EnvelopeTotal::Exact(100_001));
        budget.auto_compact = false;
        assert!(!budget.should_compact());
        assert!(budget.exceeds_usable_input());
    }
}
