//! Trusted managed-profile schema for installed code authority.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;

use crate::ConfigError;

const MAX_AUTHORITY_PACKAGES: usize = 128;
const MAX_PREOPENS_PER_PACKAGE: usize = 64;
const MAX_ENDPOINTS_PER_PACKAGE: usize = 64;

/// One administrator-authored installed-code authority generation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCodeAuthorityConfig {
    /// Fingerprint of the exact PL08 source/catalog/host/policy generation.
    pub pl08_generation: String,
    /// Exact enabled package generations authorized to execute code.
    #[serde(default)]
    pub packages: Vec<ManagedCodeAuthorityPackage>,
}

impl ManagedCodeAuthorityConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !valid_sha256(&self.pl08_generation) {
            return authority_error(
                "code authority PL08 generation must be an exact sha256 digest",
            );
        }
        if self.packages.len() > MAX_AUTHORITY_PACKAGES {
            return authority_error("code authority contains too many package rows");
        }
        let mut packages = BTreeSet::new();
        for package in &self.packages {
            package.validate()?;
            if !packages.insert(package.id.as_str()) {
                return authority_error("code authority contains duplicate package identities");
            }
        }
        Ok(())
    }
}

/// One exact installed package generation and its executable authority.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCodeAuthorityPackage {
    /// Marketplace-namespaced package id.
    pub id: String,
    /// Exact Semantic Versioning package version.
    pub version: String,
    /// Exact canonical PL02 package-tree digest.
    pub package_digest: String,
    /// Exact runtime declared by the admitted manifest.
    pub runtime: ManagedCodeRuntime,
    /// Exact package-relative entrypoint declared by the admitted manifest.
    pub entrypoint: String,
    /// Host session issuance policy.
    pub session: ManagedCodeSessionPolicy,
    /// Exact permission grants, never inferred from the manifest request.
    #[serde(default)]
    pub grants: Vec<ManagedCodeGrant>,
    /// Exact WASI filesystem resources. Native rows must leave this empty.
    #[serde(default)]
    pub preopens: Vec<ManagedWasiPreopen>,
    /// Exact WASI outbound IP endpoints. Native rows must leave this empty.
    #[serde(default)]
    pub network_endpoints: Vec<ManagedWasiNetworkEndpoint>,
}

impl ManagedCodeAuthorityPackage {
    fn validate(&self) -> Result<(), ConfigError> {
        if !valid_external_plugin_id(&self.id) {
            return authority_error("code authority package id must be marketplace/plugin");
        }
        if !valid_semver(&self.version) {
            return authority_error("code authority package version must be strict SemVer 2.0");
        }
        if !valid_sha256(&self.package_digest) {
            return authority_error("code authority package digest must be an exact sha256 digest");
        }
        if !valid_relative_plugin_path(&self.entrypoint) {
            return authority_error("code authority entrypoint must be a portable relative path");
        }
        if self.preopens.len() > MAX_PREOPENS_PER_PACKAGE
            || self.network_endpoints.len() > MAX_ENDPOINTS_PER_PACKAGE
        {
            return authority_error("code authority contains too many runtime resources");
        }

        let grants = self.grants.iter().copied().collect::<BTreeSet<_>>();
        if grants.len() != self.grants.len() {
            return authority_error("code authority contains duplicate capability grants");
        }

        match self.runtime {
            ManagedCodeRuntime::NativeProcess => {
                if !self.preopens.is_empty() || !self.network_endpoints.is_empty() {
                    return authority_error("native code authority cannot contain WASI resources");
                }
            }
            ManagedCodeRuntime::WasiComponentV1 => {
                self.validate_wasi(&grants)?;
            }
        }
        Ok(())
    }

    fn validate_wasi(&self, grants: &BTreeSet<ManagedCodeGrant>) -> Result<(), ConfigError> {
        if grants.iter().any(|grant| {
            matches!(
                grant,
                ManagedCodeGrant::ProcessSpawn
                    | ManagedCodeGrant::CredentialUse
                    | ManagedCodeGrant::McpConnect
            )
        }) {
            return authority_error("WASI code authority contains an unsupported capability grant");
        }

        let mut hosts = BTreeSet::new();
        let mut guests = BTreeSet::new();
        for preopen in &self.preopens {
            preopen.validate()?;
            if preopen.access == ManagedWasiPreopenAccess::WriteOnly {
                return authority_error(
                    "WASI write-only preopens are unsupported by the pinned engine",
                );
            }
            if !hosts.insert(preopen.host_path.as_path())
                || !guests.insert(preopen.guest_path.as_str())
            {
                return authority_error("WASI code authority contains a duplicate preopen");
            }
            if preopen.access.can_read() && !grants.contains(&ManagedCodeGrant::FilesystemRead) {
                return authority_error("WASI read preopen requires filesystem_read grant");
            }
            if preopen.access.can_write() && !grants.contains(&ManagedCodeGrant::FilesystemWrite) {
                return authority_error("WASI write preopen requires filesystem_write grant");
            }
        }
        let has_read = self.preopens.iter().any(|row| row.access.can_read());
        let has_write = self.preopens.iter().any(|row| row.access.can_write());
        if grants.contains(&ManagedCodeGrant::FilesystemRead) && !has_read {
            return authority_error(
                "WASI filesystem_read grant requires an exact readable preopen",
            );
        }
        if grants.contains(&ManagedCodeGrant::FilesystemWrite) && !has_write {
            return authority_error(
                "WASI filesystem_write grant requires an exact writable preopen",
            );
        }

        let mut endpoints = BTreeSet::new();
        for endpoint in &self.network_endpoints {
            endpoint.validate()?;
            if !endpoints.insert((endpoint.address.as_str(), endpoint.port)) {
                return authority_error(
                    "WASI code authority contains a duplicate network endpoint",
                );
            }
        }
        if self.network_endpoints.is_empty() == grants.contains(&ManagedCodeGrant::NetworkAccess) {
            return authority_error("WASI network_access grant and exact endpoints must agree");
        }
        Ok(())
    }
}

