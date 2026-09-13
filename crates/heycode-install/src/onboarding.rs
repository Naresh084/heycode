//! Q16 fresh-machine matrix with explicit evidence provenance.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Operating-system cells required by Q16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingPlatform {
    /// Native macOS runner or host.
    Macos,
    /// Native Linux runner or host.
    Linux,
    /// Native Windows runner or host.
    Windows,
}

impl OnboardingPlatform {
    /// Exact three-platform Q16 matrix.
    pub const ALL: [Self; 3] = [Self::Macos, Self::Linux, Self::Windows];
}

/// Where one evidence row came from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceSource {
    /// Directly observed on the developer's native local host.
    LocalNative,
    /// Observed on a native hosted runner in this exact workflow run.
    HostedNative {
        /// Non-zero immutable workflow run id.
        run_id: u64,
    },
    /// A workflow or script exists, but has not produced an observation.
    WorkflowDefinition,
    /// Target code compiled elsewhere; no target kernel/process ran it.
    CrossCompiled,
}

impl EvidenceSource {
    const fn is_native_observation(&self) -> bool {
        matches!(self, Self::LocalNative | Self::HostedNative { .. })
    }
}

/// What kind of turn reached terminal success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEvidence {
    /// Deterministic offline fake-provider turn; useful harness evidence only.
    DeterministicFake,
    /// Credentialed real-provider turn required by Q16 acceptance.
    RealProvider,
}

/// One required fresh-machine checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "check", content = "evidence", rename_all = "snake_case")]
pub enum FreshMachineCheck {
    /// Artifact signature/provenance verified before execution.
    AttestationVerified,
    /// Empty installation root gained the intended version.
    FreshInstall,
    /// First startup reached the ready user path without hand-editing state.
    FirstRunReady,
    /// One complete terminal turn settled.
    TurnCompleted(TurnEvidence),
}

/// One immutable observed or definition-only matrix fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FreshMachineObservation {
    platform: OnboardingPlatform,
    source: EvidenceSource,
    check: FreshMachineCheck,
    passed: bool,
}

impl FreshMachineObservation {
    /// Construct a successful fact.
    ///
    /// # Errors
    /// A hosted observation with run id zero is rejected.
    pub fn passed(
        platform: OnboardingPlatform,
        source: EvidenceSource,
        check: FreshMachineCheck,
    ) -> Result<Self, FreshMachineMatrixError> {
        Self::new(platform, source, check, true)
    }

    /// Construct a failed fact.
    ///
    /// # Errors
    /// A hosted observation with run id zero is rejected.
    pub fn failed(
        platform: OnboardingPlatform,
        source: EvidenceSource,
        check: FreshMachineCheck,
    ) -> Result<Self, FreshMachineMatrixError> {
        Self::new(platform, source, check, false)
    }

    fn new(
        platform: OnboardingPlatform,
        source: EvidenceSource,
        check: FreshMachineCheck,
        passed: bool,
    ) -> Result<Self, FreshMachineMatrixError> {
        if matches!(source, EvidenceSource::HostedNative { run_id: 0 }) {
            return Err(FreshMachineMatrixError::InvalidHostedRun);
        }
        Ok(Self {
            platform,
            source,
            check,
            passed,
        })
    }
}

/// Invalid or contradictory matrix evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FreshMachineMatrixError {
    /// Hosted evidence must identify a real immutable run.
    #[error("hosted onboarding evidence requires a non-zero run id")]
    InvalidHostedRun,
    /// One source claimed both success and failure for the same check.
    #[error("fresh-machine evidence contains a contradictory result")]
    ContradictoryObservation,
    /// Evidence JSON was malformed, oversized, incomplete, or had unknown fields.
    #[error("fresh-machine evidence document is invalid")]
    InvalidDocument,
    /// Evidence schema is newer or older than the exact supported schema.
    #[error("fresh-machine evidence schema is unsupported")]
    UnsupportedSchema,
    /// Evidence named a platform outside the certified native matrix.
    #[error("fresh-machine evidence platform is unsupported")]
    UnsupportedPlatform,
}

