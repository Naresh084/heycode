//! Safe vocabulary: tri-state health, uncertainty reasons, validated ids and
//! the origins a fact came from.
//!
//! Nothing in this module can hold credential material. Origins name *where* a
//! credential was found — an environment variable name, a well-known path, the
//! ambient metadata host — and never what it contains.

use std::fmt;
use std::path::PathBuf;

/// Tri-state health verdict for one subject of the GCP auth profile.
///
/// This enum is deliberately closed and deliberately three-valued.
/// [`Self::Unknown`] means the check could not reach a determinate answer; it
/// is never a synonym for [`Self::Healthy`] and never for [`Self::Unhealthy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpHealth {
    /// Determinate evidence proves the subject usable.
    Healthy,
    /// The check did not reach a determinate answer.
    Unknown,
    /// Determinate evidence proves the subject unusable or unset.
    Unhealthy,
}

impl GcpHealth {
    /// Combine two verdicts, keeping the weaker claim.
    ///
    /// Determinate failure dominates uncertainty, and uncertainty dominates
    /// health: a profile is only as healthy as its weakest subject.
    #[must_use]
    pub const fn weaker(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unhealthy, _) | (_, Self::Unhealthy) => Self::Unhealthy,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Healthy, Self::Healthy) => Self::Healthy,
        }
    }

    /// Stable machine code for diagnostics and UI.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unknown => "unknown",
            Self::Unhealthy => "unhealthy",
        }
    }
}

impl fmt::Display for GcpHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Why a check could not reach a determinate verdict.
///
/// Every variant is a reason the answer is *unknown*. None of them may be
/// rendered as a negative finding: "the metadata server did not answer" is not
/// "there are no credentials".
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpUncertainty {
    /// The caller's policy forbade contacting the ambient metadata server.
    ProbeDisabled,
    /// `NO_GCE_CHECK=true` suppressed the ambient metadata probe.
    ProbeSuppressed,
    /// `GCE_METADATA_HOST` is not a bare host or `host:port`.
    ProbeHostInvalid,
    /// The metadata probe crossed its deadline.
    ProbeTimedOut,
    /// The metadata probe failed before any HTTP response arrived.
    ProbeUnreachable,
    /// The metadata server answered with a status this probe cannot interpret.
    ProbeUnexpectedStatus,
    /// Caller cancellation won before the probe settled.
    Cancelled,
    /// The gcloud configuration directory could not be located, so the
    /// well-known credential file can be neither found nor ruled out.
    ConfigDirectoryUnknown,
    /// No ambient host attestation exists for this fact, and confirming it
    /// would need an authenticated call this profile never makes.
    NoAmbientAttestation,
    /// The ambient host attests a different value than the configured one.
    AmbientAttestationDiffers,
}

impl GcpUncertainty {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProbeDisabled => "probe-disabled",
            Self::ProbeSuppressed => "probe-suppressed",
            Self::ProbeHostInvalid => "probe-host-invalid",
            Self::ProbeTimedOut => "probe-timed-out",
            Self::ProbeUnreachable => "probe-unreachable",
            Self::ProbeUnexpectedStatus => "probe-unexpected-status",
            Self::Cancelled => "cancelled",
            Self::ConfigDirectoryUnknown => "config-directory-unknown",
            Self::NoAmbientAttestation => "no-ambient-attestation",
            Self::AmbientAttestationDiffers => "ambient-attestation-differs",
        }
    }

    /// Safe one-line explanation containing no host data.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ProbeDisabled => "the ambient metadata server was not probed",
            Self::ProbeSuppressed => "NO_GCE_CHECK suppressed the ambient metadata probe",
            Self::ProbeHostInvalid => "GCE_METADATA_HOST is not a bare host or host:port",
            Self::ProbeTimedOut => "the ambient metadata probe crossed its deadline",
            Self::ProbeUnreachable => "the ambient metadata server could not be reached",
            Self::ProbeUnexpectedStatus => {
                "the ambient metadata server answered with an unexpected status"
            }
            Self::Cancelled => "the check was cancelled before it settled",
            Self::ConfigDirectoryUnknown => {
                "the gcloud configuration directory could not be located"
            }
            Self::NoAmbientAttestation => {
                "no ambient host attestation is available and no authenticated call was made"
            }
            Self::AmbientAttestationDiffers => "the ambient host attests a different value",
        }
    }
}

impl fmt::Display for GcpUncertainty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

/// Why a candidate project value is not a usable project identity.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpProjectIdError {
    /// The value is empty or blank.
    Empty,
    /// A project id must be 6 to 30 characters.
    Length,
    /// A project id must start with a lowercase letter and contain only
    /// lowercase letters, digits and hyphens.
    Charset,
    /// A project id cannot end with a hyphen.
    TrailingHyphen,
    /// A numeric project number must be 1 to 20 digits without a leading zero.
    Number,
}

impl GcpProjectIdError {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Length => "length",
            Self::Charset => "charset",
            Self::TrailingHyphen => "trailing-hyphen",
            Self::Number => "number",
        }
    }
}