/// Runtime selected for one managed installed-code generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedCodeRuntime {
    /// Exact-image native child process.
    NativeProcess,
    /// Pinned heycode WIT-v1 Component Model host.
    WasiComponentV1,
}

/// Session issuance policy for one managed package rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedCodeSessionPolicy {
    /// Mint a new unpredictable id for every activation attempt.
    UniquePerActivation,
}

/// Exact capability a managed generation may grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedCodeGrant {
    /// Read from explicitly admitted filesystem resources.
    FilesystemRead,
    /// Write to explicitly admitted filesystem resources.
    FilesystemWrite,
    /// Connect to explicitly admitted network endpoints.
    NetworkAccess,
    /// Spawn through the composed process owner.
    ProcessSpawn,
    /// Resolve a declared credential reference at operation time.
    CredentialUse,
    /// Connect to a declared MCP endpoint.
    McpConnect,
    /// Register a typed event hook.
    HookRegistration,
    /// Override an explicitly override-capable registry row.
    ContributionOverride,
}

/// One lexically exact WASI preopen request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedWasiPreopen {
    /// Absolute host directory; canonical object binding happens during apply.
    pub host_path: PathBuf,
    /// Absolute portable guest path.
    pub guest_path: String,
    /// Exact read/write ceiling.
    pub access: ManagedWasiPreopenAccess,
}

impl ManagedWasiPreopen {
    fn validate(&self) -> Result<(), ConfigError> {
        if !self.host_path.is_absolute()
            || self
                .host_path
                .to_str()
                .is_none_or(|path| path.len() > 4096 || path.chars().any(char::is_control))
            || !valid_guest_path(&self.guest_path)
        {
            return authority_error("WASI preopen paths are invalid");
        }
        Ok(())
    }
}

/// Effective access for one managed WASI preopen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedWasiPreopenAccess {
    /// Read but do not mutate.
    ReadOnly,
    /// Mutate but do not read contents.
    WriteOnly,
    /// Read and mutate.
    ReadWrite,
}

impl ManagedWasiPreopenAccess {
    const fn can_read(self) -> bool {
        matches!(self, Self::ReadOnly | Self::ReadWrite)
    }

    const fn can_write(self) -> bool {
        matches!(self, Self::WriteOnly | Self::ReadWrite)
    }
}

/// One exact managed WASI outbound IP endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedWasiNetworkEndpoint {
    /// Exact IPv4 or IPv6 address. Hostname/DNS authority is unrepresentable.
    pub address: String,
    /// Exact non-zero TCP port.
    pub port: u16,
}

impl ManagedWasiNetworkEndpoint {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 || self.address.parse::<std::net::IpAddr>().is_err() {
            return authority_error(
                "WASI network endpoint must be an IP address and non-zero port",
            );
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_external_plugin_id(value: &str) -> bool {
    let mut segments = value.split('/');
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(namespace), Some(package), None)
            if valid_kebab(namespace) && valid_kebab(package) && value.len() <= 129
    )
}

fn valid_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.contains("--")
}

fn valid_relative_plugin_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.trim() == value
        && !value.starts_with('/')
        && !value.contains('\u{5c}')
        && !value.chars().any(char::is_control)
        && value.split('/').all(valid_portable_component)
}

fn valid_portable_component(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 255
        && !value.ends_with([' ', '.'])
        && !value
            .chars()
            .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
}

fn valid_guest_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && value.starts_with('/')
        && (value.len() == 1 || !value.ends_with('/'))
        && !value.contains("//")
        && !value.contains('\u{5c}')
        && !value.contains(':')
        && !value.chars().any(char::is_control)
        && (value == "/"
            || value
                .split('/')
                .skip(1)
                .all(|component| !component.is_empty() && !matches!(component, "." | "..")))
}

fn valid_semver(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || value.trim() != value {
        return false;
    }
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(core, build)| (core, Some(build)));
    if build.is_some_and(|value| !valid_identifiers(value, false))
        || without_build.matches('+').count() != 0
    {
        return false;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, pre)| (core, Some(pre)));
    let core = core.split('.').collect::<Vec<_>>();
    core.len() == 3
        && core.iter().all(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && (value.len() == 1 || !value.starts_with('0'))
                && value.parse::<u64>().is_ok()
        })
        && prerelease.is_none_or(|value| valid_identifiers(value, true))
}

fn valid_identifiers(value: &str, reject_numeric_leading_zero: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !(reject_numeric_leading_zero
                    && identifier.len() > 1
                    && identifier.bytes().all(|byte| byte.is_ascii_digit())
                    && identifier.starts_with('0'))
        })
}

fn authority_error<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Parse {
        path: "<profile-code-authority>".to_owned(),
        message: message.into(),
    })
}
