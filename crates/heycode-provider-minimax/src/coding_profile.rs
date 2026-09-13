//! Explicit eligibility gate for current MiniMax coding-tool use.
//!
//! MiniMax now redirects former Coding Plan documentation to Token Plan. A
//! Subscription Key may exist before the user has a seat or Credits, so key
//! shape is not eligibility. Current official tool guides use the ordinary
//! Token Plan OpenAI/Anthropic endpoints and publish no dedicated Coding Plan
//! endpoint; this boundary gates those documented routes without inventing a
//! third provider product or URL.

use heycode_credentials::{CredentialQuery, CredentialsError};
use heycode_llm::{CapabilitySupport, ProviderProfile};

use crate::{MiniMaxApiFamily, MiniMaxPlanId, MiniMaxProfile, MiniMaxRegion, TokenPlan};

/// Current account evidence that makes a Subscription Key usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMaxCodingEligibility {
    /// No account entitlement was inspected.
    Unknown,
    /// The key exists but the user has neither a seat nor Credits access.
    NoResources,
    /// A Token Plan seat is assigned to the user.
    TokenPlanSeat,
    /// Purchased Credits are available to the user.
    PurchasedCredits,
}

impl MiniMaxCodingEligibility {
    const fn is_eligible(self) -> bool {
        matches!(self, Self::TokenPlanSeat | Self::PurchasedCredits)
    }
}

/// Explicitly eligible Token Plan profile for coding-tool use.
///
/// This is not a third MiniMax product: provider identity and credential
/// ownership remain [`TokenPlan`]. It exists because MiniMax documents that a
/// Subscription Key may precede usable resources.
///
/// ```compile_fail
/// use heycode_provider_minimax::{
///     MiniMaxCodingEligibility, MiniMaxCodingProfile, MiniMaxProfile,
///     PayAsYouGo,
/// };
/// let _ = MiniMaxCodingProfile::select(
///     MiniMaxProfile::<PayAsYouGo>::international(),
///     MiniMaxCodingEligibility::TokenPlanSeat,
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiniMaxCodingProfile {
    profile: MiniMaxProfile<TokenPlan>,
    eligibility: MiniMaxCodingEligibility,
}

impl MiniMaxCodingProfile {
    /// Select current coding-tool use only after affirmative entitlement.
    ///
    /// # Errors
    /// Unknown or explicitly absent resources fail before an endpoint or
    /// credential query can be selected.
    pub fn select(
        profile: MiniMaxProfile<TokenPlan>,
        eligibility: MiniMaxCodingEligibility,
    ) -> Result<Self, MiniMaxCodingProfileError> {
        if !eligibility.is_eligible() {
            return Err(MiniMaxCodingProfileError::EligibilityRequired);
        }
        Ok(Self {
            profile,
            eligibility,
        })
    }

    /// Affirmative account evidence used for this selection.
    #[must_use]
    pub const fn eligibility(&self) -> MiniMaxCodingEligibility {
        self.eligibility
    }

    /// The billing product remains Token Plan.
    #[must_use]
    pub const fn plan(&self) -> MiniMaxPlanId {
        MiniMaxPlanId::TokenPlan
    }

    /// Regional deployment selected by the underlying Token Plan profile.
    #[must_use]
    pub const fn region(&self) -> MiniMaxRegion {
        self.profile.region()
    }

    /// Existing Token Plan provider identity; no duplicate coding provider is
    /// invented.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        self.profile.provider_profile()
    }

    /// Existing Token Plan credential identity.
    ///
    /// # Errors
    /// Built-in reference/kind validation failure.
    pub fn credential_query(&self) -> Result<CredentialQuery, CredentialsError> {
        self.profile.credential_query()
    }

    /// Select one currently documented Token Plan protocol endpoint.
    ///
    /// # Errors
    /// Region/protocol pairs without affirmative documentary evidence remain
    /// unavailable even after account eligibility is known.
    pub fn base_url(&self, family: MiniMaxApiFamily) -> Result<String, MiniMaxCodingProfileError> {
        if self.profile.documented_base_url(family) != CapabilitySupport::Supported {
            return Err(MiniMaxCodingProfileError::UnsupportedRoute);
        }
        Ok(self.profile.base_url(family))
    }

    /// Evidence for the tracker's historical dedicated Coding Plan endpoint.
    ///
    /// Current official pages redirect Coding Plan to Token Plan and configure
    /// ordinary `/v1` or `/anthropic` routes. Absence is not proof of
    /// Unsupported, so the dedicated endpoint remains Unknown rather than a
    /// fabricated URL.
    #[must_use]
    pub const fn dedicated_endpoint_evidence() -> CapabilitySupport {
        CapabilitySupport::Unknown
    }
}

/// Safe coding-profile selection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MiniMaxCodingProfileError {
    /// No current seat/Credits entitlement was affirmed.
    #[error("MiniMax coding-tool use requires an assigned Token Plan seat or purchased Credits")]
    EligibilityRequired,
    /// The selected region/protocol pair is not documented.
    #[error("MiniMax does not document this Token Plan route")]
    UnsupportedRoute,
}
