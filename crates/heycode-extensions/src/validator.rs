//! Atomic manifest parsing, compatibility checks, and collision validation.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::error::invalid;
use crate::model::{valid_kebab, valid_registry_name, valid_relative_path};
use crate::wire::{
    WireAuthentication, WireContribution, WireDependency, WireExposure, WireManifest, WireSource,
};
use crate::{
    ApiVersion, AuthenticationPolicy, ContributionExposure, ContributionKey, CredentialRequirement,
    ManifestError, PLUGIN_MANIFEST_SCHEMA_VERSION, PlatformTarget, PluginApiRange,
    PluginAuthentication, PluginCode, PluginContribution, PluginDependency, PluginId,
    PluginManifest, PluginPath, PluginPermission, PluginSignature, PluginSource, PluginSourceKind,
    PluginVersion, UpdateChannel,
};

const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Strict validator configured for one exact host and contribution registry.
#[derive(Debug, Clone)]
pub struct ManifestValidator {
    host_api: ApiVersion,
    host_platform: PlatformTarget,
    occupied: BTreeSet<ContributionKey>,
    allowed_overrides: BTreeSet<ContributionKey>,
}

impl ManifestValidator {
    /// Create a validator for one exact host API and platform.
    #[must_use]
    pub fn new(host_api: ApiVersion, host_platform: PlatformTarget) -> Self {
        Self {
            host_api,
            host_platform,
            occupied: BTreeSet::new(),
            allowed_overrides: BTreeSet::new(),
        }
    }

    /// Seed exact contribution rows already owned by the host or active
    /// plugins. A namespaced claimant colliding with one of these rows fails.
    #[must_use]
    pub fn with_occupied(
        mut self,
        contributions: impl IntoIterator<Item = ContributionKey>,
    ) -> Self {
        self.occupied.extend(contributions);
        self
    }

    /// Admit exact override targets supported by their owning registries.
    /// Permission declaration remains mandatory in every manifest.
    #[must_use]
    pub fn with_allowed_overrides(
        mut self,
        contributions: impl IntoIterator<Item = ContributionKey>,
    ) -> Self {
        self.allowed_overrides.extend(contributions);
        self
    }

    /// Parse and validate one manifest against this host snapshot.
    ///
    /// # Errors
    /// Malformed/unknown fields, incompatible API/platform, invalid ids,
    /// versions, paths, source/auth metadata, undeclared permissions, and
    /// contribution collisions fail before a manifest is returned.
    pub fn validate_toml(&self, raw: &str) -> Result<PluginManifest, ManifestError> {
        let mut manifests = self.validate_batch_toml(&[raw])?;
        manifests.pop().ok_or(ManifestError::InvalidDocument)
    }

    /// Validate a manifest generation atomically. Collision and duplicate-id
    /// checks cover the complete candidate batch plus occupied host rows.
    ///
    /// # Errors
    /// Any invalid candidate rejects the whole returned generation.
    pub fn validate_batch_toml(
        &self,
        documents: &[&str],
    ) -> Result<Vec<PluginManifest>, ManifestError> {
        let mut manifests = Vec::with_capacity(documents.len());
        let mut plugin_ids = BTreeSet::new();
        let mut claimed = BTreeSet::new();
        for raw in documents {
            let manifest = self.parse_manifest(raw)?;
            if !plugin_ids.insert(manifest.id().clone()) {
                return Err(ManifestError::DuplicatePluginId);
            }
            for contribution in manifest.contributions() {
                let key = contribution.key();
                match contribution.exposure() {
                    ContributionExposure::Namespaced => {
                        if self.occupied.contains(&key) || !claimed.insert(key.clone()) {
                            return Err(collision(key));
                        }
                    }
                    ContributionExposure::Override { .. } => {
                        if !self.allowed_overrides.contains(&key) {
                            return Err(ManifestError::OverrideNotAllowed {
                                kind: key.kind(),
                                name: key.name().to_owned(),
                            });
                        }
                        if !claimed.insert(key.clone()) {
                            return Err(collision(key));
                        }
                    }
                }
            }
            manifests.push(manifest);
        }
        Ok(manifests)
    }

