//! Hash-only configuration diagnostics, separate from model messages.

use crate::{RequestHeaderSnapshot, SnapshotError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The recorded component responsible for a new configuration revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestConfigurationChange {
    /// First tracked configuration, including upgrades from untracked history.
    Initial,
    /// Rendered system or guidance text changed.
    System,
    /// Ordered native client-tool declarations changed.
    Tools,
    /// Provider, model, protocol, target or credential binding changed.
    Route,
    /// Effective provider request options changed.
    Options,
}

/// Exact request-configuration fingerprints. These are not cache-hit evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestConfigurationSnapshot {
    /// Monotonic tracked configuration revision within this session.
    pub revision: u64,
    /// Fingerprint over the four component fingerprints.
    pub sha256: String,
    /// Exact rendered system bytes.
    pub system_sha256: String,
    /// Tool list in actual request order, with canonical JSON object keys.
    pub tools_sha256: String,
    /// Route and non-secret authentication binding class/reference.
    pub route_sha256: String,
    /// Effective request options, excluding timestamps and usage.
    pub options_sha256: String,
    /// Empty means unchanged since the preceding tracked request.
    pub changed: Vec<RequestConfigurationChange>,
}

impl RequestHeaderSnapshot {
    /// Record configuration identity against the latest durable request header.
    /// Does not edit tools, messages, options or provider cache policy.
    pub fn record_configuration(&mut self, previous: Option<&Self>) -> Result<(), SnapshotError> {
        let mut current = fingerprints(self)?;
        if let Some(previous) = previous.and_then(|header| header.configuration.as_ref()) {
            current.changed.clear();
            for (before, after, component) in [
                (
                    &previous.system_sha256,
                    &current.system_sha256,
                    RequestConfigurationChange::System,
                ),
                (
                    &previous.tools_sha256,
                    &current.tools_sha256,
                    RequestConfigurationChange::Tools,
                ),
                (
                    &previous.route_sha256,
                    &current.route_sha256,
                    RequestConfigurationChange::Route,
                ),
                (
                    &previous.options_sha256,
                    &current.options_sha256,
                    RequestConfigurationChange::Options,
                ),
            ] {
                if before != after {
                    current.changed.push(component);
                }
            }
            current.revision = if current.changed.is_empty() {
                previous.revision
            } else {
                previous
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("configuration revision overflow"))?
            };
        }
        self.configuration = Some(current);
        self.validate()
    }
}

impl RequestHeaderSnapshot {
    pub(crate) fn validate_configuration_after(
        &self,
        previous: Option<&Self>,
    ) -> Result<(), SnapshotError> {
        if self.configuration.is_none() {
            return Ok(());
        }
        let mut expected = self.clone();
        expected.record_configuration(previous)?;
        if self.configuration != expected.configuration {
            return Err(invalid(
                "configuration revision or changed components do not match history",
            ));
        }
        Ok(())
    }
}

impl RequestConfigurationSnapshot {
    pub(crate) fn validate(&self, header: &RequestHeaderSnapshot) -> Result<(), SnapshotError> {
        let expected = fingerprints(header)?;
        let mut unique = Vec::new();
        for change in &self.changed {
            if unique.contains(change) {
                return Err(invalid("duplicate changed component"));
            }
            unique.push(*change);
        }
        if self.revision == 0
            || self.sha256 != expected.sha256
            || self.system_sha256 != expected.system_sha256
            || self.tools_sha256 != expected.tools_sha256
            || self.route_sha256 != expected.route_sha256
            || self.options_sha256 != expected.options_sha256
            || (self.changed.contains(&RequestConfigurationChange::Initial)
                && self.changed.len() != 1)
        {
            return Err(invalid(
                "configuration fingerprint does not match request fields",
            ));
        }
        Ok(())
    }
}

fn fingerprints(
    header: &RequestHeaderSnapshot,
) -> Result<RequestConfigurationSnapshot, SnapshotError> {
    let system_sha256 = header.prompt_sha256.clone();
    let tools_sha256 = json_hash(
        serde_json::to_value(&header.tools).map_err(|_| invalid("tool serialization failed"))?,
    )?;
    let route_sha256 = json_hash(serde_json::json!({
        "provider":header.provider,"model":header.model,"protocol":header.protocol,
        "target":header.target,"authentication":header.authentication,
    }))?;
    let options_sha256 = json_hash(
        serde_json::to_value(&header.options)
            .map_err(|_| invalid("option serialization failed"))?,
    )?;
    let sha256 = json_hash(
        serde_json::json!({"system":system_sha256,"tools":tools_sha256,"route":route_sha256,"options":options_sha256}),
    )?;
    Ok(RequestConfigurationSnapshot {
        revision: 1,
        sha256,
        system_sha256,
        tools_sha256,
        route_sha256,
        options_sha256,
        changed: vec![RequestConfigurationChange::Initial],
    })
}

fn json_hash(mut value: serde_json::Value) -> Result<String, SnapshotError> {
    heycode_core::canonicalize_json(&mut value);
    let bytes =
        serde_json::to_vec(&value).map_err(|_| invalid("configuration serialization failed"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn invalid(message: &str) -> SnapshotError {
    SnapshotError::InvalidField {
        field: "configuration",
        message: message.into(),
    }
}
