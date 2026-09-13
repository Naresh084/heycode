//! Whole-generation admission of one pinned release manifest.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::error::{invalid_artifact, invalid_field};
use crate::signature::verify_manifest_signature;
use crate::{
    ArtifactAttestation, ArtifactDigest, AttestedReleaseManifest, RELEASE_MANIFEST_SCHEMA_VERSION,
    ReleaseArtifact, ReleaseManifest, ReleasePlatform, ReleaseSignatureVerifier,
    ReleaseTrustPolicy, ReleaseVersion, SignatureScheme, UpdateError,
};

pub(crate) const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_ARTIFACTS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifest {
    schema_version: u32,
    version: String,
    config_schema_version: u32,
    plugin_api_version: u32,
    artifacts: Vec<WireArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireArtifact {
    platform: String,
    digest: String,
    attestation: WireAttestation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAttestation {
    scheme: WireSignatureScheme,
    bundle_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireSignatureScheme {
    GithubSigstoreBundleV1,
}

impl ReleaseManifest {
    /// Verify the pinned manifest digest over exact fetched bytes, then admit
    /// the complete generation.
    ///
    /// Order is load-bearing. The byte budget is applied first, the pinned
    /// digest second, and the document is decoded only after both hold, so an
    /// unpinned or oversized manifest is refused on that ground rather than
    /// reaching the parser.
    ///
    /// # Errors
    /// Oversized bytes, a digest differing from the caller's pin, a malformed
    /// or unknown-field document, an unsupported schema, too many rows, or any
    /// invalid row. One bad row rejects the whole generation.
    pub fn parse_pinned(pinned: &ArtifactDigest, raw: &[u8]) -> Result<Self, UpdateError> {
        if raw.len() > MAX_MANIFEST_BYTES {
            return Err(UpdateError::ManifestTooLarge {
                limit: MAX_MANIFEST_BYTES,
            });
        }
        let digest = ArtifactDigest::of_bytes(raw);
        if &digest != pinned {
            return Err(UpdateError::ManifestDigestMismatch {
                pinned: pinned.clone(),
                found: digest,
            });
        }
        parse_document(raw, digest)
    }

    /// Authenticate exact manifest bytes through a locally configured signer
    /// policy before parsing any remotely supplied field.
    ///
    /// # Errors
    /// Oversized input, signature verification failure, or any manifest
    /// admission failure. Signature verification precedes JSON decoding.
    pub fn parse_attested(
        raw: &[u8],
        bundle: &[u8],
        trust: &ReleaseTrustPolicy,
        verifier: &dyn ReleaseSignatureVerifier,
    ) -> Result<AttestedReleaseManifest, UpdateError> {
        if raw.len() > MAX_MANIFEST_BYTES {
            return Err(UpdateError::ManifestTooLarge {
                limit: MAX_MANIFEST_BYTES,
            });
        }
        let signature = verify_manifest_signature(raw, bundle, trust, verifier)?;
        let manifest = parse_document(raw, ArtifactDigest::of_bytes(raw))?;
        Ok(AttestedReleaseManifest::new(
            manifest,
            trust.clone(),
            signature,
            raw.to_vec(),
            bundle.to_vec(),
        ))
    }
}

fn parse_document(raw: &[u8], digest: ArtifactDigest) -> Result<ReleaseManifest, UpdateError> {
    let wire =
        serde_json::from_slice::<WireManifest>(raw).map_err(|_| UpdateError::InvalidDocument)?;
    if wire.schema_version != RELEASE_MANIFEST_SCHEMA_VERSION {
        return Err(UpdateError::UnsupportedManifestSchema {
            found: wire.schema_version,
            supported: RELEASE_MANIFEST_SCHEMA_VERSION,
        });
    }
    let version = ReleaseVersion::parse(wire.version)?;
    if wire.config_schema_version == 0 {
        return Err(invalid_field(
            "config_schema_version",
            "must be greater than zero",
        ));
    }
    if wire.plugin_api_version == 0 {
        return Err(invalid_field(
            "plugin_api_version",
            "must be greater than zero",
        ));
    }
    if wire.artifacts.len() > MAX_ARTIFACTS {
        return Err(invalid_field(
            "artifacts",
            "exceeds the fixed per-release artifact budget",
        ));
    }
    if wire.artifacts.is_empty() {
        return Err(invalid_field(
            "artifacts",
            "a release must offer at least one artifact",
        ));
    }
    let mut artifacts = Vec::with_capacity(wire.artifacts.len());
    let mut seen = BTreeSet::new();
    for (index, artifact) in wire.artifacts.into_iter().enumerate() {
        let admitted = validate_artifact(index, artifact)?;
        if !seen.insert(admitted.platform().clone()) {
            return Err(invalid_artifact(
                index,
                "platform",
                "an earlier row already offers this platform",
            ));
        }
        artifacts.push(admitted);
    }
    artifacts.sort_by(|left, right| left.platform().cmp(right.platform()));
    Ok(ReleaseManifest {
        schema_version: RELEASE_MANIFEST_SCHEMA_VERSION,
        version,
        config_schema_version: wire.config_schema_version,
        plugin_api_version: wire.plugin_api_version,
        digest,
        artifacts,
    })
}

fn validate_artifact(index: usize, artifact: WireArtifact) -> Result<ReleaseArtifact, UpdateError> {
    let platform = ReleasePlatform::new(artifact.platform).map_err(|_| {
        invalid_artifact(
            index,
            "platform",
            "must be lowercase alphanumeric segments joined by hyphens",
        )
    })?;
    let digest = ArtifactDigest::parse(&artifact.digest).map_err(|_| {
        invalid_artifact(
            index,
            "digest",
            "must be sha256 followed by 64 lowercase hexadecimal digits",
        )
    })?;
    let scheme = match artifact.attestation.scheme {
        WireSignatureScheme::GithubSigstoreBundleV1 => SignatureScheme::GithubSigstoreBundleV1,
    };
    let bundle_digest =
        ArtifactDigest::parse(&artifact.attestation.bundle_digest).map_err(|_| {
            invalid_artifact(
                index,
                "attestation.bundle_digest",
                "must be sha256 followed by 64 lowercase hexadecimal digits",
            )
        })?;
    Ok(ReleaseArtifact {
        platform,
        digest,
        attestation: ArtifactAttestation {
            scheme,
            bundle_digest,
        },
    })
}