    fn parse_manifest(&self, raw: &str) -> Result<PluginManifest, ManifestError> {
        if raw.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let probe: toml::Value = toml::from_str(raw).map_err(|_| ManifestError::InvalidDocument)?;
        let found = probe
            .as_table()
            .and_then(|table| table.get("schema_version"))
            .and_then(toml::Value::as_integer)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(ManifestError::InvalidDocument)?;
        if found != PLUGIN_MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                found,
                supported: PLUGIN_MANIFEST_SCHEMA_VERSION,
            });
        }
        let wire: WireManifest = toml::from_str(raw).map_err(|_| ManifestError::InvalidDocument)?;
        self.validate_wire(wire)
    }

    fn validate_wire(&self, wire: WireManifest) -> Result<PluginManifest, ManifestError> {
        let id = PluginId::new(wire.id)?;
        validate_text("name", &wire.name, 128)?;
        validate_text("description", &wire.description, 2_048)?;
        validate_license(&wire.license)?;
        let version = PluginVersion::parse(wire.version)?;

        let minimum = ApiVersion::new(wire.api.minimum)
            .map_err(|_| invalid("api.minimum", "must be greater than zero"))?;
        let maximum = ApiVersion::new(wire.api.maximum)
            .map_err(|_| invalid("api.maximum", "must be greater than zero"))?;
        if minimum > maximum {
            return Err(invalid(
                "api",
                "minimum must be less than or equal to maximum",
            ));
        }
        if self.host_api < minimum || self.host_api > maximum {
            return Err(ManifestError::IncompatibleApi {
                host: self.host_api,
                minimum,
                maximum,
            });
        }
        let api = PluginApiRange { minimum, maximum };

        let permissions = unique_permissions(wire.requested_permissions)?;
        let platforms = unique_platforms(wire.platforms)?;
        if !platforms.contains(&self.host_platform) {
            return Err(ManifestError::UnsupportedPlatform {
                os: self.host_platform.os(),
                architecture: self.host_platform.architecture(),
            });
        }

        let configuration_schema = wire
            .configuration_schema
            .map(|path| {
                let path = PluginPath::new("configuration_schema", path)?;
                if !path.as_str().ends_with(".json") {
                    return Err(invalid(
                        "configuration_schema",
                        "must point to a relative JSON schema",
                    ));
                }
                Ok(path)
            })
            .transpose()?;
        let source = validate_source(wire.source)?;
        let authentication = validate_authentication(wire.authentication, &permissions)?;
        let dependencies = validate_dependencies(&id, wire.dependencies)?;
        let conflicts = validate_conflicts(&id, &dependencies, wire.conflicts)?;
        let contributions = validate_contributions(&id, &permissions, wire.contributions)?;
        let code = wire
            .code
            .map(|code| {
                PluginPath::new("code.entrypoint", code.entrypoint)
                    .map(|entrypoint| PluginCode::new(code.runtime, entrypoint))
            })
            .transpose()?;
        if code.is_some()
            && contributions
                .iter()
                .all(|contribution| contribution.kind() == crate::ContributionKind::Mcp)
        {
            return Err(invalid(
                "code",
                "code plugins require at least one non-MCP contribution",
            ));
        }

        Ok(PluginManifest::new(
            wire.schema_version,
            id,
            wire.name,
            version,
            wire.description,
            wire.license,
            api,
            contributions,
            configuration_schema,
            permissions,
            wire.default_enabled,
            platforms,
            source,
            dependencies,
            conflicts,
            authentication,
            code,
        ))
    }
}