/// Evidence for one operating-system cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformOnboardingEvidence {
    observations: Vec<FreshMachineObservation>,
}

impl PlatformOnboardingEvidence {
    /// Whether any native local/hosted check actually ran.
    #[must_use]
    pub fn native_observed(&self) -> bool {
        self.observations
            .iter()
            .any(|observation| observation.source.is_native_observation())
    }

    /// Whether no native local/hosted check ran.
    #[must_use]
    pub fn is_unobserved(&self) -> bool {
        !self.native_observed()
    }

    /// Whether one native run passed attestation, install, ready, and any turn.
    #[must_use]
    pub fn deterministic_native_complete(&self) -> bool {
        self.complete_for_turn(None)
    }

    /// Whether one native run passed the full real-provider Q16 sequence.
    #[must_use]
    pub fn q16_real_turn_complete(&self) -> bool {
        self.complete_for_turn(Some(TurnEvidence::RealProvider))
    }

    fn complete_for_turn(&self, required_turn: Option<TurnEvidence>) -> bool {
        let sources = self
            .observations
            .iter()
            .filter(|observation| observation.source.is_native_observation())
            .map(|observation| observation.source.clone())
            .collect::<BTreeSet<_>>();
        sources.into_iter().any(|source| {
            let passed = self
                .observations
                .iter()
                .filter(|observation| observation.source == source && observation.passed)
                .map(|observation| observation.check.clone())
                .collect::<BTreeSet<_>>();
            let turn_passed = match required_turn {
                Some(turn) => passed.contains(&FreshMachineCheck::TurnCompleted(turn)),
                None => passed
                    .iter()
                    .any(|check| matches!(check, FreshMachineCheck::TurnCompleted(_))),
            };
            passed.contains(&FreshMachineCheck::AttestationVerified)
                && passed.contains(&FreshMachineCheck::FreshInstall)
                && passed.contains(&FreshMachineCheck::FirstRunReady)
                && turn_passed
        })
    }
}

/// Deterministic evaluation of macOS, Linux, and Windows onboarding evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshMachineMatrix {
    platforms: BTreeMap<OnboardingPlatform, PlatformOnboardingEvidence>,
}

impl FreshMachineMatrix {
    /// Validate contradictory rows and evaluate the exact three-platform matrix.
    ///
    /// # Errors
    /// One source/check claiming both pass and fail is refused rather than
    /// allowing a green row to hide a red row.
    pub fn evaluate(
        observations: Vec<FreshMachineObservation>,
    ) -> Result<Self, FreshMachineMatrixError> {
        let mut verdicts = BTreeMap::new();
        for observation in &observations {
            let key = (
                observation.platform,
                observation.source.clone(),
                observation.check.clone(),
            );
            if let Some(previous) = verdicts.insert(key, observation.passed)
                && previous != observation.passed
            {
                return Err(FreshMachineMatrixError::ContradictoryObservation);
            }
        }
        let platforms = OnboardingPlatform::ALL
            .into_iter()
            .map(|platform| {
                let rows = observations
                    .iter()
                    .filter(|observation| observation.platform == platform)
                    .cloned()
                    .collect();
                (platform, PlatformOnboardingEvidence { observations: rows })
            })
            .collect();
        Ok(Self { platforms })
    }

    /// Evidence for one required platform.
    #[must_use]
    pub fn platform(&self, platform: OnboardingPlatform) -> &PlatformOnboardingEvidence {
        &self.platforms[&platform]
    }

    /// Whether every platform has a workflow-definition row.
    #[must_use]
    pub fn has_definition_for_all_platforms(&self) -> bool {
        self.platforms.values().all(|platform| {
            platform
                .observations
                .iter()
                .any(|observation| observation.source == EvidenceSource::WorkflowDefinition)
        })
    }

