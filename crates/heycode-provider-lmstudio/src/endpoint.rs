//! Validated LM Studio origin, its documented probe surfaces and auth posture.

use heycode_credentials::CredentialQuery;
use heycode_http::HttpRequest;

use crate::config::LmStudioConfigError;

/// Documented default LM Studio local-server origin.
///
/// <https://lmstudio.ai/docs/app/api/endpoints/openai> documents the base URL
/// `http://localhost:1234/v1` for a server whose "server port is `1234`", and
/// every `curl` example in the REST documentation uses `http://localhost:1234`.
pub const LM_STUDIO_DEFAULT_BASE_URL: &str = "http://localhost:1234";

/// Registry id for the LM Studio provider.
pub const LM_STUDIO_PROVIDER: &str = "lmstudio";

/// Human display name for the LM Studio provider.
pub const LM_STUDIO_DISPLAY_NAME: &str = "LM Studio";

/// One documented LM Studio HTTP surface this crate can probe with a `GET`.
///
/// Every LM Studio inference route is `POST`-only and probing one would load a
/// model, so each surface is observed through its own model-list route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LmStudioSurface {
    /// Native REST generation v1 under `/api/v1/*`, released in LM Studio
    /// 0.4.0. <https://lmstudio.ai/docs/developer/rest/list>
    NativeRestV1,
    /// Legacy native REST generation v0 under `/api/v0/*`.
    /// <https://lmstudio.ai/docs/developer/rest/endpoints>
    NativeRestV0,
    /// OpenAI-compatible surface under `/v1/*`.
    /// <https://lmstudio.ai/docs/developer/openai-compat/models>
    OpenAiCompatible,
}

impl LmStudioSurface {
    /// Every probed surface, in probe order: the current native generation
    /// first, then the legacy one, then the compatibility surface.
    pub const ALL: [Self; 3] = [
        Self::NativeRestV1,
        Self::NativeRestV0,
        Self::OpenAiCompatible,
    ];

    /// Documented model-list path this surface is observed through.
    ///
    /// `GET /api/v1/models` — <https://lmstudio.ai/docs/developer/rest/list>
    /// `GET /api/v0/models` — <https://lmstudio.ai/docs/developer/rest/endpoints>
    /// `GET /v1/models` — <https://lmstudio.ai/docs/developer/openai-compat/models>
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::NativeRestV1 => "/api/v1/models",
            Self::NativeRestV0 => "/api/v0/models",
            Self::OpenAiCompatible => "/v1/models",
        }
    }

    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeRestV1 => "native-rest-v1",
            Self::NativeRestV0 => "native-rest-v0",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }
}

/// Validated LM Studio origin plus every derived probe URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioEndpoint {
    base_url: String,
    native_rest_v1_url: String,
    native_rest_v0_url: String,
    openai_compatible_url: String,
}

impl LmStudioEndpoint {
    /// The documented default local endpoint.
    ///
    /// [`LM_STUDIO_DEFAULT_BASE_URL`] is a compile-time constant origin, so
    /// this cannot fail; a test pins it against [`Self::new`].
    #[must_use]
    pub fn local() -> Self {
        Self::derive(LM_STUDIO_DEFAULT_BASE_URL)
    }

    /// Validate a base origin and derive every documented probe URL.
    ///
    /// # Errors
    /// [`LmStudioConfigError::BaseUrl`] when the origin is not an absolute
    /// host-qualified HTTP(S) URL without embedded credentials, or when any
    /// derived probe URL is unusable. The error carries no URL text, because a
    /// configured base URL may embed credentials.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, LmStudioConfigError> {
        let endpoint = Self::derive(base_url.as_ref());
        for surface in LmStudioSurface::ALL {
            HttpRequest::get(endpoint.url(surface)).map_err(|_| LmStudioConfigError::BaseUrl)?;
        }
        Ok(endpoint)
    }

    /// Normalized origin without a trailing slash.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Probe URL for one surface.
    #[must_use]
    pub fn url(&self, surface: LmStudioSurface) -> &str {
        match surface {
            LmStudioSurface::NativeRestV1 => &self.native_rest_v1_url,
            LmStudioSurface::NativeRestV0 => &self.native_rest_v0_url,
            LmStudioSurface::OpenAiCompatible => &self.openai_compatible_url,
        }
    }

    fn derive(base_url: &str) -> Self {
        let base_url = base_url.trim_end_matches('/').to_owned();
        Self {
            native_rest_v1_url: format!("{base_url}{}", LmStudioSurface::NativeRestV1.path()),
            native_rest_v0_url: format!("{base_url}{}", LmStudioSurface::NativeRestV0.path()),
            openai_compatible_url: format!(
                "{base_url}{}",
                LmStudioSurface::OpenAiCompatible.path()
            ),
            base_url,
        }
    }
}

/// How detection authenticates to the LM Studio server.
///
/// "By default, LM Studio does not require authentication for API requests."
/// A user may enable API tokens in Server Settings, after which every request
/// carries `Authorization: Bearer $LM_API_TOKEN`.
/// <https://lmstudio.ai/docs/developer/core/authentication>
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LmStudioAuth {
    /// Probe unauthenticated. This is LM Studio's documented default posture,
    /// modelled explicitly rather than as an absent credential.
    None,
    /// Resolve a bearer token from this query and send it on every probe.
    ///
    /// A query that resolves to nothing falls back to an unauthenticated probe:
    /// LM Studio needs no credential by default, and a server that does need
    /// one answers `401`, which detection reports as
    /// [`crate::LmStudioCredentialState::Required`].
    BearerToken(CredentialQuery),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoint_equals_the_validated_documented_default() {
        assert_eq!(
            LmStudioEndpoint::local(),
            LmStudioEndpoint::new(LM_STUDIO_DEFAULT_BASE_URL).unwrap()
        );
    }
}