fn collision(key: ContributionKey) -> ManifestError {
    ManifestError::ContributionCollision {
        kind: key.kind(),
        name: key.name().to_owned(),
    }
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(invalid(
            field,
            "must be non-empty, trimmed, control-free, and within its byte limit",
        ));
    }
    Ok(())
}

fn unique_permissions(
    permissions: Vec<PluginPermission>,
) -> Result<Vec<PluginPermission>, ManifestError> {
    let mut seen = BTreeSet::new();
    for permission in &permissions {
        if !seen.insert(*permission) {
            return Err(ManifestError::DuplicateField {
                field: "requested_permissions",
            });
        }
    }
    Ok(permissions)
}

fn unique_platforms(
    platforms: Vec<crate::wire::WirePlatform>,
) -> Result<Vec<PlatformTarget>, ManifestError> {
    if platforms.is_empty() {
        return Err(invalid(
            "platforms",
            "must contain at least one exact target",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(platforms.len());
    for platform in platforms {
        let target = PlatformTarget::new(platform.os, platform.architecture);
        if !seen.insert(target) {
            return Err(ManifestError::DuplicateField { field: "platforms" });
        }
        result.push(target);
    }
    Ok(result)
}

fn validate_authentication(
    wire: WireAuthentication,
    permissions: &[PluginPermission],
) -> Result<PluginAuthentication, ManifestError> {
    match wire.policy {
        AuthenticationPolicy::None if !wire.credentials.is_empty() => {
            return Err(invalid(
                "authentication.credentials",
                "must be empty when policy is none",
            ));
        }
        AuthenticationPolicy::Optional | AuthenticationPolicy::Required
            if wire.credentials.is_empty() =>
        {
            return Err(invalid(
                "authentication.credentials",
                "must not be empty when authentication is declared",
            ));
        }
        _ => {}
    }
    if !wire.credentials.is_empty() && !permissions.contains(&PluginPermission::CredentialUse) {
        return Err(ManifestError::MissingPermission {
            permission: PluginPermission::CredentialUse,
            required_by: "authentication",
        });
    }
    let mut seen = BTreeSet::new();
    let mut credential_references = Vec::with_capacity(wire.credentials.len());
    for credential in wire.credentials {
        if !valid_credential_reference(&credential.reference) {
            return Err(invalid(
                "authentication.credentials.reference",
                "must be a stable non-secret credential reference",
            ));
        }
        if !valid_kebab(&credential.kind) {
            return Err(invalid(
                "authentication.credentials.kind",
                "must be lowercase kebab-case",
            ));
        }
        if !seen.insert(credential.reference.clone()) {
            return Err(ManifestError::DuplicateField {
                field: "authentication.credentials.reference",
            });
        }
        credential_references.push(CredentialRequirement {
            reference: credential.reference,
            kind: credential.kind,
        });
    }
    Ok(PluginAuthentication {
        policy: wire.policy,
        credential_references,
    })
}

fn validate_dependencies(
    manifest_id: &PluginId,
    dependencies: Vec<WireDependency>,
) -> Result<Vec<PluginDependency>, ManifestError> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(dependencies.len());
    for dependency in dependencies {
        let id = PluginId::new(dependency.id).map_err(|_| {
            invalid(
                "dependencies.id",
                "must be marketplace/plugin with lowercase kebab-case segments",
            )
        })?;
        if &id == manifest_id {
            return Err(ManifestError::DependencyConflict);
        }
        if !seen.insert(id.clone()) {
            return Err(ManifestError::DuplicateField {
                field: "dependencies.id",
            });
        }
        let minimum_version = PluginVersion::parse(dependency.minimum_version).map_err(|_| {
            invalid(
                "dependencies.minimum_version",
                "must be a valid Semantic Versioning 2.0 value",
            )
        })?;
        let maximum_version_exclusive = dependency
            .maximum_version_exclusive
            .map(|value| {
                PluginVersion::parse(value).map_err(|_| {
                    invalid(
                        "dependencies.maximum_version_exclusive",
                        "must be a valid Semantic Versioning 2.0 value",
                    )
                })
            })
            .transpose()?;
        if maximum_version_exclusive
            .as_ref()
            .is_some_and(|maximum| maximum.precedence_cmp(&minimum_version) != Ordering::Greater)
        {
            return Err(invalid(
                "dependencies",
                "exclusive maximum must be greater than the minimum",
            ));
        }
        result.push(PluginDependency {
            id,
            minimum_version,
            maximum_version_exclusive,
            optional: dependency.optional,
        });
    }
    Ok(result)
}

fn validate_conflicts(
    manifest_id: &PluginId,
    dependencies: &[PluginDependency],
    conflicts: Vec<String>,
) -> Result<Vec<PluginId>, ManifestError> {
    let dependency_ids = dependencies
        .iter()
        .map(|dependency| &dependency.id)
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(conflicts.len());
    for conflict in conflicts {
        let id = PluginId::new(conflict).map_err(|_| {
            invalid(
                "conflicts",
                "must contain marketplace/plugin ids with lowercase kebab-case segments",
            )
        })?;
        if &id == manifest_id || dependency_ids.contains(&id) {
            return Err(ManifestError::DependencyConflict);
        }
        if !seen.insert(id.clone()) {
            return Err(ManifestError::DuplicateField { field: "conflicts" });
        }
        result.push(id);
    }
    Ok(result)
}

fn validate_contributions(
    manifest_id: &PluginId,
    permissions: &[PluginPermission],
    contributions: Vec<WireContribution>,
) -> Result<Vec<PluginContribution>, ManifestError> {
    if contributions.is_empty() {
        return Err(invalid(
            "contributions",
            "must contain at least one declarative contribution",
        ));
    }
    let mut paths = BTreeSet::new();
    let mut local_ids = BTreeSet::new();
    let mut result = Vec::with_capacity(contributions.len());
    for contribution in contributions {
        if !valid_kebab(&contribution.id) {
            return Err(invalid("contributions.id", "must be lowercase kebab-case"));
        }
        if !local_ids.insert((contribution.kind, contribution.id.clone())) {
            return Err(ManifestError::DuplicateField {
                field: "contributions.id",
            });
        }
        let path = PluginPath::new("contributions.path", contribution.path)?;
        if path.as_str().split('/').next() != Some(contribution.kind.directory()) {
            return Err(invalid(
                "contributions.path",
                "must be under the directory owned by its contribution kind",
            ));
        }
        if !paths.insert(path.as_str().to_ascii_lowercase()) {
            return Err(ManifestError::DuplicateField {
                field: "contributions.path",
            });
        }
        let (exposure, public_name) = match contribution.exposure {
            WireExposure::Namespaced => (
                ContributionExposure::Namespaced,
                format!("{}::{}", manifest_id.as_str(), contribution.id),
            ),
            WireExposure::Override { name } => {
                if !permissions.contains(&PluginPermission::ContributionOverride) {
                    return Err(ManifestError::MissingPermission {
                        permission: PluginPermission::ContributionOverride,
                        required_by: "contribution override",
                    });
                }
                if !valid_registry_name(&name) {
                    return Err(invalid(
                        "contributions.exposure.name",
                        "must be a valid public registry name",
                    ));
                }
                (ContributionExposure::Override { name: name.clone() }, name)
            }
        };
        if !valid_registry_name(&public_name) {
            return Err(invalid(
                "contributions.public_name",
                "computed public name exceeds the registry budget",
            ));
        }
        result.push(PluginContribution::new(
            contribution.kind,
            contribution.id,
            path,
            exposure,
            public_name,
        ));
    }
    Ok(result)
}

fn validate_source(wire: WireSource) -> Result<PluginSource, ManifestError> {
    validate_source_locator(wire.kind, &wire.locator)?;
    if let Some(revision) = wire.revision.as_deref() {
        validate_text("source.revision", revision, 256)?;
        if !revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
            || revision
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(invalid(
                "source.revision",
                "contains unsupported characters or traversal",
            ));
        }
    }
    let checksum = wire
        .checksum
        .map(|value| {
            if !valid_sha256(&value) {
                return Err(invalid(
                    "source.checksum",
                    "must be sha256 followed by 64 lowercase hexadecimal digits",
                ));
            }
            Ok(value)
        })
        .transpose()?;
    let signature = wire.signature.map(validate_signature).transpose()?;
    if wire.update_channel == UpdateChannel::Pinned && checksum.is_none() && wire.revision.is_none()
    {
        return Err(invalid(
            "source.update_channel",
            "pinned sources require a checksum or revision",
        ));
    }
    Ok(PluginSource {
        kind: wire.kind,
        locator: wire.locator,
        revision: wire.revision,
        checksum,
        signature,
        update_channel: wire.update_channel,
    })
}

/// Shared with the marketplace catalog boundary: a catalog locator and a
/// manifest locator are the same untrusted shape and must not drift apart.
pub(crate) fn validate_source_locator(
    kind: PluginSourceKind,
    locator: &str,
) -> Result<(), ManifestError> {
    validate_text("source.locator", locator, 2_048)?;
    if locator.contains('?') || locator.contains('#') || locator.chars().any(char::is_whitespace) {
        return Err(invalid(
            "source.locator",
            "query strings and fragments are not allowed in public source metadata",
        ));
    }
    match kind {
        PluginSourceKind::Local => {
            if !valid_relative_path(locator) {
                return Err(invalid(
                    "source.locator",
                    "local source must be a normalized relative path",
                ));
            }
        }
        PluginSourceKind::Https => validate_public_url(locator, "https://")?,
        PluginSourceKind::Git => {
            let public_url = if locator.starts_with("https://") {
                validate_public_url(locator, "https://").is_ok()
            } else if locator.starts_with("ssh://") {
                validate_public_url(locator, "ssh://").is_ok()
            } else {
                false
            };
            let scp_style = locator.strip_prefix("git@").is_some_and(|remainder| {
                remainder
                    .split_once(':')
                    .is_some_and(|(host, path)| valid_url_host(host) && valid_relative_path(path))
            });
            if !public_url && !scp_style {
                return Err(invalid(
                    "source.locator",
                    "git source must be a public HTTPS/SSH or git@ locator without URL credentials",
                ));
            }
        }
        PluginSourceKind::Registry | PluginSourceKind::Marketplace => {
            if !locator.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
            }) || locator
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
            {
                return Err(invalid(
                    "source.locator",
                    "registry locator contains unsupported characters or traversal",
                ));
            }
        }
    }
    Ok(())
}

