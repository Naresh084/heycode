//! Evidence-backed verdict of one LM Studio detection run.

use heycode_core::ProviderProtocol;
use heycode_llm::{CapabilitySupport, ProviderDescriptor};

use crate::endpoint::{LM_STUDIO_DISPLAY_NAME, LM_STUDIO_PROVIDER, LmStudioSurface};

/// Published LM Studio release that introduced the native REST v1 surface.
///
/// "With LM Studio 0.4.0, we have officially released our native v1 REST API
/// at `/api/v1/*` endpoints."
/// <https://lmstudio.ai/docs/developer/api-changelog>
pub const LM_STUDIO_NATIVE_REST_V1_RELEASE: &str = "0.4.0";

/// What one surface probe observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioProbe {
    /// The surface answered with its documented body shape.
    Recognized,
    /// The surface answered, but not with its documented shape. LM Studio's
    /// server answers `200` on unknown paths, so a status alone is never
    /// evidence.
    Unrecognized,
    /// The server demanded a credential (`401`/`403`).
    Unauthorized,
    /// The probe timed out, was cancelled, exhausted the detection budget, or
    /// returned a body too large to read. Nothing may be concluded — in
    /// particular this is never [`Self::Unreachable`].
    Indeterminate,
    /// The connection failed before any response arrived.
    Unreachable,
}

/// One probed surface and what it observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LmStudioSurfaceObservation {
    /// Probed surface.
    pub surface: LmStudioSurface,
    /// What the probe saw.
    pub probe: LmStudioProbe,
}

/// Reachability verdict for the configured endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioHealth {
    /// Every probe failed to reach a server. LM Studio not running is an
    /// ordinary, expected local state, not an error.
    NotRunning,
    /// At least one probe was indeterminate and none was recognized. A hung or
    /// slow server is deliberately not reported as
    /// [`Self::NotRunning`].
    Indeterminate,
    /// The endpoint answered, but no probe returned a documented shape.
    RunningUnrecognized,
    /// At least one probed surface answered in its documented shape.
    ///
    /// A recognized OpenAI-compatible model list alone does not prove the
    /// server is LM Studio — many local runtimes serve one. Only a recognized
    /// native REST surface does; see
    /// [`LmStudioServerReport::identified_lm_studio`].
    Running,
}

/// Native LM Studio REST generation observed at the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioRestApi {
    /// No native REST generation was recognized.
    Unknown,
    /// Legacy generation v0 under `/api/v0/*`.
    V0,
    /// Current generation v1 under `/api/v1/*`.
    V1,
}

/// What the observed surfaces prove about the LM Studio application version.
///
/// LM Studio publishes no version, health or status endpoint, so an exact
/// release is never available over HTTP and is never claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioVersion {
    /// No reachable evidence bounds the version.
    Unknown,
    /// The observed API generation proves at least this published release.
    AtLeast(&'static str),
}

/// Whether the server required a credential from this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmStudioCredentialState {
    /// Nothing reachable proved either way.
    Unknown,
    /// An unauthenticated probe was accepted: no credential is required. This
    /// is LM Studio's documented default posture, proved rather than assumed.
    NotRequired,
    /// A probe carrying a configured bearer token was accepted. This does not
    /// prove the token was necessary.
    Accepted,
    /// The server rejected a probe with `401`/`403`: a token is required.
    Required,
}

/// One protocol verdict plus the surface that proved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LmStudioProtocolDetection {
    /// Protocol family this row is about.
    pub protocol: ProviderProtocol,
    /// Tri-state verdict. `Unknown` is never promoted to `Supported`, and this
    /// crate never emits `Unsupported`: a probe can prove a surface is present,
    /// never that it is absent.
    pub support: CapabilitySupport,
    /// The probed surface that proved `Supported`, when one did.
    pub evidence: Option<LmStudioSurface>,
}

/// Verdict of one LM Studio detection run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioServerReport {
    health: LmStudioHealth,
    version: LmStudioVersion,
    rest_api: LmStudioRestApi,
    credential: LmStudioCredentialState,
    protocols: Vec<LmStudioProtocolDetection>,
    observations: Vec<LmStudioSurfaceObservation>,
}

impl LmStudioServerReport {
    /// Reachability verdict.
    #[must_use]
    pub const fn health(&self) -> LmStudioHealth {
        self.health
    }

    /// Version lower bound proved by the observed API generation.
    #[must_use]
    pub const fn version(&self) -> LmStudioVersion {
        self.version
    }

    /// Native REST generation observed at the endpoint.
    #[must_use]
    pub const fn rest_api(&self) -> LmStudioRestApi {
        self.rest_api
    }

    /// Whether the server required a credential from this run.
    #[must_use]
    pub const fn credential(&self) -> LmStudioCredentialState {
        self.credential
    }

    /// Per-protocol verdicts in a stable order.
    #[must_use]
    pub fn protocols(&self) -> &[LmStudioProtocolDetection] {
        &self.protocols
    }

    /// Every probed surface and what it observed, in probe order.
    #[must_use]
    pub fn observations(&self) -> &[LmStudioSurfaceObservation] {
        &self.observations
    }

    /// What one specific surface observed.
    #[must_use]
    pub fn observation(&self, surface: LmStudioSurface) -> Option<LmStudioProbe> {
        self.observations
            .iter()
            .find(|observation| observation.surface == surface)
            .map(|observation| observation.probe)
    }

    /// Verdict for one protocol family.
    #[must_use]
    pub fn protocol(&self, protocol: ProviderProtocol) -> CapabilitySupport {
        self.protocols
            .iter()
            .find(|row| row.protocol == protocol)
            .map_or(CapabilitySupport::Unknown, |row| row.support)
    }

