//! Plan-bound MiniMax secret and its closed admission taxonomy.

use std::fmt;
use std::marker::PhantomData;

use heycode_credentials::CredentialSecret;

use crate::plan::{MiniMaxPlan, MiniMaxPlanId};

/// A MiniMax secret that is bound to one plan for the rest of its life.
///
/// The only way to obtain one is [`crate::MiniMaxProfile::admit`] or
/// [`crate::MiniMaxProfile::resolve`], and a profile is itself parameterised by
/// the plan — so the plan of a credential is decided by the profile that minted
/// it and can never be re-labelled afterwards. Passing the wrong product to a
/// function that named a concrete plan is a type error:
///
/// ```compile_fail
/// use heycode_provider_minimax::{MiniMaxCredential, PayAsYouGo, TokenPlan};
///
/// fn charge_the_subscription(_credential: &MiniMaxCredential<TokenPlan>) {}
///
/// fn misroute(pay_as_you_go: &MiniMaxCredential<PayAsYouGo>) {
///     charge_the_subscription(pay_as_you_go);
/// }
/// ```
///
/// The same call with the matching product compiles, so the failure above is
/// the plan mismatch and not a broken example:
///
/// ```
/// use heycode_provider_minimax::{MiniMaxCredential, TokenPlan};
///
/// fn charge_the_subscription(_credential: &MiniMaxCredential<TokenPlan>) {}
///
/// fn route(token_plan: &MiniMaxCredential<TokenPlan>) {
///     charge_the_subscription(token_plan);
/// }
/// ```
///
/// This removes accidental confusion, which is what the two products' shared
/// environment-variable name causes. It is not a defence against a caller that
/// deliberately copies bytes out of [`MiniMaxCredential::expose`].
pub struct MiniMaxCredential<P: MiniMaxPlan> {
    secret: CredentialSecret,
    plan: PhantomData<P>,
}

impl<P: MiniMaxPlan> MiniMaxCredential<P> {
    pub(crate) fn bind(secret: CredentialSecret) -> Self {
        Self {
            secret,
            plan: PhantomData,
        }
    }

    /// The plan this secret was admitted for.
    #[must_use]
    pub const fn plan(&self) -> MiniMaxPlanId {
        P::ID
    }

    /// Expose the secret at the operation boundary that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.secret.expose()
    }
}

/// Names the plan and never the secret.
impl<P: MiniMaxPlan> fmt::Debug for MiniMaxCredential<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "MiniMaxCredential<{}>([REDACTED])",
            P::ID.registry_name()
        )
    }
}

/// Closed reasons a secret cannot become a plan-bound MiniMax credential.
///
/// No variant carries the rejected secret, and the registry `message` is the
/// S04 failure text, which providers are required to keep redacted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MiniMaxCredentialError {
    /// No credential provider holds this plan's reference.
    #[error("no credential provider holds the MiniMax {plan} reference `{reference}`")]
    Missing {
        /// Plan whose reference was looked up.
        plan: MiniMaxPlanId,
        /// Non-secret reference that was looked up.
        reference: &'static str,
    },
    /// The credential registry or one of its providers failed.
    #[error("the credential registry could not resolve the MiniMax {plan} reference: {message}")]
    Registry {
        /// Plan whose reference was looked up.
        plan: MiniMaxPlanId,
        /// Redacted S04 failure text.
        message: String,
    },
    /// The secret is empty or holds bytes that cannot appear in a header.
    #[error(
        "the MiniMax {plan} secret is empty or contains bytes that cannot appear in an HTTP header"
    )]
    Malformed {
        /// Plan whose reference was resolved.
        plan: MiniMaxPlanId,
    },
    /// The secret carries a prefix MiniMax documents for the other product.
    #[error(
        "the secret stored for MiniMax {plan} starts with `{prefix}`, which MiniMax documents for {other}"
    )]
    ForeignPlan {
        /// Plan the secret was being admitted for.
        plan: MiniMaxPlanId,
        /// Plan the prefix actually belongs to.
        other: MiniMaxPlanId,
        /// Documented prefix that identified the other plan.
        prefix: &'static str,
    },
    /// The secret lacks the prefix MiniMax documents for this product.
    #[error("the secret stored for MiniMax {plan} does not start with the documented `{prefix}`")]
    PrefixMismatch {
        /// Plan the secret was being admitted for.
        plan: MiniMaxPlanId,
        /// Documented prefix the secret must carry.
        prefix: &'static str,
    },
}

/// Decide whether `secret` may be admitted as `plan`'s credential.
///
/// The rule is derived from [`MiniMaxPlanId::documented_key_prefix`] rather
/// than special-cased per plan, so documenting a pay-as-you-go prefix later
/// makes the check symmetric without touching this function.
pub(crate) fn admit(
    plan: MiniMaxPlanId,
    secret: &CredentialSecret,
) -> Result<(), MiniMaxCredentialError> {
    let value = secret.expose();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(MiniMaxCredentialError::Malformed { plan });
    }
    for other in MiniMaxPlanId::ALL {
        if other == plan {
            continue;
        }
        if let Some(prefix) = other.documented_key_prefix()
            && value.starts_with(prefix)
        {
            return Err(MiniMaxCredentialError::ForeignPlan {
                plan,
                other,
                prefix,
            });
        }
    }
    if let Some(prefix) = plan.documented_key_prefix()
        && !value.starts_with(prefix)
    {
        return Err(MiniMaxCredentialError::PrefixMismatch { plan, prefix });
    }
    Ok(())
}
