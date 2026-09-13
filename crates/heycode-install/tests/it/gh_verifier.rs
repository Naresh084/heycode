//! Production GitHub CLI verifier crosses only the composed subprocess seam.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt as _;

    use heycode_install::{
        GhAttestationVerifier, ReleaseManifest, SignatureSubject, SignatureVerificationFault,
        UpdateError,
    };

    use crate::it::support::{
        TEST_REPOSITORY, TEST_WORKFLOW, artifact_bytes, attestation_bundle, attested_manifest_json,
    };

    #[test]
    fn verifier_uses_exact_offline_identity_flags_and_becomes_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let arguments = temp.path().join("arguments");
        let script = temp.path().join("gh-fixture");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit 0\n",
                arguments.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let verifier = GhAttestationVerifier::new(
            heycode_exec::SubprocessService::local(),
            &script,
            temp.path().join("scratch"),
        )
        .unwrap();
        let artifact = artifact_bytes("1.0.0");
        let artifact_bundle = attestation_bundle(&artifact);
        let document = attested_manifest_json(
            "1.0.0",
            25,
            1,
            &artifact,
            &heycode_install::ArtifactDigest::of_bytes(&artifact_bundle),
        );
        let bundle = attestation_bundle(document.as_bytes());

        let trust = heycode_install::ReleaseTrustPolicy::github_at_ref(
            TEST_REPOSITORY,
            TEST_WORKFLOW,
            "refs/tags/v1.0.0",
        )
        .unwrap();
        ReleaseManifest::parse_attested(document.as_bytes(), &bundle, &trust, &verifier).unwrap();
        let args = std::fs::read_to_string(&arguments).unwrap();
        assert!(args.contains("attestation\nverify\n--help\n"));
        assert!(args.contains("attestation\nverify\n"));
        assert!(args.contains("--repo\nheycode-fixtures/release-signing\n"));
        assert!(args.contains("--bundle\n"));
        assert!(args.contains("--signer-workflow\ngithub.com/heycode-fixtures/release-signing/.github/workflows/fixture-release.yml\n"));
        assert!(args.contains("--cert-oidc-issuer\nhttps://token.actions.githubusercontent.com\n"));
        assert!(args.contains("--source-ref\nrefs/tags/v1.0.0\n"));
        assert!(args.contains("--deny-self-hosted-runners\n"));

        verifier.close();
        assert_eq!(
            ReleaseManifest::parse_attested(document.as_bytes(), &bundle, &trust, &verifier)
                .err()
                .expect("closed verifier must fail"),
            UpdateError::SignatureVerification {
                subject: SignatureSubject::Manifest,
                fault: SignatureVerificationFault::Unavailable,
            }
        );
    }
}
