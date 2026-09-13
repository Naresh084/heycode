//! Shared fixtures: a pinned manifest and the artifact bytes it describes.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_install::{
    ArtifactDigest, AttestedReleaseManifest, PluginCompatibilitySet, ReleaseChannel,
    ReleaseManifest, ReleasePlatform, ReleasePolicy, ReleasePolicyVerdict,
    ReleaseSignatureVerifier, ReleaseTrustPolicy, ReleaseVersion, SignatureVerificationFault,
    SignatureVerificationRequest, VerifiedReleaseArtifact,
};

pub const PLATFORM: &str = "testos-testarch";
pub const TEST_REPOSITORY: &str = "heycode-fixtures/release-signing";
pub const TEST_WORKFLOW: &str = ".github/workflows/fixture-release.yml";
pub const TEST_ISSUER: &str = "https://token.actions.githubusercontent.com";
const TEST_SIGNING_SEED: &[u8] = b"dshx deterministic release fixture identity v1";

pub fn platform() -> ReleasePlatform {
    ReleasePlatform::new(PLATFORM).unwrap()
}

pub fn version(value: &str) -> ReleaseVersion {
    ReleaseVersion::parse(value).unwrap()
}

/// The exact bytes of one platform artifact.
pub fn artifact_bytes(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho heycode {version}\n").into_bytes()
}

/// Deterministic stand-in for an externally verified Sigstore bundle.
///
/// The seed is test-only and deliberately has no production API. Production
/// supplies a verifier for the GitHub/Sigstore bundle and holds no signing
/// secret in heycode.
pub fn attestation_bundle(subject: &[u8]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(TEST_SIGNING_SEED.len() + subject.len());
    signed.extend_from_slice(TEST_SIGNING_SEED);
    signed.extend_from_slice(subject);
    format!(
        "fixture-attestation-v1:{}:{TEST_REPOSITORY}:{TEST_WORKFLOW}",
        ArtifactDigest::of_bytes(&signed)
    )
    .into_bytes()
}

/// Schema-v2 manifest requiring one exact artifact attestation bundle.
pub fn attested_manifest_json(
    version: &str,
    config_schema_version: u32,
    plugin_api_version: u32,
    bytes: &[u8],
    bundle_digest: &ArtifactDigest,
) -> String {
    format!(
        r#"{{"schema_version":2,"version":"{version}","config_schema_version":{config_schema_version},"plugin_api_version":{plugin_api_version},"artifacts":[{{"platform":"{PLATFORM}","digest":"{}","attestation":{{"scheme":"github_sigstore_bundle_v1","bundle_digest":"{bundle_digest}"}}}}]}}"#,
        ArtifactDigest::of_bytes(bytes)
    )
}

/// A manifest document for one release offering exactly this platform.
pub fn manifest_json(version: &str, config_schema_version: u32, bytes: &[u8]) -> String {
    let bundle = attestation_bundle(bytes);
    attested_manifest_json(
        version,
        config_schema_version,
        1,
        bytes,
        &ArtifactDigest::of_bytes(&bundle),
    )
}

/// Admit a manifest through the pin the caller holds, as production does.
pub fn admit(document: &str) -> ReleaseManifest {
    let pinned = ArtifactDigest::of_bytes(document.as_bytes());
    ReleaseManifest::parse_pinned(&pinned, document.as_bytes()).unwrap()
}

pub struct FixtureVerifier;

impl ReleaseSignatureVerifier for FixtureVerifier {
    fn verify(
        &self,
        request: SignatureVerificationRequest<'_>,
    ) -> Result<(), SignatureVerificationFault> {
        if request.expected_repository() == TEST_REPOSITORY
            && request.expected_workflow() == TEST_WORKFLOW
            && request.expected_issuer() == TEST_ISSUER
            && request.bundle() == attestation_bundle(request.subject_bytes())
        {
            Ok(())
        } else {
            Err(SignatureVerificationFault::Invalid)
        }
    }
}

pub fn trust() -> ReleaseTrustPolicy {
    ReleaseTrustPolicy::github(TEST_REPOSITORY, TEST_WORKFLOW).unwrap()
}

pub fn attested_release(
    version: &str,
    config_schema_version: u32,
    plugin_api_version: u32,
) -> (AttestedReleaseManifest, Vec<u8>, Vec<u8>) {
    let bytes = artifact_bytes(version);
    let artifact_bundle = attestation_bundle(&bytes);
    let document = attested_manifest_json(
        version,
        config_schema_version,
        plugin_api_version,
        &bytes,
        &ArtifactDigest::of_bytes(&artifact_bundle),
    );
    let manifest_bundle = attestation_bundle(document.as_bytes());
    let manifest = ReleaseManifest::parse_attested(
        document.as_bytes(),
        &manifest_bundle,
        &trust(),
        &FixtureVerifier,
    )
    .unwrap();
    (manifest, bytes, artifact_bundle)
}

pub fn verified_release(
    version: &str,
    config_schema_version: u32,
    plugin_api_version: u32,
) -> (VerifiedReleaseArtifact, Vec<u8>) {
    let (manifest, bytes, artifact_bundle) =
        attested_release(version, config_schema_version, plugin_api_version);
    let verified = manifest
        .verify_artifact(
            &platform(),
            bytes.clone(),
            &artifact_bundle,
            &FixtureVerifier,
        )
        .unwrap();
    (verified, bytes)
}

pub fn approved_update(
    current: &str,
    candidate: &str,
    config_schema_version: u32,
    plugin_api_version: u32,
) -> (VerifiedReleaseArtifact, Vec<u8>) {
    let (manifest, bytes, bundle) =
        attested_release(candidate, config_schema_version, plugin_api_version);
    let policy = ReleasePolicy::new(ReleaseChannel::Preview, PluginCompatibilitySet::empty());
    let verdict = policy.evaluate(&version(current), &manifest);
    let ReleasePolicyVerdict::Approved(approved) = verdict else {
        panic!("the fixture update must be policy-approved");
    };
    let artifact = approved
        .verify_artifact(&platform(), bytes.clone(), &bundle, &FixtureVerifier)
        .unwrap();
    (artifact, bytes)
}