    /// Whether all three platforms completed the deterministic native harness.
    #[must_use]
    pub fn deterministic_native_complete(&self) -> bool {
        self.platforms
            .values()
            .all(PlatformOnboardingEvidence::deterministic_native_complete)
    }

    /// Whether all three platforms completed a real-provider native turn.
    #[must_use]
    pub fn q16_real_turn_complete(&self) -> bool {
        self.platforms
            .values()
            .all(PlatformOnboardingEvidence::q16_real_turn_complete)
    }
}

const FRESH_MACHINE_EVIDENCE_SCHEMA_VERSION: u32 = 1;
const MAX_FRESH_MACHINE_EVIDENCE_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvidenceDocument {
    schema_version: u32,
    platform: String,
    source: WireEvidenceSource,
    checks: Vec<WireEvidenceCheck>,
    turn: WireTurnEvidence,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireEvidenceSource {
    LocalNative,
    HostedNative { run_id: u64 },
    WorkflowDefinition,
    CrossCompiled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireEvidenceCheck {
    AttestationVerified,
    FreshInstall,
    FirstRunReady,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireTurnEvidence {
    DeterministicFake,
    RealProvider,
}

/// Parse one content-free workflow evidence document into matrix observations.
///
/// The document is strict schema v1 and must contain the complete attestation,
/// fresh-install, first-ready and turn sequence for one source/platform. It has
/// no field for provider content or credentials.
///
/// # Errors
/// Oversized/malformed/future documents, unsupported platforms, incomplete or
/// duplicate checks, and invalid hosted run ids fail closed.
pub fn parse_fresh_machine_evidence(
    bytes: &[u8],
) -> Result<Vec<FreshMachineObservation>, FreshMachineMatrixError> {
    if bytes.is_empty() || bytes.len() > MAX_FRESH_MACHINE_EVIDENCE_BYTES {
        return Err(FreshMachineMatrixError::InvalidDocument);
    }
    let wire = serde_json::from_slice::<WireEvidenceDocument>(bytes)
        .map_err(|_| FreshMachineMatrixError::InvalidDocument)?;
    if wire.schema_version != FRESH_MACHINE_EVIDENCE_SCHEMA_VERSION {
        return Err(FreshMachineMatrixError::UnsupportedSchema);
    }
    let platform = match wire.platform.as_str() {
        "macos-aarch64" | "macos-x86_64" => OnboardingPlatform::Macos,
        "linux-aarch64" | "linux-x86_64" => OnboardingPlatform::Linux,
        "windows-aarch64" | "windows-x86_64" => OnboardingPlatform::Windows,
        _ => return Err(FreshMachineMatrixError::UnsupportedPlatform),
    };
    let source = match wire.source {
        WireEvidenceSource::LocalNative => EvidenceSource::LocalNative,
        WireEvidenceSource::HostedNative { run_id } => EvidenceSource::HostedNative { run_id },
        WireEvidenceSource::WorkflowDefinition => EvidenceSource::WorkflowDefinition,
        WireEvidenceSource::CrossCompiled => EvidenceSource::CrossCompiled,
    };
    let checks = wire.checks.into_iter().collect::<BTreeSet<_>>();
    let required = BTreeSet::from([
        WireEvidenceCheck::AttestationVerified,
        WireEvidenceCheck::FreshInstall,
        WireEvidenceCheck::FirstRunReady,
    ]);
    if checks != required {
        return Err(FreshMachineMatrixError::InvalidDocument);
    }
    let turn = match wire.turn {
        WireTurnEvidence::DeterministicFake => TurnEvidence::DeterministicFake,
        WireTurnEvidence::RealProvider => TurnEvidence::RealProvider,
    };
    [
        FreshMachineCheck::AttestationVerified,
        FreshMachineCheck::FreshInstall,
        FreshMachineCheck::FirstRunReady,
        FreshMachineCheck::TurnCompleted(turn),
    ]
    .into_iter()
    .map(|check| FreshMachineObservation::passed(platform, source.clone(), check))
    .collect()
}
