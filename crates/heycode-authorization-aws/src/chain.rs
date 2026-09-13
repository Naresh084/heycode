//! Discovery of the AWS SDK credential chain.
//!
//! Discovery reads the documented environment variables and shared
//! configuration files and reports *which* source the chain resolves to. It
//! deliberately performs no network I/O: turning a discovered source into a
//! verdict is a separate step, because "there is a source here" and "that
//! source works" are different claims.

use crate::ini::{self, SharedFile};
use crate::{
    AwsCredentialSource, AwsFileRead, AwsHost, AwsProfileName, AwsProfileResolution, AwsRejection,
    AwsUndetermined,
};

/// Static access key id in the process environment.
pub const AWS_ACCESS_KEY_ID_VAR: &str = "AWS_ACCESS_KEY_ID";
/// Static secret access key in the process environment.
pub const AWS_SECRET_ACCESS_KEY_VAR: &str = "AWS_SECRET_ACCESS_KEY";
/// Web-identity token file in the process environment.
pub const AWS_WEB_IDENTITY_TOKEN_FILE_VAR: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";
/// Role the web-identity token is exchanged for.
pub const AWS_ROLE_ARN_VAR: &str = "AWS_ROLE_ARN";
/// Amazon ECS task-role endpoint, relative to the ECS metadata host.
pub const AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR: &str =
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI";
/// Absolute container credential endpoint, used by Amazon EKS Pod Identity.
pub const AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR: &str = "AWS_CONTAINER_CREDENTIALS_FULL_URI";

/// What the credential chain resolves to, before any live check.
///
/// Discovery is separated from validation so a caller can ask the cheap,
/// offline question on its own — PAWS02/PAWS03 need the *source* and region to
/// build a request, not a verdict.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwsChainDiscovery {
    /// No credential source this crate can inspect is configured.
    ///
    /// The AWS SDKs would still fall through to the EC2 instance metadata
    /// service. This crate does not probe it — IMDSv2 requires a `PUT` the
    /// shared HTTP boundary does not offer, and claiming a source that was
    /// never observed would be a guess, not a report.
    Absent,
    /// Exactly one source is configured. It still needs a check to become a
    /// verdict.
    Found(AwsCredentialSource),
    /// A source is configured but cannot work as written.
    Unusable {
        /// Where the unusable configuration lives.
        source: AwsCredentialSource,
        /// Why it cannot work.
        reason: AwsRejection,
    },
    /// Discovery itself could not complete, so not even the location is known.
    Blocked(AwsUndetermined),
}

/// Walk the credential chain over one host.
///
/// Order: static environment keys, web identity, the effective profile's
/// shared configuration, then the container credential endpoint. SDKs differ
/// in the details of their own ordering; this is a diagnostic over the same
/// documented sources, not a re-implementation of an SDK resolver.
#[must_use]
pub fn discover(host: &dyn AwsHost, profile: &AwsProfileResolution) -> AwsChainDiscovery {
    if let Some(discovery) = environment_keys(host) {
        return discovery;
    }
    if let Some(discovery) = web_identity(host) {
        return discovery;
    }
    if let Some(profile) = profile.profile()
        && let Some(discovery) = shared_configuration(host, profile)
    {
        return discovery;
    }
    if let Some(discovery) = container_role(host) {
        return discovery;
    }
    AwsChainDiscovery::Absent
}

fn environment_keys(host: &dyn AwsHost) -> Option<AwsChainDiscovery> {
    let id = host.var(AWS_ACCESS_KEY_ID_VAR).is_some();
    let secret = host.var(AWS_SECRET_ACCESS_KEY_VAR).is_some();
    let source = AwsCredentialSource::Environment {
        variable: AWS_ACCESS_KEY_ID_VAR,
    };
    match (id, secret) {
        (false, false) => None,
        (true, true) => Some(AwsChainDiscovery::Found(source)),
        // Half a key pair is a determinate misconfiguration, not a reason to
        // pretend the environment is empty and describe a source no request
        // would use.
        _ => Some(AwsChainDiscovery::Unusable {
            source: AwsCredentialSource::Environment {
                variable: if id {
                    AWS_ACCESS_KEY_ID_VAR
                } else {
                    AWS_SECRET_ACCESS_KEY_VAR
                },
            },
            reason: AwsRejection::IncompleteConfiguration,
        }),
    }
}