impl fmt::Display for GcpProjectIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Which form a validated project identity takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpProjectIdKind {
    /// A project id: 6-30 characters, `[a-z][a-z0-9-]*`, no trailing hyphen.
    Id,
    /// An automatically generated numeric project number.
    Number,
}

/// A validated Google Cloud project identity.
///
/// Both the project id and the automatically generated project number are
/// accepted because Vertex AI resource paths accept either.
///
/// Rules follow the documented project id constraints: 6 to 30 characters,
/// lowercase letters, digits and hyphens only, starting with a letter and not
/// ending with a hyphen
/// (<https://docs.cloud.google.com/resource-manager/docs/creating-managing-projects>).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GcpProjectId(String);

impl GcpProjectId {
    /// Validate a project id or project number.
    ///
    /// # Errors
    /// Returns the exact [`GcpProjectIdError`] the value violated.
    pub fn new(value: impl Into<String>) -> Result<Self, GcpProjectIdError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(GcpProjectIdError::Empty);
        }
        let bytes = value.as_bytes();
        if bytes.iter().all(u8::is_ascii_digit) {
            if bytes.len() > 20 || bytes.first() == Some(&b'0') {
                return Err(GcpProjectIdError::Number);
            }
            return Ok(Self(value));
        }
        if !bytes.first().is_some_and(u8::is_ascii_lowercase)
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err(GcpProjectIdError::Charset);
        }
        if bytes.last() == Some(&b'-') {
            return Err(GcpProjectIdError::TrailingHyphen);
        }
        if !(6..=30).contains(&bytes.len()) {
            return Err(GcpProjectIdError::Length);
        }
        Ok(Self(value))
    }

    /// Whether this identity is an id or a generated number.
    #[must_use]
    pub fn kind(&self) -> GcpProjectIdKind {
        if self.0.as_bytes().iter().all(u8::is_ascii_digit) {
            GcpProjectIdKind::Number
        } else {
            GcpProjectIdKind::Id
        }
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GcpProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a candidate location value is not a Vertex AI location.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpLocationError {
    /// The value is empty or blank.
    Empty,
    /// The value contains bytes outside lowercase letters, digits and hyphens.
    Charset,
    /// The value names a zone. Vertex AI takes a region or `global`.
    ZoneNotRegion,
    /// The value is neither `global` nor `<area>-<direction><number>`.
    Shape,
}

impl GcpLocationError {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Charset => "charset",
            Self::ZoneNotRegion => "zone-not-region",
            Self::Shape => "shape",
        }
    }
}

impl fmt::Display for GcpLocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Which form a validated location takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpLocationKind {
    /// The multi-region `global` endpoint.
    Global,
    /// One regional location such as `us-central1`.
    Region,
}

/// A validated Vertex AI location.
///
/// `global` and region identifiers are accepted; a zone is refused explicitly
/// because pasting a zone where a region belongs is a common and otherwise
/// silent misconfiguration. The value is shape-validated rather than checked
/// against a compiled region list, which would go stale the moment Google adds
/// a region.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GcpLocation(String);

impl GcpLocation {
    /// Validate a Vertex AI location.
    ///
    /// # Errors
    /// Returns the exact [`GcpLocationError`] the value violated.
    pub fn new(value: impl Into<String>) -> Result<Self, GcpLocationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(GcpLocationError::Empty);
        }
        if value.len() > 63
            || !value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err(GcpLocationError::Charset);
        }
        if value == "global" {
            return Ok(Self(value));
        }
        let segments: Vec<&str> = value.split('-').collect();
        if segments.len() == 3
            && is_area(segments[0])
            && is_direction_with_number(segments[1])
            && segments[2].len() == 1
            && segments[2].as_bytes()[0].is_ascii_lowercase()
        {
            return Err(GcpLocationError::ZoneNotRegion);
        }
        if segments.len() == 2 && is_area(segments[0]) && is_direction_with_number(segments[1]) {
            return Ok(Self(value));
        }
        Err(GcpLocationError::Shape)
    }

    /// Whether this location is `global` or a region.
    #[must_use]
    pub fn kind(&self) -> GcpLocationKind {
        if self.0 == "global" {
            GcpLocationKind::Global
        } else {
            GcpLocationKind::Region
        }
    }

    /// Stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GcpLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn is_area(segment: &str) -> bool {
    !segment.is_empty() && segment.as_bytes().iter().all(u8::is_ascii_lowercase)
}

fn is_direction_with_number(segment: &str) -> bool {
    let letters = segment
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_lowercase())
        .count();
    let digits = segment.len() - letters;
    letters > 0 && digits > 0 && segment.as_bytes()[letters..].iter().all(u8::is_ascii_digit)
}

