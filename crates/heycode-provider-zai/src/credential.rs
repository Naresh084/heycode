//! Plan-bound Z.ai credential references.

use std::marker::PhantomData;

use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};

use crate::{ZAI_CREDENTIAL_KIND, ZaiPlan, ZaiPlanKind, ZaiProfileError};

/// A non-secret credential reference bound at compile time to one Z.ai plan.
///
/// This type holds a lookup request, never a secret: resolution happens in the
/// credential registry at the operation that needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiCredential<P: ZaiPlanKind> {
    query: CredentialQuery,
    plan: PhantomData<P>,
}

impl<P: ZaiPlanKind> ZaiCredential<P> {
    /// This plan's default reference.
    ///
    /// # Errors
    /// [`ZaiProfileError::InvalidReference`] if the compiled default ever
    /// stops satisfying the S04 reference grammar.
    pub fn default_reference() -> Result<Self, ZaiProfileError> {
        Self::reference(P::DEFAULT_CREDENTIAL_REFERENCE)
    }

    /// An explicit non-secret reference for this plan.
    ///
    /// Configuration outranks the provider default (GOTCHAS #113), so a user
    /// may name the store slot; they may not name the *other* plan's default,
    /// because that is exactly one plan's key standing in for the other.
    ///
    /// # Errors
    /// [`ZaiProfileError::CrossPlanReference`] for the other plan's default
    /// reference, and [`ZaiProfileError::InvalidReference`] for a value the
    /// S04 reference grammar rejects.
    pub fn reference(reference: impl Into<String>) -> Result<Self, ZaiProfileError> {
        let reference = reference.into();
        if reference == P::FOREIGN_CREDENTIAL_REFERENCE {
            return Err(ZaiProfileError::CrossPlanReference {
                plan: P::PLAN,
                reference,
            });
        }
        let name = CredentialReference::new(reference.clone())
            .map_err(|_| ZaiProfileError::InvalidReference { reference })?;
        let kind = CredentialKind::new(ZAI_CREDENTIAL_KIND).map_err(|_| {
            ZaiProfileError::InvalidReference {
                reference: ZAI_CREDENTIAL_KIND.to_owned(),
            }
        })?;
        Ok(Self {
            query: CredentialQuery::new(name, kind),
            plan: PhantomData,
        })
    }

    /// Plan this credential belongs to.
    #[must_use]
    pub fn plan(&self) -> ZaiPlan {
        P::PLAN
    }

    /// Lookup request for the credential registry.
    #[must_use]
    pub fn query(&self) -> &CredentialQuery {
        &self.query
    }

    /// Non-secret reference name.
    #[must_use]
    pub fn reference_name(&self) -> &str {
        self.query.reference.as_str()
    }
}
