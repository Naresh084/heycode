//! Bounded, cancellable LM Studio surface detection.

use std::sync::Arc;
use std::time::Duration;

use heycode_credentials::CredentialsService;
use heycode_http::{HttpRequest, HttpResponse, HttpService, TransportError};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::LmStudioConfig;
use crate::endpoint::{LmStudioAuth, LmStudioSurface};
use crate::report::{LmStudioProbe, LmStudioServerReport, LmStudioSurfaceObservation};

/// Default total detection budget shared by every probe of one run.
pub const LM_STUDIO_DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Default deadline for one model-list read.
///
/// A local library can hold hundreds of models, so this is deliberately far
/// larger than the reachability budget while still bounding a hung server.
pub const LM_STUDIO_DEFAULT_CATALOG_TIMEOUT: Duration = Duration::from_secs(30);

/// Response cap for one probe. Detection reads only an envelope; PLM02 owns the
/// full model listing.
const PROBE_RESPONSE_LIMIT: usize = 1024 * 1024;

/// Detects an LM Studio server's presence, version bound and protocol surfaces.
///
/// Detection never fails: "not running" is an ordinary local state and is
/// reported as a verdict, not an error the caller must decode.
pub struct LmStudioDetector {
    http: HttpService,
    credentials: Option<Arc<CredentialsService>>,
    config: LmStudioConfig,
}

impl LmStudioDetector {
    /// Build a detector for one configured endpoint.
    ///
    /// `credentials` is required only when the configuration carries
    /// [`LmStudioAuth::BearerToken`]; the documented default posture resolves
    /// nothing.
    #[must_use]
    pub const fn new(
        http: HttpService,
        credentials: Option<Arc<CredentialsService>>,
        config: LmStudioConfig,
    ) -> Self {
        Self {
            http,
            credentials,
            config,
        }
    }

    /// Configuration this detector probes with.
    #[must_use]
    pub const fn config(&self) -> &LmStudioConfig {
        &self.config
    }

    /// Probe every documented surface and derive one verdict.
    ///
    /// The configured budget is shared by the whole run, not granted per probe,
    /// so a hung local server cannot cost more than that one budget. A probe
    /// that runs out of budget is [`LmStudioProbe::Indeterminate`] and is never
    /// reported as an unreachable server.
    pub async fn detect(&self, cancellation: CancellationToken) -> LmStudioServerReport {
        let authorization = self.authorization();
        let authenticated = authorization.is_some();
        let deadline = tokio::time::Instant::now() + self.config.timeout();
        let mut observations = Vec::with_capacity(LmStudioSurface::ALL.len());
        for surface in LmStudioSurface::ALL {
            let probe = self
                .probe(surface, authorization.as_deref(), deadline, &cancellation)
                .await;
            observations.push(LmStudioSurfaceObservation { surface, probe });
        }
        LmStudioServerReport::from_observations(observations, authenticated)
    }

    /// Resolve the bearer header for this run, if one is configured and
    /// available. The secret lives only for the duration of one run and is
    /// never stored on this detector.
    fn authorization(&self) -> Option<String> {
        let LmStudioAuth::BearerToken(query) = self.config.auth() else {
            return None;
        };
        let secret = self.credentials.as_ref()?.resolve(query).ok().flatten()?;
        Some(format!("Bearer {}", secret.expose()))
    }

    async fn probe(
        &self,
        surface: LmStudioSurface,
        authorization: Option<&str>,
        deadline: tokio::time::Instant,
        cancellation: &CancellationToken,
    ) -> LmStudioProbe {
        if cancellation.is_cancelled() {
            return LmStudioProbe::Indeterminate;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return LmStudioProbe::Indeterminate;
        }
        let mut request = HttpRequest::get(self.config.endpoint().url(surface))
            .and_then(|request| request.header("accept", "application/json"));
        if let Some(value) = authorization {
            request = request.and_then(|request| request.header("authorization", value));
        }
        let Ok(request) =
            request.map(|request| request.with_max_response_bytes(PROBE_RESPONSE_LIMIT))
        else {
            // The endpoint validated at construction, so this is an internal
            // fault rather than evidence about the server.
            return LmStudioProbe::Indeterminate;
        };
        match tokio::time::timeout(remaining, self.http.send(request, cancellation.clone())).await {
            Err(_elapsed) => LmStudioProbe::Indeterminate,
            Ok(Err(error)) => classify_transport_error(&error),
            Ok(Ok(response)) => classify_response(surface, &response),
        }
    }
}

