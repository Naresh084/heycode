//! Plan-parameterised MiniMax provider profile.

use std::marker::PhantomData;

use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialsError,
    CredentialsService,
};
use heycode_llm::{CapabilitySupport, ProviderDescriptor, ProviderProfile};

use crate::credential::{MiniMaxCredential, MiniMaxCredentialError, admit};
use crate::endpoint::{MiniMaxApiFamily, MiniMaxRegion, base_url, documented_base_url};
use crate::models::MINIMAX_M3;
use crate::plan::{MiniMaxPlan, MiniMaxPlanId};

/// One MiniMax product on one regional deployment.
///
/// The plan is a type parameter, so a profile cannot be re-pointed at the other
/// product after construction and everything it mints — its credential query,
/// its routing name and its [`MiniMaxCredential`] — inherits that binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiniMaxProfile<P: MiniMaxPlan> {
    region: MiniMaxRegion,
    plan: PhantomData<P>,
}

impl<P: MiniMaxPlan> MiniMaxProfile<P> {
    /// Profile for an explicit regional deployment.
    #[must_use]
    pub const fn new(region: MiniMaxRegion) -> Self {
        Self {
            region,
            plan: PhantomData,
        }
    }

    /// Profile for the international deployment.
    #[must_use]
    pub const fn international() -> Self {
        Self::new(MiniMaxRegion::International)
    }

    /// Product this profile bills.
    #[must_use]
    pub const fn plan(&self) -> MiniMaxPlanId {
        P::ID
    }

    /// Deployment this profile routes to.
    #[must_use]
    pub const fn region(&self) -> MiniMaxRegion {
        self.region
    }

    /// Absolute base URL for one protocol family.
    #[must_use]
    pub fn base_url(&self, family: MiniMaxApiFamily) -> String {
        base_url(self.region, family)
    }

    /// Documentary evidence that this deployment serves `family`.
    #[must_use]
    pub const fn documented_base_url(&self, family: MiniMaxApiFamily) -> CapabilitySupport {
        documented_base_url(self.region, family)
    }

    /// The S04 lookup this product's secret is stored under.
    ///
    /// Both the reference and the kind differ between the two products, so a
    /// query built for one can never resolve the other's secret.
    ///
    /// # Errors
    /// The built-in literals are validated through the same public S04
    /// boundary as any user-supplied reference and kind.
    pub fn credential_query(&self) -> Result<CredentialQuery, CredentialsError> {
        Ok(CredentialQuery::new(
            CredentialReference::new(P::ID.credential_reference())?,
            CredentialKind::new(P::ID.credential_kind())?,
        ))
    }

    /// Safe provider identity for catalogs and routing.
    ///
    /// Only protocol families this deployment is documented to serve are
    /// listed; an undocumented pair is absent rather than optimistically
    /// declared.
    #[must_use]
    pub fn provider_descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: P::ID.registry_name().to_owned(),
            display_name: P::ID.display_name().to_owned(),
            protocols: MiniMaxApiFamily::ALL
                .into_iter()
                .filter(|family| self.documented_base_url(*family) == CapabilitySupport::Supported)
                .map(MiniMaxApiFamily::protocol)
                .collect(),
        }
    }

    /// Safe identity/default metadata shared by setup and routing.
    ///
    /// The two products produce two distinct registry names, so composing both
    /// at once is legal and a route always states which one was billed.
    /// Composing the same product for two regions is not: `ProviderRegistry`
    /// rejects the duplicate name, which is the intended fail-loud outcome for
    /// a world that tried to serve one MiniMax account from two deployments.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: P::ID.registry_name().to_owned(),
            descriptor: self.provider_descriptor(),
            default_model: MINIMAX_M3.to_owned(),
            credential_reference: Some(P::ID.credential_reference().to_owned()),
        }
    }

    /// Bind an already-resolved secret to this profile's product.
    ///
    /// # Errors
    /// A secret that is empty, unusable as a header value, or carries the other
    /// product's documented prefix is refused, as is one missing this product's
    /// own documented prefix.
    pub fn admit(
        &self,
        secret: CredentialSecret,
    ) -> Result<MiniMaxCredential<P>, MiniMaxCredentialError> {
        admit(P::ID, &secret)?;
        Ok(MiniMaxCredential::bind(secret))
    }

    /// Resolve this product's secret through the credential registry.
    ///
    /// # Errors
    /// An absent reference, a registry/provider failure, or a secret refused by
    /// [`MiniMaxProfile::admit`] all fail without exposing the secret.
    pub fn resolve(
        &self,
        credentials: &CredentialsService,
    ) -> Result<MiniMaxCredential<P>, MiniMaxCredentialError> {
        let query = self
            .credential_query()
            .map_err(|error| MiniMaxCredentialError::Registry {
                plan: P::ID,
                message: error.to_string(),
            })?;
        let secret = credentials
            .resolve(&query)
            .map_err(|error| MiniMaxCredentialError::Registry {
                plan: P::ID,
                message: error.to_string(),
            })?
            .ok_or(MiniMaxCredentialError::Missing {
                plan: P::ID,
                reference: P::ID.credential_reference(),
            })?;
        self.admit(secret)
    }
}