fn web_identity(host: &dyn AwsHost) -> Option<AwsChainDiscovery> {
    let token = host.var(AWS_WEB_IDENTITY_TOKEN_FILE_VAR).is_some();
    if !token {
        return None;
    }
    let source = AwsCredentialSource::WebIdentityToken {
        variable: AWS_WEB_IDENTITY_TOKEN_FILE_VAR,
    };
    if host.var(AWS_ROLE_ARN_VAR).is_some() {
        Some(AwsChainDiscovery::Found(source))
    } else {
        Some(AwsChainDiscovery::Unusable {
            source,
            reason: AwsRejection::IncompleteConfiguration,
        })
    }
}

fn shared_configuration(host: &dyn AwsHost, profile: &AwsProfileName) -> Option<AwsChainDiscovery> {
    let credentials = match ini::read(host, SharedFile::Credentials) {
        AwsFileRead::Found(text) => {
            ini::section_keys(&text, profile.as_str(), SharedFile::Credentials)
        }
        AwsFileRead::Absent => None,
        AwsFileRead::Unreadable => {
            return Some(AwsChainDiscovery::Blocked(
                AwsUndetermined::ConfigurationUnreadable,
            ));
        }
    };
    if let Some(keys) = credentials
        && let Some(discovery) = access_keys(&keys, profile)
    {
        return Some(discovery);
    }
    let config = match ini::read(host, SharedFile::Config) {
        AwsFileRead::Found(text) => ini::section_keys(&text, profile.as_str(), SharedFile::Config)?,
        AwsFileRead::Absent => return None,
        AwsFileRead::Unreadable => {
            return Some(AwsChainDiscovery::Blocked(
                AwsUndetermined::ConfigurationUnreadable,
            ));
        }
    };
    if config.contains("role_arn") {
        let complete = config.contains("source_profile")
            || config.contains("credential_source")
            || config.contains("web_identity_token_file");
        return Some(assume_role(profile, complete));
    }
    if config.contains("sso_session") {
        return Some(AwsChainDiscovery::Found(AwsCredentialSource::SsoSession {
            profile: profile.clone(),
        }));
    }
    if config.contains("sso_start_url") {
        let complete = config.contains("sso_account_id") && config.contains("sso_role_name");
        return Some(sso(profile, complete));
    }
    if config.contains("credential_process") {
        return Some(AwsChainDiscovery::Found(
            AwsCredentialSource::CredentialProcess {
                profile: profile.clone(),
            },
        ));
    }
    access_keys(&config, profile)
}

fn access_keys(
    keys: &std::collections::BTreeSet<String>,
    profile: &AwsProfileName,
) -> Option<AwsChainDiscovery> {
    if !keys.contains("aws_access_key_id") {
        return None;
    }
    let source = AwsCredentialSource::ProfileAccessKeys {
        profile: profile.clone(),
    };
    Some(if keys.contains("aws_secret_access_key") {
        AwsChainDiscovery::Found(source)
    } else {
        AwsChainDiscovery::Unusable {
            source,
            reason: AwsRejection::IncompleteConfiguration,
        }
    })
}

fn assume_role(profile: &AwsProfileName, complete: bool) -> AwsChainDiscovery {
    let source = AwsCredentialSource::AssumeRole {
        profile: profile.clone(),
    };
    if complete {
        AwsChainDiscovery::Found(source)
    } else {
        AwsChainDiscovery::Unusable {
            source,
            reason: AwsRejection::IncompleteConfiguration,
        }
    }
}

fn sso(profile: &AwsProfileName, complete: bool) -> AwsChainDiscovery {
    let source = AwsCredentialSource::SsoSession {
        profile: profile.clone(),
    };
    if complete {
        AwsChainDiscovery::Found(source)
    } else {
        AwsChainDiscovery::Unusable {
            source,
            reason: AwsRejection::IncompleteConfiguration,
        }
    }
}

fn container_role(host: &dyn AwsHost) -> Option<AwsChainDiscovery> {
    // The absolute endpoint is documented as an alternative that applies only
    // when the relative one is unset.
    for variable in [
        AWS_CONTAINER_CREDENTIALS_RELATIVE_URI_VAR,
        AWS_CONTAINER_CREDENTIALS_FULL_URI_VAR,
    ] {
        if host.var(variable).is_some() {
            return Some(AwsChainDiscovery::Found(
                AwsCredentialSource::ContainerRole { variable },
            ));
        }
    }
    None
}
