//! Explicit resolution of one Google Cloud authentication profile.
//!
//! Defaulting happens here and nowhere else: `resolve(request) -> profile` is
//! the single step that turns caller intent plus the documented discovery
//! chain into three independent health verdicts. No consumer re-derives a
//! fallback later.
//!
//! `resolve` is infallible by construction. A health check that could return
//! `Err` would collapse the tri-state, because a caller would have to decide
//! whether an error meant "unhealthy" or "unknown". Every failure is instead a
//! named state on the subject it concerns.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use heycode_http::HttpService;
use tokio_util::sync::CancellationToken;

use crate::env::{GcpEnvironment, GcpFileError};
use crate::health::{GcpAccountHealth, GcpAuthProfile, GcpLocationHealth, GcpProjectHealth};
use crate::metadata::{MetadataOutcome, probe, probe_suppressed, resolve_host};
use crate::model::{
    GcpAdcCredentialType, GcpAdcFault, GcpAdcOrigin, GcpLocation, GcpLocationOrigin, GcpProjectId,
    GcpProjectOrigin, GcpUncertainty,
};

/// Environment variable naming an explicit ADC credential file.
pub const ENV_GOOGLE_APPLICATION_CREDENTIALS: &str = "GOOGLE_APPLICATION_CREDENTIALS";
/// Environment variable overriding the gcloud configuration directory.
pub const ENV_CLOUDSDK_CONFIG: &str = "CLOUDSDK_CONFIG";
/// Environment variable holding the Unix home directory.
pub const ENV_HOME: &str = "HOME";
/// Environment variable holding the Windows roaming application data root.
pub const ENV_APPDATA: &str = "APPDATA";
/// Environment variable holding the Windows system drive.
pub const ENV_SYSTEM_DRIVE: &str = "SystemDrive";
/// Environment variable naming the default project.
pub const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// Superseded environment variable naming the default project.
pub const ENV_GCLOUD_PROJECT: &str = "GCLOUD_PROJECT";
/// gcloud `core/project` property environment variable.
pub const ENV_CLOUDSDK_CORE_PROJECT: &str = "CLOUDSDK_CORE_PROJECT";
/// Environment variable naming the Vertex AI location.
pub const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
/// gcloud `compute/region` property environment variable.
pub const ENV_CLOUDSDK_COMPUTE_REGION: &str = "CLOUDSDK_COMPUTE_REGION";
/// File name of the gcloud well-known application default credentials file.
pub const ADC_FILE_NAME: &str = "application_default_credentials.json";
/// Directory name gcloud uses inside its configuration root.
pub const GCLOUD_CONFIG_DIR: &str = "gcloud";

/// Largest accepted Application Default Credentials document.
const MAX_ADC_BYTES: usize = 64 * 1024;

/// Which well-known gcloud configuration layout applies to this host.
///
/// The platform is an input rather than a compile-time branch so both layouts
/// are compiled and tested on every host, instead of one of them becoming a
/// `cfg` island that only one CI leg ever sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpHostPlatform {
    /// Linux and macOS: `$HOME/.config/gcloud`.
    Unix,
    /// Windows: `%APPDATA%\gcloud`, else `%SystemDrive%\gcloud`.
    Windows,
}

impl GcpHostPlatform {
    /// The layout of the host this binary runs on.
    #[must_use]
    pub const fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Whether the resolver may contact the ambient metadata server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcpMetadataPolicy {
    /// Never contact it. Every ambient fact stays undetermined.
    Disabled,
    /// Probe with this total budget across the probe's requests.
    Probe {
        /// Total deadline for the probe.
        budget: Duration,
    },
}

impl GcpMetadataPolicy {
    /// Default probe budget.
    ///
    /// The budget belongs to the caller, not to the environment: heycode
    /// deliberately does not read `GCE_METADATA_TIMEOUT`, so a health check
    /// cannot be stalled by ambient configuration.
    pub const DEFAULT_BUDGET: Duration = Duration::from_millis(1000);