fn validate_public_url(locator: &str, scheme: &str) -> Result<(), ManifestError> {
    if !locator.starts_with(scheme) || url_authority(locator).is_none_or(invalid_authority) {
        return Err(invalid(
            "source.locator",
            "must be a public URL without embedded credentials",
        ));
    }
    Ok(())
}

fn url_authority(locator: &str) -> Option<&str> {
    let (_, rest) = locator.split_once("://")?;
    Some(rest.split('/').next().unwrap_or(rest))
}

fn invalid_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return true;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((address, suffix)) = rest.split_once(']') else {
            return true;
        };
        return address.parse::<std::net::Ipv6Addr>().is_err()
            || !(suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_url_port));
    }
    let mut parts = authority.split(':');
    let host = parts.next().unwrap_or_default();
    let port = parts.next();
    if parts.next().is_some()
        || host.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return true;
    }
    !valid_url_host(host) || port.is_some_and(|value| !valid_url_port(value))
}

fn valid_url_host(host: &str) -> bool {
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_url_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
}

/// Shared with the marketplace catalog boundary: a signature declared by a
/// catalog gets exactly the manifest's canonical-encoding rules.
pub(crate) fn validate_signature(
    wire: crate::wire::WireSignature,
) -> Result<PluginSignature, ManifestError> {
    if !valid_credential_reference(&wire.key_id) {
        return Err(invalid(
            "source.signature.key_id",
            "must be a stable public verification-key reference",
        ));
    }
    if !valid_ed25519_signature(&wire.value) {
        return Err(invalid(
            "source.signature.value",
            "must be bounded canonical base64",
        ));
    }
    Ok(PluginSignature {
        algorithm: wire.algorithm,
        key_id: wire.key_id,
        value: wire.value,
    })
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_ed25519_signature(value: &str) -> bool {
    if value.len() != 88 || !value.ends_with("==") {
        return false;
    }
    let data = &value[..86];
    data.bytes().all(is_base64_data)
        && data
            .as_bytes()
            .last()
            .and_then(|byte| base64_value(*byte))
            .is_some_and(|value| value & 0x0f == 0)
}

fn is_base64_data(byte: u8) -> bool {
    base64_value(byte).is_some()
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn valid_credential_reference(value: &str) -> bool {
    value.len() <= 256
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LicenseToken<'a> {
    Atom(&'a str),
    And,
    Or,
    With,
    Left,
    Right,
}

fn validate_license(value: &str) -> Result<(), ManifestError> {
    validate_text("license", value, 256)?;
    let tokens = tokenize_license(value)
        .ok_or_else(|| invalid("license", "must be a bounded SPDX-shaped expression"))?;
    let mut cursor = 0;
    if !parse_license_expression(&tokens, &mut cursor, 0) || cursor != tokens.len() {
        return Err(invalid(
            "license",
            "must be a bounded SPDX-shaped expression",
        ));
    }
    Ok(())
}

fn tokenize_license(value: &str) -> Option<Vec<LicenseToken<'_>>> {
    let mut tokens = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index] == b'(' {
            tokens.push(LicenseToken::Left);
            index += 1;
            continue;
        }
        if bytes[index] == b')' {
            tokens.push(LicenseToken::Right);
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && !matches!(bytes[index], b'(' | b')')
        {
            index += 1;
        }
        let token = &value[start..index];
        tokens.push(match token {
            "AND" => LicenseToken::And,
            "OR" => LicenseToken::Or,
            "WITH" => LicenseToken::With,
            _ if token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'+')) =>
            {
                LicenseToken::Atom(token)
            }
            _ => return None,
        });
        if tokens.len() > 128 {
            return None;
        }
    }
    Some(tokens)
}