impl std::fmt::Debug for LmStudioDetector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LmStudioDetector")
            .field("base_url", &self.config.endpoint().base_url())
            .field(
                "authenticated",
                &matches!(self.config.auth(), LmStudioAuth::BearerToken(_)),
            )
            .field("timeout", &self.config.timeout())
            .finish()
    }
}

fn classify_transport_error(error: &TransportError) -> LmStudioProbe {
    match error {
        TransportError::Http {
            status: 401 | 403, ..
        } => LmStudioProbe::Unauthorized,
        TransportError::Http { .. } => LmStudioProbe::Unrecognized,
        TransportError::Network { .. } => LmStudioProbe::Unreachable,
        TransportError::Cancelled
        | TransportError::Timeout
        | TransportError::ResponseTooLarge { .. }
        | TransportError::InvalidRequest { .. }
        | TransportError::InvalidSse { .. } => LmStudioProbe::Indeterminate,
        // `TransportError` is non-exhaustive: a transport outcome this crate has
        // not classified proves nothing about the server.
        _ => LmStudioProbe::Indeterminate,
    }
}

/// Classify one probe response.
///
/// LM Studio answers `200 OK` on unknown paths
/// (<https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1323>), so the
/// status is only a gate; the documented body shape is the evidence.
fn classify_response(surface: LmStudioSurface, response: &HttpResponse) -> LmStudioProbe {
    match response.status {
        401 | 403 => return LmStudioProbe::Unauthorized,
        200..=299 => {}
        _ => return LmStudioProbe::Unrecognized,
    }
    if !is_json(response.content_type.as_deref()) {
        return LmStudioProbe::Unrecognized;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&response.body) else {
        return LmStudioProbe::Unrecognized;
    };
    if recognizes(surface, &body) {
        LmStudioProbe::Recognized
    } else {
        LmStudioProbe::Unrecognized
    }
}

fn is_json(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| value == "application/json" || value.ends_with("+json"))
}

/// Whether a body matches the surface's documented model-list envelope.
///
/// The v0 and OpenAI-compatible surfaces share the same envelope, so the path
/// is what separates them; the native v1 surface has its own.
fn recognizes(surface: LmStudioSurface, body: &Value) -> bool {
    match surface {
        // `{ "models": [ … ] }` — <https://lmstudio.ai/docs/developer/rest/list>
        LmStudioSurface::NativeRestV1 => body.get("models").is_some_and(Value::is_array),
        // `{ "object": "list", "data": [ … ] }` —
        // <https://lmstudio.ai/docs/developer/rest/endpoints> for v0. LM Studio
        // publishes no verbatim body for its OpenAI-compatible `/v1/models`, so
        // that surface is held to the same documented list envelope, which its
        // own v0 example serves.
        LmStudioSurface::NativeRestV0 | LmStudioSurface::OpenAiCompatible => {
            body.get("object").and_then(Value::as_str) == Some("list")
                && body.get("data").is_some_and(Value::is_array)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn unclassified_transport_outcomes_prove_nothing_about_the_server() {
        assert_eq!(
            classify_transport_error(&TransportError::Timeout),
            LmStudioProbe::Indeterminate
        );
        assert_eq!(
            classify_transport_error(&TransportError::ResponseTooLarge { max_bytes: 1 }),
            LmStudioProbe::Indeterminate
        );
    }
}
