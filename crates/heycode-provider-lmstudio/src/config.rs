//! Complete, explicitly resolved LM Studio detection configuration.

use std::time::Duration;

use heycode_credentials::CredentialQuery;

use crate::detect::{LM_STUDIO_DEFAULT_CATALOG_TIMEOUT, LM_STUDIO_DEFAULT_TIMEOUT};
use crate::endpoint::{LmStudioAuth, LmStudioEndpoint};

/// Largest accepted total detection budget.
///
/// Detection runs against a local process, so a budget beyond a minute would
/// only hide a hung server for longer.
pub const LM_STUDIO_MAX_TIMEOUT: Duration = Duration::from_secs(60);

/// Rejected LM Studio detection configuration.
///
/// No variant carries URL text, because a configured base URL may embed
/// credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LmStudioConfigError {
    /// Base URL is not an absolute host-qualified HTTP(S) origin, or it embeds
    /// credentials.
    #[error("LM Studio base URL must be an absolute host-qualified HTTP(S) origin")]
    BaseUrl,
    /// Detection budget is zero or above [`LM_STUDIO_MAX_TIMEOUT`].
    #[error("LM Studio detection budget must be non-zero and at most 60 seconds")]
    Timeout,
}

/// Endpoint, auth posture and detection budget for one LM Studio server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioConfig {
    endpoint: LmStudioEndpoint,
    auth: LmStudioAuth,
    timeout: Duration,
    catalog_timeout: Duration,
}

impl LmStudioConfig {
    /// Detect the documented default local endpoint with no credential.
    #[must_use]
    pub fn local() -> Self {
        Self {
            endpoint: LmStudioEndpoint::local(),
            auth: LmStudioAuth::None,
            timeout: LM_STUDIO_DEFAULT_TIMEOUT,
            catalog_timeout: LM_STUDIO_DEFAULT_CATALOG_TIMEOUT,
        }
    }

    /// Detect an explicit endpoint instead of the documented default.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: LmStudioEndpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Send `Authorization: Bearer <token>` resolved from `query` on every
    /// probe.
    #[must_use]
    pub fn with_bearer_token(mut self, query: CredentialQuery) -> Self {
        self.auth = LmStudioAuth::BearerToken(query);
        self
    }

    /// Replace the total detection budget shared by every probe.
    ///
    /// # Errors
    /// [`LmStudioConfigError::Timeout`] for a zero or above-maximum budget.
    /// An out-of-range budget is rejected, never silently clamped.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, LmStudioConfigError> {
        if timeout.is_zero() || timeout > LM_STUDIO_MAX_TIMEOUT {
            return Err(LmStudioConfigError::Timeout);
        }
        self.timeout = timeout;
        Ok(self)
    }

    /// Replace the model-list deadline.
    ///
    /// Reading a large local library legitimately takes longer than a
    /// reachability probe, so it has its own budget rather than reusing the
    /// detection one.
    ///
    /// # Errors
    /// [`LmStudioConfigError::Timeout`] for a zero or above-maximum budget.
    /// An out-of-range budget is rejected, never silently clamped.
    pub fn with_catalog_timeout(mut self, timeout: Duration) -> Result<Self, LmStudioConfigError> {
        if timeout.is_zero() || timeout > LM_STUDIO_MAX_TIMEOUT {
            return Err(LmStudioConfigError::Timeout);
        }
        self.catalog_timeout = timeout;
        Ok(self)
    }

    /// Deadline for one model-list read.
    #[must_use]
    pub const fn catalog_timeout(&self) -> Duration {
        self.catalog_timeout
    }

    /// Configured endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &LmStudioEndpoint {
        &self.endpoint
    }

    /// Configured auth posture.
    #[must_use]
    pub const fn auth(&self) -> &LmStudioAuth {
        &self.auth
    }

    /// Total budget shared by every probe of one detection run.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}
