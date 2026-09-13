//! Strict private TOML wire structures for manifest schema v1.

use serde::Deserialize;

use crate::{
    Architecture, AuthenticationPolicy, ContributionKind, OperatingSystem, PluginCodeRuntime,
    PluginPermission, PluginSourceKind, SignatureAlgorithm, UpdateChannel,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireManifest {
    pub(crate) schema_version: u32,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) description: String,
    pub(crate) license: String,
    pub(crate) api: WireApiRange,
    pub(crate) contributions: Vec<WireContribution>,
    pub(crate) configuration_schema: Option<String>,
    pub(crate) requested_permissions: Vec<PluginPermission>,
    pub(crate) default_enabled: bool,
    pub(crate) platforms: Vec<WirePlatform>,
    pub(crate) source: WireSource,
    pub(crate) dependencies: Vec<WireDependency>,
    pub(crate) conflicts: Vec<String>,
    pub(crate) authentication: WireAuthentication,
    #[serde(default)]
    pub(crate) code: Option<WireCode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireCode {
    pub(crate) runtime: PluginCodeRuntime,
    pub(crate) entrypoint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireApiRange {
    pub(crate) minimum: u32,
    pub(crate) maximum: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WirePlatform {
    pub(crate) os: OperatingSystem,
    pub(crate) architecture: Architecture,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireDependency {
    pub(crate) id: String,
    pub(crate) minimum_version: String,
    pub(crate) maximum_version_exclusive: Option<String>,
    pub(crate) optional: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireCredential {
    pub(crate) reference: String,
    pub(crate) kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireAuthentication {
    pub(crate) policy: AuthenticationPolicy,
    pub(crate) credentials: Vec<WireCredential>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireSignature {
    pub(crate) algorithm: SignatureAlgorithm,
    pub(crate) key_id: String,
    pub(crate) value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireSource {
    pub(crate) kind: PluginSourceKind,
    pub(crate) locator: String,
    pub(crate) revision: Option<String>,
    pub(crate) checksum: Option<String>,
    pub(crate) signature: Option<WireSignature>,
    pub(crate) update_channel: UpdateChannel,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WireExposure {
    Namespaced,
    Override { name: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WireContribution {
    pub(crate) kind: ContributionKind,
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) exposure: WireExposure,
}
