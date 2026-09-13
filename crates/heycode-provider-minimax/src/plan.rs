//! MiniMax's two commercially distinct credential products.
//!
//! MiniMax sells the same models twice: metered *pay-as-you-go* against an
//! account balance, and a fixed-price *Token Plan* subscription. The two carry
//! separate credentials that MiniMax states are **not interchangeable**, yet
//! MiniMax's own guides put both under the single `MINIMAX_API_KEY`
//! environment variable. heycode therefore treats the plan as identity, not as a
//! label: every plan fact hangs off [`MiniMaxPlanId`], and each plan also has
//! a zero-sized marker type so a value can be bound to one plan at compile
//! time.
//!
//! Sources:
//! - <https://platform.minimax.io/docs/guides/quickstart-preparation> —
//!   pay-as-you-go **API Key** vs Token Plan **Subscription Key**, and
//!   `export MINIMAX_API_KEY=${YOUR_API_KEY}`.
//! - <https://platform.minimax.io/docs/token-plan/intro> — "The Subscription
//!   Key is not interchangeable with pay-as-you-go API Keys."
//! - <https://platform.minimax.io/docs/token-plan/faq> — asked whether the two
//!   may be used interchangeably, the answer is "No, they cannot".
//! - <https://platform.minimax.io/docs/token-plan/openclaw> — "Set the
//!   `MINIMAX_API_KEY` environment variable (Token Plan API Key starts with
//!   `sk-cp`)".
//! - <https://platform.minimax.io/docs/token-plan/other-tools> — "The
//!   subscription key uses the prefix `sk-cp-…`".

use std::fmt;

/// Which of MiniMax's two credential products a value belongs to.
///
/// The enum is deliberately closed. A third MiniMax product must break every
/// consumer that decides per plan rather than being absorbed by a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxPlanId {
    /// Metered usage billed against the account balance, authenticated with
    /// the platform **API Key**.
    PayAsYouGo,
    /// Fixed-price subscription plus purchased Credits, authenticated with the
    /// **Subscription Key**.
    TokenPlan,
}

impl MiniMaxPlanId {
    /// Every plan MiniMax currently sells, in a stable order.
    pub const ALL: [Self; 2] = [Self::PayAsYouGo, Self::TokenPlan];

    /// Routing/registry name of this plan's provider profile.
    ///
    /// The two names differ so a route recorded in settings or a session log
    /// states which product was billed.
    #[must_use]
    pub const fn registry_name(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "minimax",
            Self::TokenPlan => "minimax-token-plan",
        }
    }

    /// Provider display name; it always names the plan.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "MiniMax (Pay-as-you-go)",
            Self::TokenPlan => "MiniMax (Token Plan)",
        }
    }

    /// Short human label for this plan alone.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "Pay-as-you-go",
            Self::TokenPlan => "Token Plan",
        }
    }

    /// Non-secret credential reference this plan's secret is stored under.
    ///
    /// `MINIMAX_API_KEY` is MiniMax's own documented variable name for the
    /// pay-as-you-go API Key. MiniMax reuses that same name for Token Plan
    /// keys, which is precisely the confusion this module exists to remove, so
    /// `MINIMAX_TOKEN_PLAN_KEY` is a **heycode-owned** reference name rather than
    /// a MiniMax-documented one. Two references mean the two secrets can be
    /// configured at once and neither can shadow the other.
    #[must_use]
    pub const fn credential_reference(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "MINIMAX_API_KEY",
            Self::TokenPlan => "MINIMAX_TOKEN_PLAN_KEY",
        }
    }

    /// S04 semantic credential kind for this plan.
    ///
    /// The kinds differ as well as the references, so a lookup carrying the
    /// wrong kind cannot be satisfied even when both secrets are stored under
    /// the same reference by a hand-edited settings document. `subscription-key`
    /// is MiniMax's own term for the Token Plan credential.
    #[must_use]
    pub const fn credential_kind(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "api-key",
            Self::TokenPlan => "subscription-key",
        }
    }

    /// Plugin name of this plan's model-catalog contribution.
    ///
    /// The names differ because both products may be composed at once, and two
    /// plugins claiming one name would collide at composition rather than
    /// registering two distinct provider catalogs.
    #[must_use]
    pub const fn catalog_plugin_name(self) -> &'static str {
        match self {
            Self::PayAsYouGo => "catalog-minimax",
            Self::TokenPlan => "catalog-minimax-token-plan",
        }
    }

    /// Prefix MiniMax documents for this plan's key, when it documents one.
    ///
    /// `sk-cp` is documented for the Token Plan key. MiniMax publishes no
    /// prefix for the pay-as-you-go API Key on its own documentation, so that
    /// stays [`None`]: an unverified prefix would reject valid keys, and third-
    /// party claims are not evidence.
    #[must_use]
    pub const fn documented_key_prefix(self) -> Option<&'static str> {
        match self {
            Self::PayAsYouGo => None,
            Self::TokenPlan => Some("sk-cp"),
        }
    }
}

impl fmt::Display for MiniMaxPlanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

mod sealed {
    /// Prevents foreign crates from inventing a third MiniMax plan.
    pub trait Sealed {}
}

/// Type-level identity of one MiniMax plan.
///
/// Implemented only by [`PayAsYouGo`] and [`TokenPlan`]. Types parameterised by
/// this trait — [`crate::MiniMaxCredential`] and [`crate::MiniMaxProfile`] —
/// therefore have exactly two inhabitants each, and the compiler refuses to
/// substitute one for the other.
pub trait MiniMaxPlan: sealed::Sealed + Copy + Send + Sync + 'static {
    /// Runtime discriminant for this marker type.
    const ID: MiniMaxPlanId;
    /// Plugin name of this plan's catalog contribution.
    ///
    /// Always [`MiniMaxPlanId::catalog_plugin_name`] for [`Self::ID`]; the
    /// constant exists only because [`heycode_core::Plugin::name`] needs a
    /// `&'static str` known at compile time.
    const CATALOG_PLUGIN_NAME: &'static str;
}

/// Marker type for metered pay-as-you-go billing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayAsYouGo;

/// Marker type for the fixed-price Token Plan subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenPlan;

impl sealed::Sealed for PayAsYouGo {}
impl sealed::Sealed for TokenPlan {}

impl MiniMaxPlan for PayAsYouGo {
    const ID: MiniMaxPlanId = MiniMaxPlanId::PayAsYouGo;
    const CATALOG_PLUGIN_NAME: &'static str = MiniMaxPlanId::PayAsYouGo.catalog_plugin_name();
}

impl MiniMaxPlan for TokenPlan {
    const ID: MiniMaxPlanId = MiniMaxPlanId::TokenPlan;
    const CATALOG_PLUGIN_NAME: &'static str = MiniMaxPlanId::TokenPlan.catalog_plugin_name();
}