fn parse_license_expression(tokens: &[LicenseToken<'_>], cursor: &mut usize, depth: usize) -> bool {
    if depth > 16 || !parse_license_term(tokens, cursor, depth) {
        return false;
    }
    while matches!(
        tokens.get(*cursor),
        Some(LicenseToken::And | LicenseToken::Or)
    ) {
        *cursor += 1;
        if !parse_license_term(tokens, cursor, depth) {
            return false;
        }
    }
    true
}

fn parse_license_term(tokens: &[LicenseToken<'_>], cursor: &mut usize, depth: usize) -> bool {
    match tokens.get(*cursor) {
        Some(LicenseToken::Atom(atom)) if !atom.is_empty() => {
            *cursor += 1;
            if matches!(tokens.get(*cursor), Some(LicenseToken::With)) {
                *cursor += 1;
                if !matches!(tokens.get(*cursor), Some(LicenseToken::Atom(_))) {
                    return false;
                }
                *cursor += 1;
            }
            true
        }
        Some(LicenseToken::Left) => {
            *cursor += 1;
            if !parse_license_expression(tokens, cursor, depth + 1)
                || !matches!(tokens.get(*cursor), Some(LicenseToken::Right))
            {
                return false;
            }
            *cursor += 1;
            true
        }
        _ => false,
    }
}