    /// Probe with [`Self::DEFAULT_BUDGET`].
    #[must_use]
    pub const fn probe() -> Self {
        Self::Probe {
            budget: Self::DEFAULT_BUDGET,
        }
    }
}

/// One profile resolution request.
#[derive(Debug, Clone)]
pub struct GcpProfileRequest {
    /// Caller-supplied project override. Wins over every discovered source.
    pub project: Option<String>,
    /// Caller-supplied location override. Wins over every discovered source.
    pub location: Option<String>,
    /// Which well-known gcloud layout applies.
    pub platform: GcpHostPlatform,
    /// Whether the ambient metadata server may be probed.
    pub metadata: GcpMetadataPolicy,
}

impl Default for GcpProfileRequest {
    fn default() -> Self {
        Self {
            project: None,
            location: None,
            platform: GcpHostPlatform::host(),
            metadata: GcpMetadataPolicy::probe(),
        }
    }
}

/// Resolves Application Default Credentials, project and location health.
#[derive(Clone)]
pub struct GcpAuthService {
    environment: Arc<dyn GcpEnvironment>,
    http: HttpService,
}

impl GcpAuthService {
    /// Build over an environment boundary and the composed HTTP transport.
    #[must_use]
    pub fn new(environment: Arc<dyn GcpEnvironment>, http: HttpService) -> Self {
        Self { environment, http }
    }

    /// Resolve one profile and check account, project and location health.
    ///
    /// Cancellation is honored before and during the ambient probe; a
    /// cancelled probe leaves every fact that depended on it undetermined and
    /// never produces a negative finding.
    pub async fn resolve(
        &self,
        request: GcpProfileRequest,
        cancellation: CancellationToken,
    ) -> GcpAuthProfile {
        let metadata = self.probe_metadata(&request, cancellation).await;
        let environment = self.environment.as_ref();
        let account = resolve_account(&request, environment, &metadata);
        let project = resolve_project(&request, environment, account.document_project, &metadata);
        let location = resolve_location(&request, environment, &metadata);
        GcpAuthProfile::new(account.health, project, location, now_ms())
    }