/// Documented `type` values of an Application Default Credentials document.
///
/// The set mirrors the types google-auth accepts when loading ADC
/// (<https://github.com/googleapis/google-auth-library-python/blob/main/google/auth/_default.py>).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpAdcCredentialType {
    /// A service account key.
    ServiceAccount,
    /// A user credential produced by `gcloud auth application-default login`.
    AuthorizedUser,
    /// A workload/workforce identity federation configuration.
    ExternalAccount,
    /// An authorized-user credential obtained through identity federation.
    ExternalAccountAuthorizedUser,
    /// A service-account impersonation configuration.
    ImpersonatedServiceAccount,
    /// A Google Distributed Cloud Hosted service account.
    GdchServiceAccount,
}

impl GcpAdcCredentialType {
    /// Parse the documented wire value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "service_account" => Some(Self::ServiceAccount),
            "authorized_user" => Some(Self::AuthorizedUser),
            "external_account" => Some(Self::ExternalAccount),
            "external_account_authorized_user" => Some(Self::ExternalAccountAuthorizedUser),
            "impersonated_service_account" => Some(Self::ImpersonatedServiceAccount),
            "gdch_service_account" => Some(Self::GdchServiceAccount),
            _ => None,
        }
    }

    /// Documented wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServiceAccount => "service_account",
            Self::AuthorizedUser => "authorized_user",
            Self::ExternalAccount => "external_account",
            Self::ExternalAccountAuthorizedUser => "external_account_authorized_user",
            Self::ImpersonatedServiceAccount => "impersonated_service_account",
            Self::GdchServiceAccount => "gdch_service_account",
        }
    }
}

impl fmt::Display for GcpAdcCredentialType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where an Application Default Credentials source was found.
///
/// An origin is a *location*, never contents. Reporting that a credential came
/// from `GOOGLE_APPLICATION_CREDENTIALS` or from a well-known path is what
/// makes a health report actionable; reporting the bytes at that path is not.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcpAdcOrigin {
    /// A process environment variable named this credential file.
    EnvironmentVariable {
        /// Environment variable name.
        name: &'static str,
        /// Path the variable named.
        path: PathBuf,
    },
    /// The gcloud well-known application default credentials file.
    WellKnownFile {
        /// Resolved path.
        path: PathBuf,
    },
    /// The service account attached to the ambient host.
    MetadataServer {
        /// Metadata host that answered.
        host: String,
    },
}

impl fmt::Display for GcpAdcOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvironmentVariable { name, path } => {
                write!(formatter, "{name}={}", path.display())
            }
            Self::WellKnownFile { path } => write!(formatter, "gcloud file {}", path.display()),
            Self::MetadataServer { host } => write!(formatter, "metadata server {host}"),
        }
    }
}

/// Why a configured Application Default Credentials source is unusable.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpAdcFault {
    /// The named path does not exist.
    FileMissing,
    /// The path exists but could not be read.
    FileUnreadable,
    /// The document is larger than the accepted bound.
    FileTooLarge,
    /// The bytes are not a UTF-8 JSON object.
    NotJsonObject,
    /// The document has no `type` field.
    TypeMissing,
    /// The `type` field is not a documented ADC credential type.
    TypeUnsupported,
}

impl GcpAdcFault {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::FileMissing => "file-missing",
            Self::FileUnreadable => "file-unreadable",
            Self::FileTooLarge => "file-too-large",
            Self::NotJsonObject => "not-json-object",
            Self::TypeMissing => "type-missing",
            Self::TypeUnsupported => "type-unsupported",
        }
    }

    /// Safe one-line explanation containing no document bytes.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::FileMissing => "the configured credential file does not exist",
            Self::FileUnreadable => "the configured credential file could not be read",
            Self::FileTooLarge => "the configured credential file exceeds the accepted size",
            Self::NotJsonObject => {
                "the configured credential file is not a readable JSON credential document"
            }
            Self::TypeMissing => "the credential document has no `type` field",
            Self::TypeUnsupported => "the credential document declares an unsupported `type`",
        }
    }
}

impl fmt::Display for GcpAdcFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

/// Where an effective project identity came from.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpProjectOrigin {
    /// The caller supplied it explicitly.
    Explicit,
    /// A process environment variable.
    EnvironmentVariable(&'static str),
    /// The `project_id` field of the resolved credential document.
    CredentialDocument,
    /// `project/project-id` on the ambient metadata server.
    MetadataServer,
}

impl fmt::Display for GcpProjectOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => formatter.write_str("explicit"),
            Self::EnvironmentVariable(name) => formatter.write_str(name),
            Self::CredentialDocument => formatter.write_str("credential document"),
            Self::MetadataServer => formatter.write_str("metadata server"),
        }
    }
}

/// Where an effective location came from.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpLocationOrigin {
    /// The caller supplied it explicitly.
    Explicit,
    /// A process environment variable.
    EnvironmentVariable(&'static str),
    /// The region of `instance/zone` on the ambient metadata server.
    MetadataZone,
}

impl fmt::Display for GcpLocationOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => formatter.write_str("explicit"),
            Self::EnvironmentVariable(name) => formatter.write_str(name),
            Self::MetadataZone => formatter.write_str("metadata server zone"),
        }
    }
}