    /// Whether this run proved the endpoint is LM Studio itself.
    ///
    /// Only LM Studio's own REST surface proves the product; a recognized
    /// OpenAI-compatible model list is served by many local runtimes and proves
    /// only the protocol surface.
    #[must_use]
    pub const fn identified_lm_studio(&self) -> bool {
        matches!(self.rest_api, LmStudioRestApi::V1 | LmStudioRestApi::V0)
    }

    /// Safe provider descriptor built from this run's evidence.
    ///
    /// A protocol appears only when this run proved it. With no proved
    /// protocol the descriptor carries the conservative
    /// [`ProviderProtocol::Unknown`] row rather than an empty list, matching
    /// the default `Provider::descriptor` contract.
    #[must_use]
    pub fn provider_descriptor(&self) -> ProviderDescriptor {
        let protocols: Vec<ProviderProtocol> = self
            .protocols
            .iter()
            .filter(|row| row.support == CapabilitySupport::Supported)
            .map(|row| row.protocol)
            .collect();
        ProviderDescriptor {
            id: LM_STUDIO_PROVIDER.to_owned(),
            display_name: LM_STUDIO_DISPLAY_NAME.to_owned(),
            protocols: if protocols.is_empty() {
                vec![ProviderProtocol::Unknown]
            } else {
                protocols
            },
        }
    }

    /// Derive one complete verdict from the run's observations.
    ///
    /// `authenticated` states whether the probes actually carried a resolved
    /// bearer token, which is what separates a proved "no credential required"
    /// from an accepted authenticated probe.
    pub(crate) fn from_observations(
        observations: Vec<LmStudioSurfaceObservation>,
        authenticated: bool,
    ) -> Self {
        let probe = |surface: LmStudioSurface| {
            observations
                .iter()
                .find(|observation| observation.surface == surface)
                .map_or(LmStudioProbe::Indeterminate, |observation| {
                    observation.probe
                })
        };
        let any = |wanted: LmStudioProbe| {
            observations
                .iter()
                .any(|observation| observation.probe == wanted)
        };

        let rest_api = if probe(LmStudioSurface::NativeRestV1) == LmStudioProbe::Recognized {
            LmStudioRestApi::V1
        } else if probe(LmStudioSurface::NativeRestV0) == LmStudioProbe::Recognized {
            LmStudioRestApi::V0
        } else {
            LmStudioRestApi::Unknown
        };

        // Only the v1 generation carries a citable release. The changelog does
        // not bound when v0 appeared or when it will be withdrawn, so a v0-only
        // server proves no version at all.
        let version = match rest_api {
            LmStudioRestApi::V1 => LmStudioVersion::AtLeast(LM_STUDIO_NATIVE_REST_V1_RELEASE),
            LmStudioRestApi::V0 | LmStudioRestApi::Unknown => LmStudioVersion::Unknown,
        };

        let health = if any(LmStudioProbe::Recognized) {
            LmStudioHealth::Running
        } else if any(LmStudioProbe::Indeterminate) {
            LmStudioHealth::Indeterminate
        } else if any(LmStudioProbe::Unrecognized) || any(LmStudioProbe::Unauthorized) {
            LmStudioHealth::RunningUnrecognized
        } else {
            LmStudioHealth::NotRunning
        };

        // A rejection outranks an acceptance: if any documented surface demanded
        // a credential, then a credential is required, whatever another surface
        // served without one.
        let credential = if any(LmStudioProbe::Unauthorized) {
            LmStudioCredentialState::Required
        } else if any(LmStudioProbe::Recognized) {
            if authenticated {
                LmStudioCredentialState::Accepted
            } else {
                LmStudioCredentialState::NotRequired
            }
        } else {
            LmStudioCredentialState::Unknown
        };

        let openai_compatible = probe(LmStudioSurface::OpenAiCompatible);
        let protocols = vec![
            // The observed `/v1/models` list is one endpoint of the same
            // OpenAI-compatible surface that serves `/v1/chat/completions`
            // (<https://lmstudio.ai/docs/app/api/endpoints/openai>). This is a
            // surface fact, not a capability claim (GOTCHAS #22).
            LmStudioProtocolDetection {
                protocol: ProviderProtocol::OpenAiChatCompletions,
                support: if openai_compatible == LmStudioProbe::Recognized {
                    CapabilitySupport::Supported
                } else {
                    CapabilitySupport::Unknown
                },
                evidence: if openai_compatible == LmStudioProbe::Recognized {
                    Some(LmStudioSurface::OpenAiCompatible)
                } else {
                    None
                },
            },
            // `POST /v1/responses` arrived in LM Studio 0.3.29 and has no
            // observable `GET` surface, so nothing reachable distinguishes a
            // server that routes it. A version lower bound is not an
            // observation of the protocol, so this stays Unknown.
            // <https://lmstudio.ai/docs/developer/api-changelog>
            LmStudioProtocolDetection {
                protocol: ProviderProtocol::OpenAiResponses,
                support: CapabilitySupport::Unknown,
                evidence: None,
            },
            // `POST /v1/messages` arrived in LM Studio 0.4.1, one release after
            // the newest generation this crate can observe, and likewise has no
            // observable `GET` surface.
            // <https://lmstudio.ai/docs/developer/api-changelog>
            LmStudioProtocolDetection {
                protocol: ProviderProtocol::AnthropicMessages,
                support: CapabilitySupport::Unknown,
                evidence: None,
            },
        ];

        Self {
            health,
            version,
            rest_api,
            credential,
            protocols,
            observations,
        }
    }
}
