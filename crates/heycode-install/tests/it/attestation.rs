//! Q14 signature admission: the manifest and artifact are verified before publication.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use heycode_install::{
    ArtifactDigest, ArtifactMismatch, ReleaseManifest, ReleaseSignatureVerifier,
    ReleaseTrustPolicy, SignatureSubject, SignatureVerificationFault, SignatureVerificationRequest,
    UpdateError,
};

use super::support::{
    TEST_ISSUER, TEST_REPOSITORY, TEST_WORKFLOW, artifact_bytes, attestation_bundle,
    attested_manifest_json, platform,
};

struct FixtureVerifier {
    calls: AtomicUsize,
}

impl FixtureVerifier {
    const fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ReleaseSignatureVerifier for FixtureVerifier {
    fn verify(
        &self,
        request: SignatureVerificationRequest<'_>,
    ) -> Result<(), SignatureVerificationFault> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let expected = attestation_bundle(request.subject_bytes());
        if request.expected_repository() != TEST_REPOSITORY
            || request.expected_workflow() != TEST_WORKFLOW
            || request.expected_issuer() != TEST_ISSUER
            || request.bundle() != expected
        {
            return Err(SignatureVerificationFault::Invalid);
        }
        Ok(())
    }
}

fn trust() -> ReleaseTrustPolicy {
    ReleaseTrustPolicy::github(TEST_REPOSITORY, TEST_WORKFLOW).unwrap()
}

#[test]
fn a_manifest_signature_is_checked_before_untrusted_document_bytes_are_parsed() {
    let raw = b"not-json {{ canary-manifest-body";
    let verifier = FixtureVerifier::new();

    let error = ReleaseManifest::parse_attested(raw, b"wrong-bundle", &trust(), &verifier)
        .err()
        .expect("the invalid signature must be refused");

    assert_eq!(
        error,
        UpdateError::SignatureVerification {
            subject: SignatureSubject::Manifest,
            fault: SignatureVerificationFault::Invalid,
        }
    );
    assert_eq!(verifier.calls(), 1);
}

#[test]
fn a_valid_manifest_and_artifact_require_two_exact_signature_verifications() {
    let bytes = artifact_bytes("1.0.0");
    let artifact_bundle = attestation_bundle(&bytes);
    let document = attested_manifest_json(
        "1.0.0",
        24,
        1,
        &bytes,
        &ArtifactDigest::of_bytes(&artifact_bundle),
    );
    let manifest_bundle = attestation_bundle(document.as_bytes());
    let verifier = FixtureVerifier::new();

    let manifest =
        ReleaseManifest::parse_attested(document.as_bytes(), &manifest_bundle, &trust(), &verifier)
            .unwrap();
    let verified = manifest
        .verify_artifact(&platform(), bytes.clone(), &artifact_bundle, &verifier)
        .unwrap();

    assert_eq!(verifier.calls(), 2);
    assert_eq!(verified.bytes(), bytes);
    assert_eq!(verified.signature().repository(), TEST_REPOSITORY);
    assert_eq!(verified.signature().workflow(), TEST_WORKFLOW);
}

#[test]
fn an_artifact_checksum_mismatch_fails_before_the_signature_verifier_runs() {
    let expected = artifact_bytes("1.0.0");
    let expected_bundle = attestation_bundle(&expected);
    let document = attested_manifest_json(
        "1.0.0",
        24,
        1,
        &expected,
        &ArtifactDigest::of_bytes(&expected_bundle),
    );
    let manifest_bundle = attestation_bundle(document.as_bytes());
    let verifier = FixtureVerifier::new();
    let manifest =
        ReleaseManifest::parse_attested(document.as_bytes(), &manifest_bundle, &trust(), &verifier)
            .unwrap();
    assert_eq!(verifier.calls(), 1);

    let substituted = artifact_bytes("1.0.0-substituted");
    let error = manifest
        .verify_artifact(
            &platform(),
            substituted.clone(),
            &attestation_bundle(&substituted),
            &verifier,
        )
        .err()
        .expect("the substituted artifact must be refused");

    let UpdateError::Mismatch(mismatch) = error else {
        panic!("substituted bytes must fail on the checksum");
    };
    assert!(matches!(*mismatch, ArtifactMismatch::Digest { .. }));
    assert_eq!(
        verifier.calls(),
        1,
        "a checksum failure must not reach the signature verifier"
    );
}

#[test]
fn a_substituted_attestation_bundle_fails_before_artifact_signature_verification() {
    let bytes = artifact_bytes("1.0.0");
    let artifact_bundle = attestation_bundle(&bytes);
    let document = attested_manifest_json(
        "1.0.0",
        24,
        1,
        &bytes,
        &ArtifactDigest::of_bytes(&artifact_bundle),
    );
    let manifest_bundle = attestation_bundle(document.as_bytes());
    let verifier = FixtureVerifier::new();
    let manifest =
        ReleaseManifest::parse_attested(document.as_bytes(), &manifest_bundle, &trust(), &verifier)
            .unwrap();

    let substituted = b"different signed bundle";
    let error = manifest
        .verify_artifact(&platform(), bytes, substituted, &verifier)
        .err()
        .expect("the substituted bundle must be refused");

    assert_eq!(
        error,
        UpdateError::SignatureBundleDigestMismatch {
            pinned: ArtifactDigest::of_bytes(&artifact_bundle),
            found: ArtifactDigest::of_bytes(substituted),
        }
    );
    assert_eq!(
        verifier.calls(),
        1,
        "only the manifest verification should have run"
    );
}
