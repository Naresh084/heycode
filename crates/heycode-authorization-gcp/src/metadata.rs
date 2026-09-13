//! Bounded, credential-blind probe of the ambient Compute Engine metadata
//! server.
//!
//! The probe reads only non-secret facts: whether a default service account is
//! attached, the host project id and the host zone. It deliberately never
//! requests `instance/service-accounts/default/token`, which would materialize
//! an access token this crate has no reason to hold.
//!
//! Endpoint, header and path facts follow the Compute Engine metadata
//! documentation (<https://docs.cloud.google.com/compute/docs/metadata/overview>):
//! the DNS name `metadata.google.internal`, the request header
//! `Metadata-Flavor: Google`, the `/computeMetadata/v1` prefix and the same
//! header echoed back to prove the responder is the real metadata server.

use std::time::Duration;

use heycode_http::{HttpRequest, HttpService, TransportError};
use tokio_util::sync::CancellationToken;

use crate::env::GcpEnvironment;
use crate::model::GcpUncertainty;

/// Environment variable that overrides the metadata host.
pub const ENV_GCE_METADATA_HOST: &str = "GCE_METADATA_HOST";
/// Environment variable that suppresses the ambient probe entirely.
pub const ENV_NO_GCE_CHECK: &str = "NO_GCE_CHECK";
/// Documented default metadata host.
pub const DEFAULT_METADATA_HOST: &str = "metadata.google.internal";

const FLAVOR_HEADER: &str = "metadata-flavor";
const FLAVOR_VALUE: &str = "Google";
const MAX_METADATA_BYTES: usize = 4096;

/// Non-secret facts one authoritative metadata server answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetadataFacts {
    pub(crate) host: String,
    pub(crate) service_account_attached: bool,
    /// `Ok(None)` means the server determinately reports no project id;
    /// `Err` means that one lookup never settled and says nothing either way.
    pub(crate) project: Result<Option<String>, GcpUncertainty>,
    /// Same contract as [`Self::project`], for the region of `instance/zone`.
    pub(crate) zone_region: Result<Option<String>, GcpUncertainty>,
}

/// What the ambient probe established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MetadataOutcome {
    /// An authoritative metadata server answered.
    Answered(MetadataFacts),
    /// Something answered and proved it is not a metadata server.
    NotPresent,
    /// No determinate answer was reached.
    Undetermined(GcpUncertainty),
}

/// Resolve the metadata host, honoring the documented `GCE_METADATA_HOST`
/// override.
///
/// The override must be a bare host or `host:port`. A value carrying a scheme,
/// path, query or userinfo is refused rather than pasted into a URL.
pub(crate) fn resolve_host(environment: &dyn GcpEnvironment) -> Result<String, GcpUncertainty> {
    let Some(host) = environment.var(ENV_GCE_METADATA_HOST) else {
        return Ok(DEFAULT_METADATA_HOST.to_owned());
    };
    let shaped = !host.is_empty()
        && host.len() <= 255
        && !host.starts_with(':')
        && host.as_bytes().iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-' | b':' | b'[' | b']')
        });
    if !shaped {
        return Err(GcpUncertainty::ProbeHostInvalid);
    }
    Ok(host)
}

/// Whether `NO_GCE_CHECK` suppresses the probe.
///
/// google-auth compares the value with an exact lowercase `"true"`
/// (<https://github.com/googleapis/google-auth-library-python/blob/main/google/auth/compute_engine/_metadata.py>);
/// heycode matches that exactly so a value such as `False` or `0` keeps the
/// library's default behavior of probing.
pub(crate) fn probe_suppressed(environment: &dyn GcpEnvironment) -> bool {
    environment
        .var(ENV_NO_GCE_CHECK)
        .is_some_and(|value| value == "true")
}

/// Run the bounded probe.
pub(crate) async fn probe(
    http: &HttpService,
    host: &str,
    budget: Duration,
    cancellation: CancellationToken,
) -> MetadataOutcome {
    let inner = probe_inner(http, host, &cancellation);
    match tokio::time::timeout(budget, inner).await {
        Ok(outcome) => outcome,
        Err(_) => MetadataOutcome::Undetermined(GcpUncertainty::ProbeTimedOut),
    }
}

