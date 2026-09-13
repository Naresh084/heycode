//! Safe AWS credential status vocabulary.
//!
//! Every type here is a *report*: it names where a credential lives and what a
//! check concluded about it. None of it carries credential material, an
//! account id, a role ARN or an IAM Identity Center start URL, so the whole
//! module is safe to serialize, log and render.

use serde::Serialize;

use crate::{AwsProfileName, AwsProfileResolution, AwsRegionResolution};

/// Where a credential came from.
///
/// This is the line this crate draws around provenance: an environment
/// variable *name*, a profile *name* and a credential *reference* are safe to
/// publish because they say where to look. The value behind any of them is
/// not, and no variant has a field for one.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AwsCredentialSource {
    /// An explicit Amazon Bedrock API key behind a heycode credential reference.
    BedrockApiKey {
        /// Non-secret credential reference that resolved it.
        reference: heycode_credentials::CredentialReference,
    },
    /// Static access keys in the process environment.
    Environment {
        /// Variable that proved their presence.
        variable: &'static str,
    },
    /// A web-identity token file named by the process environment.
    WebIdentityToken {
        /// Variable that names the token file.
        variable: &'static str,
    },
    /// Static access keys in this profile's shared configuration.
    ProfileAccessKeys {
        /// Profile that carries them.
        profile: AwsProfileName,
    },
    /// A `credential_process` helper configured for this profile.
    CredentialProcess {
        /// Profile that configures the helper.
        profile: AwsProfileName,
    },
    /// An IAM Identity Center session configured for this profile.
    SsoSession {
        /// Profile that configures the session.
        profile: AwsProfileName,
    },
    /// A role this profile assumes.
    AssumeRole {
        /// Profile that configures the role.
        profile: AwsProfileName,
    },
    /// The Amazon ECS / EKS container credential endpoint.
    ContainerRole {
        /// Variable that names the endpoint.
        variable: &'static str,
    },
}

impl AwsCredentialSource {
    /// Stable machine code for diagnostics and wire projections.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BedrockApiKey { .. } => "bedrock-api-key",
            Self::Environment { .. } => "environment",
            Self::WebIdentityToken { .. } => "web-identity-token",
            Self::ProfileAccessKeys { .. } => "profile-access-keys",
            Self::CredentialProcess { .. } => "credential-process",
            Self::SsoSession { .. } => "sso-session",
            Self::AssumeRole { .. } => "assume-role",
            Self::ContainerRole { .. } => "container-role",
        }
    }
}

/// Why a discovered credential has no proven verdict.
///
/// Each arm is a reason a check did not conclude — never a verdict itself.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsUndetermined {
    /// No region resolved, so no service endpoint could be addressed.
    RegionUnresolved,
    /// The credential registry could not be asked.
    CredentialStoreUnavailable,
    /// Shared AWS configuration exists but could not be read.
    ConfigurationUnreadable,
    /// Proving this source requires a SigV4-signed request, which this crate
    /// composes no signer for.
    RequiresSignedRequest,
    /// The endpoint could not be reached, timed out, or answered 5xx/429.
    ServiceUnreachable,
    /// The endpoint answered, but not in a shape that proves anything.
    UnusableResponse,
    /// The caller cancelled before the check settled.
    Cancelled,
}

impl AwsUndetermined {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RegionUnresolved => "region-unresolved",
            Self::CredentialStoreUnavailable => "credential-store-unavailable",
            Self::ConfigurationUnreadable => "configuration-unreadable",
            Self::RequiresSignedRequest => "requires-signed-request",
            Self::ServiceUnreachable => "service-unreachable",
            Self::UnusableResponse => "unusable-response",
            Self::Cancelled => "cancelled",
        }
    }

    /// Safe actionable text. It quotes no response body and no configuration
    /// value.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::RegionUnresolved => "no AWS region is configured, so no endpoint was addressed",
            Self::CredentialStoreUnavailable => "the credential registry could not be inspected",
            Self::ConfigurationUnreadable => "shared AWS configuration exists but is unreadable",
            Self::RequiresSignedRequest => {
                "this credential source can only be proven by a signed AWS request"
            }
            Self::ServiceUnreachable => "no healthy AWS endpoint answered the check",
            Self::UnusableResponse => "the endpoint answered in an unrecognized shape",
            Self::Cancelled => "the check was cancelled",
        }
    }
}

/// Why a credential is proven unusable.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsRejection {
    /// AWS rejected the credential.
    Unauthorized,
    /// The configuration names a source but omits a part it requires.
    IncompleteConfiguration,
    /// The configured endpoint is not one an authorization token may be sent
    /// to: plaintext HTTP outside loopback/link-local, or a URL the HTTP
    /// boundary refuses.
    UnsafeEndpoint,
}

impl AwsRejection {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::IncompleteConfiguration => "incomplete-configuration",
            Self::UnsafeEndpoint => "unsafe-endpoint",
        }
    }

    /// Safe actionable text.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unauthorized => "AWS rejected the credential",
            Self::IncompleteConfiguration => {
                "the configured credential source is missing a required setting"
            }
            Self::UnsafeEndpoint => {
                "the configured container credential endpoint is not a safe destination"
            }
        }
    }
}

/// Status of one AWS credential path.
///
/// The four arms are distinct outcomes and none widens into another.
/// `Undetermined` in particular is never promoted: "the check could not
/// conclude" is neither proof a credential works nor proof it does not, and
/// neither of those is "nothing is configured".
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AwsCredentialStatus {
    /// No credential source this path can inspect is configured. Not a
    /// failure — and, for the chain, not a claim that the EC2 instance
    /// metadata service holds nothing, because this crate does not probe it.
    Absent,
    /// Credential material was discovered, but nothing proved it either way.
    Undetermined {
        /// Where the discovered material lives, or `None` when discovery
        /// itself could not complete and even the location is unknown.
        source: Option<AwsCredentialSource>,
        /// Why no verdict was reached.
        reason: AwsUndetermined,
    },
    /// A live check proved AWS accepts the credential.
    Valid {
        /// Where the accepted credential lives.
        source: AwsCredentialSource,
        /// Unix epoch milliseconds of the check.
        checked_at_ms: u64,
    },
    /// A check proved the credential cannot be used as configured.
    Rejected {
        /// Where the rejected credential lives.
        source: AwsCredentialSource,
        /// Why it is unusable.
        reason: AwsRejection,
    },
}

impl AwsCredentialStatus {
    /// True only for an explicitly proven credential.
    ///
    /// `Undetermined` answers `false` here and `false` from
    /// [`Self::is_rejected`]. A caller that must distinguish the two matches
    /// the enum rather than negating one predicate into the other.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// True only for an explicitly disproven credential.
    #[must_use]
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected { .. })
    }

    /// Where the credential lives, when any material was discovered.
    #[must_use]
    pub const fn source(&self) -> Option<&AwsCredentialSource> {
        match self {
            Self::Undetermined { source, .. } => source.as_ref(),
            Self::Valid { source, .. } | Self::Rejected { source, .. } => Some(source),
            Self::Absent => None,
        }
    }

    /// Stable machine code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Undetermined { .. } => "undetermined",
            Self::Valid { .. } => "valid",
            Self::Rejected { .. } => "rejected",
        }
    }
}

/// Complete safe AWS authentication report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AwsAuthReport {
    /// Effective profile and why.
    pub profile: AwsProfileResolution,
    /// Effective region and why.
    pub region: AwsRegionResolution,
    /// Explicit Amazon Bedrock API key path.
    pub api_key: AwsCredentialStatus,
    /// AWS SDK credential-chain path.
    pub chain: AwsCredentialStatus,
}