    async fn probe_metadata(
        &self,
        request: &GcpProfileRequest,
        cancellation: CancellationToken,
    ) -> MetadataOutcome {
        let GcpMetadataPolicy::Probe { budget } = request.metadata else {
            return MetadataOutcome::Undetermined(GcpUncertainty::ProbeDisabled);
        };
        if cancellation.is_cancelled() {
            return MetadataOutcome::Undetermined(GcpUncertainty::Cancelled);
        }
        if probe_suppressed(self.environment.as_ref()) {
            return MetadataOutcome::Undetermined(GcpUncertainty::ProbeSuppressed);
        }
        let host = match resolve_host(self.environment.as_ref()) {
            Ok(host) => host,
            Err(reason) => return MetadataOutcome::Undetermined(reason),
        };
        probe(&self.http, &host, budget, cancellation).await
    }
}

struct AccountResolution {
    health: GcpAccountHealth,
    /// `Err` means a higher-precedence source could not be inspected, so the
    /// document's project id can be neither read nor ruled out.
    document_project: Result<Option<String>, GcpUncertainty>,
}

fn resolve_account(
    request: &GcpProfileRequest,
    environment: &dyn GcpEnvironment,
    metadata: &MetadataOutcome,
) -> AccountResolution {
    if let Some(raw) = environment.var(ENV_GOOGLE_APPLICATION_CREDENTIALS) {
        let path = PathBuf::from(raw);
        let origin = GcpAdcOrigin::EnvironmentVariable {
            name: ENV_GOOGLE_APPLICATION_CREDENTIALS,
            path: path.clone(),
        };
        return match environment.read_file(&path, MAX_ADC_BYTES) {
            Ok(bytes) => from_document(&bytes, origin),
            Err(GcpFileError::NotFound) => faulted(origin, GcpAdcFault::FileMissing),
            Err(GcpFileError::Unreadable) => faulted(origin, GcpAdcFault::FileUnreadable),
            Err(GcpFileError::TooLarge) => faulted(origin, GcpAdcFault::FileTooLarge),
        };
    }

    let path = match well_known_adc_path(environment, request.platform) {
        Ok(path) => path,
        Err(reason) => {
            return AccountResolution {
                health: GcpAccountHealth::Undetermined { reason },
                document_project: Err(reason),
            };
        }
    };
    let origin = GcpAdcOrigin::WellKnownFile { path: path.clone() };
    match environment.read_file(&path, MAX_ADC_BYTES) {
        Ok(bytes) => return from_document(&bytes, origin),
        Err(GcpFileError::Unreadable) => {
            return faulted(origin, GcpAdcFault::FileUnreadable);
        }
        Err(GcpFileError::TooLarge) => return faulted(origin, GcpAdcFault::FileTooLarge),
        Err(GcpFileError::NotFound) => {}
    }

    match metadata {
        MetadataOutcome::Answered(facts) if facts.service_account_attached => AccountResolution {
            health: GcpAccountHealth::Configured {
                origin: GcpAdcOrigin::MetadataServer {
                    host: facts.host.clone(),
                },
                credential: None,
            },
            document_project: Ok(None),
        },
        MetadataOutcome::Answered(_) | MetadataOutcome::NotPresent => AccountResolution {
            health: GcpAccountHealth::Absent,
            document_project: Ok(None),
        },
        // Both document sources were inspected determinately above, so the
        // document carries no project id even though the account itself is
        // unknown. Only an uninspectable document source may return `Err`.
        MetadataOutcome::Undetermined(reason) => AccountResolution {
            health: GcpAccountHealth::Undetermined { reason: *reason },
            document_project: Ok(None),
        },
    }
}

fn faulted(origin: GcpAdcOrigin, fault: GcpAdcFault) -> AccountResolution {
    AccountResolution {
        health: GcpAccountHealth::Faulted { origin, fault },
        document_project: Ok(None),
    }
}

/// Extract only the two safe fields from an ADC document.
///
/// A `service_account` document carries `private_key`, and an
/// `authorized_user` document carries `client_secret` and `refresh_token`.
/// Nothing but `type` and `project_id` is ever bound to a name here, and the
/// borrowed bytes are dropped by the caller immediately afterwards.
fn from_document(bytes: &[u8], origin: GcpAdcOrigin) -> AccountResolution {
    #[derive(serde::Deserialize)]
    struct SafeFields {
        #[serde(rename = "type")]
        kind: Option<String>,
        project_id: Option<String>,
    }

    let Ok(fields) = serde_json::from_slice::<SafeFields>(bytes) else {
        return faulted(origin, GcpAdcFault::NotJsonObject);
    };
    let Some(kind) = fields.kind else {
        return faulted(origin, GcpAdcFault::TypeMissing);
    };
    let Some(credential) = GcpAdcCredentialType::parse(&kind) else {
        return faulted(origin, GcpAdcFault::TypeUnsupported);
    };
    AccountResolution {
        health: GcpAccountHealth::Configured {
            origin,
            credential: Some(credential),
        },
        document_project: Ok(fields.project_id),
    }
}

fn well_known_adc_path(
    environment: &dyn GcpEnvironment,
    platform: GcpHostPlatform,
) -> Result<PathBuf, GcpUncertainty> {
    if let Some(directory) = environment.var(ENV_CLOUDSDK_CONFIG) {
        return Ok(PathBuf::from(directory).join(ADC_FILE_NAME));
    }
    match platform {
        GcpHostPlatform::Unix => environment
            .var(ENV_HOME)
            .map(|home| {
                PathBuf::from(home)
                    .join(".config")
                    .join(GCLOUD_CONFIG_DIR)
                    .join(ADC_FILE_NAME)
            })
            .ok_or(GcpUncertainty::ConfigDirectoryUnknown),
        GcpHostPlatform::Windows => {
            let root = environment.var(ENV_APPDATA).unwrap_or_else(|| {
                environment
                    .var(ENV_SYSTEM_DRIVE)
                    .unwrap_or_else(|| "C:".to_owned())
            });
            let root = root.trim_end_matches('\\');
            Ok(PathBuf::from(format!(
                "{root}\\{GCLOUD_CONFIG_DIR}\\{ADC_FILE_NAME}"
            )))
        }
    }
}

fn resolve_project(
    request: &GcpProfileRequest,
    environment: &dyn GcpEnvironment,
    document_project: Result<Option<String>, GcpUncertainty>,
    metadata: &MetadataOutcome,
) -> GcpProjectHealth {
    if let Some(raw) = request.project.as_deref() {
        return classify_project(raw, GcpProjectOrigin::Explicit, metadata);
    }
    for name in [ENV_GOOGLE_CLOUD_PROJECT, ENV_GCLOUD_PROJECT] {
        if let Some(raw) = environment.var(name) {
            return classify_project(&raw, GcpProjectOrigin::EnvironmentVariable(name), metadata);
        }
    }
    match document_project {
        Err(reason) => return GcpProjectHealth::Undetermined { reason },
        Ok(Some(raw)) => {
            return classify_project(&raw, GcpProjectOrigin::CredentialDocument, metadata);
        }
        Ok(None) => {}
    }
    if let Some(raw) = environment.var(ENV_CLOUDSDK_CORE_PROJECT) {
        return classify_project(
            &raw,
            GcpProjectOrigin::EnvironmentVariable(ENV_CLOUDSDK_CORE_PROJECT),
            metadata,
        );
    }
    match metadata {
        MetadataOutcome::Answered(facts) => match &facts.project {
            Ok(Some(raw)) => classify_project(raw, GcpProjectOrigin::MetadataServer, metadata),
            Ok(None) => GcpProjectHealth::Unset,
            Err(reason) => GcpProjectHealth::Undetermined { reason: *reason },
        },
        MetadataOutcome::NotPresent => GcpProjectHealth::Unset,
        MetadataOutcome::Undetermined(reason) => GcpProjectHealth::Undetermined { reason: *reason },
    }
}

fn classify_project(
    raw: &str,
    origin: GcpProjectOrigin,
    metadata: &MetadataOutcome,
) -> GcpProjectHealth {
    let project = match GcpProjectId::new(raw) {
        Ok(project) => project,
        Err(error) => return GcpProjectHealth::Malformed { origin, error },
    };
    if origin == GcpProjectOrigin::MetadataServer {
        return GcpProjectHealth::Confirmed { project, origin };
    }
    let reason = match metadata {
        MetadataOutcome::Answered(facts) => match &facts.project {
            Ok(Some(ambient)) if ambient == project.as_str() => {
                return GcpProjectHealth::Confirmed { project, origin };
            }
            Ok(Some(_)) => GcpUncertainty::AmbientAttestationDiffers,
            Ok(None) => GcpUncertainty::NoAmbientAttestation,
            Err(reason) => *reason,
        },
        MetadataOutcome::NotPresent => GcpUncertainty::NoAmbientAttestation,
        MetadataOutcome::Undetermined(reason) => *reason,
    };
    GcpProjectHealth::Unconfirmed {
        project,
        origin,
        reason,
    }
}

fn resolve_location(
    request: &GcpProfileRequest,
    environment: &dyn GcpEnvironment,
    metadata: &MetadataOutcome,
) -> GcpLocationHealth {
    if let Some(raw) = request.location.as_deref() {
        return classify_location(raw, GcpLocationOrigin::Explicit);
    }
    for name in [ENV_GOOGLE_CLOUD_LOCATION, ENV_CLOUDSDK_COMPUTE_REGION] {
        if let Some(raw) = environment.var(name) {
            return classify_location(&raw, GcpLocationOrigin::EnvironmentVariable(name));
        }
    }
    match metadata {
        MetadataOutcome::Answered(facts) => match &facts.zone_region {
            Ok(Some(raw)) => classify_location(raw, GcpLocationOrigin::MetadataZone),
            Ok(None) => GcpLocationHealth::Unset,
            Err(reason) => GcpLocationHealth::Undetermined { reason: *reason },
        },
        MetadataOutcome::NotPresent => GcpLocationHealth::Unset,
        MetadataOutcome::Undetermined(reason) => {
            GcpLocationHealth::Undetermined { reason: *reason }
        }
    }
}

fn classify_location(raw: &str, origin: GcpLocationOrigin) -> GcpLocationHealth {
    match GcpLocation::new(raw) {
        Ok(location) => GcpLocationHealth::Selected { location, origin },
        Err(error) => GcpLocationHealth::Malformed { origin, error },
    }
}

fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}
