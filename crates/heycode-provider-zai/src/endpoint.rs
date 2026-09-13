//! Plan-bound Z.ai endpoints.

use std::marker::PhantomData;

use crate::{ZaiPlan, ZaiPlanKind, ZaiProfileError, ZaiProtocol};

/// One documented Z.ai base URL, bound at compile time to the plan that
/// publishes it.
///
/// There is deliberately no base-URL override. A profile whose endpoint could
/// be redirected is a profile whose plan no longer describes where a request
/// goes, and no consumer needs a Z.ai proxy today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZaiEndpoint<P: ZaiPlanKind> {
    protocol: ZaiProtocol,
    base_url: &'static str,
    plan: PhantomData<P>,
}

impl<P: ZaiPlanKind> ZaiEndpoint<P> {
    /// The base URL Z.ai documents for this plan and protocol.
    ///
    /// # Errors
    /// [`ZaiProfileError::UndocumentedEndpoint`] when Z.ai publishes no base
    /// URL for the pair. heycode never substitutes the other plan's URL.
    pub fn documented(protocol: ZaiProtocol) -> Result<Self, ZaiProfileError> {
        let base_url =
            P::documented_base_url(protocol).ok_or(ZaiProfileError::UndocumentedEndpoint {
                plan: P::PLAN,
                protocol,
            })?;
        Ok(Self {
            protocol,
            base_url,
            plan: PhantomData,
        })
    }

    /// Plan this endpoint belongs to.
    #[must_use]
    pub fn plan(&self) -> ZaiPlan {
        P::PLAN
    }

    /// Protocol spoken at this base URL.
    #[must_use]
    pub fn protocol(&self) -> ZaiProtocol {
        self.protocol
    }

    /// Base URL, without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &'static str {
        self.base_url
    }
}