async fn probe_inner(
    http: &HttpService,
    host: &str,
    cancellation: &CancellationToken,
) -> MetadataOutcome {
    let service_account_attached = match get(
        http,
        host,
        "instance/service-accounts/default/",
        cancellation,
    )
    .await
    {
        Fetch::Ok(_) => true,
        Fetch::NotFound => false,
        Fetch::NotAuthoritative => return MetadataOutcome::NotPresent,
        Fetch::Unexpected => {
            return MetadataOutcome::Undetermined(GcpUncertainty::ProbeUnexpectedStatus);
        }
        Fetch::Failed(reason) => return MetadataOutcome::Undetermined(reason),
    };
    let project = match get(http, host, "project/project-id", cancellation).await {
        Fetch::Ok(body) => Ok(non_blank(body)),
        Fetch::NotFound => Ok(None),
        Fetch::NotAuthoritative | Fetch::Unexpected => Err(GcpUncertainty::ProbeUnexpectedStatus),
        Fetch::Failed(reason) => Err(reason),
    };
    let zone_region = match get(http, host, "instance/zone", cancellation).await {
        Fetch::Ok(body) => Ok(non_blank(body).as_deref().and_then(region_of_zone)),
        Fetch::NotFound => Ok(None),
        Fetch::NotAuthoritative | Fetch::Unexpected => Err(GcpUncertainty::ProbeUnexpectedStatus),
        Fetch::Failed(reason) => Err(reason),
    };
    MetadataOutcome::Answered(MetadataFacts {
        host: host.to_owned(),
        service_account_attached,
        project,
        zone_region,
    })
}

enum Fetch {
    /// Status 200 with the authoritative flavor header.
    Ok(String),
    /// Status 404 with the authoritative flavor header.
    NotFound,
    /// Status 200 without the flavor header: the responder proved it is not a
    /// metadata server.
    NotAuthoritative,
    /// Any other status. No documented meaning, so no determinate claim.
    Unexpected,
    /// The exchange never produced a response.
    Failed(GcpUncertainty),
}

async fn get(
    http: &HttpService,
    host: &str,
    path: &str,
    cancellation: &CancellationToken,
) -> Fetch {
    let url = format!("http://{host}/computeMetadata/v1/{path}");
    let Ok(request) = HttpRequest::get(&url) else {
        return Fetch::Failed(GcpUncertainty::ProbeHostInvalid);
    };
    let Ok(request) = request.header(FLAVOR_HEADER, FLAVOR_VALUE) else {
        return Fetch::Failed(GcpUncertainty::ProbeUnreachable);
    };
    let request = request.with_max_response_bytes(MAX_METADATA_BYTES);
    match http.send(request, cancellation.clone()).await {
        Ok(response) => {
            let authoritative = response
                .header(FLAVOR_HEADER)
                .is_some_and(|value| value == FLAVOR_VALUE);
            match (response.status, authoritative) {
                (200, true) => match String::from_utf8(response.body) {
                    Ok(body) => Fetch::Ok(body),
                    Err(_) => Fetch::Unexpected,
                },
                (200, false) => Fetch::NotAuthoritative,
                (404, true) => Fetch::NotFound,
                _ => Fetch::Unexpected,
            }
        }
        Err(TransportError::Cancelled) => Fetch::Failed(GcpUncertainty::Cancelled),
        Err(TransportError::Timeout) => Fetch::Failed(GcpUncertainty::ProbeTimedOut),
        Err(_) => Fetch::Failed(GcpUncertainty::ProbeUnreachable),
    }
}

fn non_blank(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Derive the region from a metadata `instance/zone` value.
///
/// The documented shape is `projects/<numeric-project>/zones/<zone>`, and a
/// zone is its region plus a single trailing letter.
fn region_of_zone(value: &str) -> Option<String> {
    let zone = value.rsplit('/').next()?;
    let (region, suffix) = zone.rsplit_once('-')?;
    if suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_lowercase() && !region.is_empty() {
        Some(region.to_owned())
    } else {
        None
    }
}
