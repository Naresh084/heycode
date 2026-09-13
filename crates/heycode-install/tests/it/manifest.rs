//! Q14 release-manifest admission: pin first, whole generation, no echo.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_install::{
    ArtifactDigest, RELEASE_MANIFEST_SCHEMA_VERSION, ReleaseManifest, ReleasePlatform,
    ReleaseTrustPolicy, SignatureScheme, UpdateError,
};

use super::support::{PLATFORM, admit, artifact_bytes, manifest_json, platform, version};

fn parse(document: &str) -> Result<ReleaseManifest, UpdateError> {
    let pinned = ArtifactDigest::of_bytes(document.as_bytes());
    ReleaseManifest::parse_pinned(&pinned, document.as_bytes())
}

fn artifact_row(platform: &str, digest: &ArtifactDigest) -> String {
    let bundle_digest = ArtifactDigest::of_bytes(b"fixture-bundle");
    format!(
        r#"{{"platform":"{platform}","digest":"{digest}","attestation":{{"scheme":"github_sigstore_bundle_v1","bundle_digest":"{bundle_digest}"}}}}"#
    )
}

#[test]
fn manifest_bytes_that_miss_the_pinned_digest_are_refused_before_the_document_is_parsed() {
    let expected = manifest_json("1.0.0", 18, &artifact_bytes("1.0.0"));
    let pinned = ArtifactDigest::of_bytes(expected.as_bytes());
    // Structurally invalid as well as unpinned: the digest guard must be the
    // one that fires, or nothing proves the document went unparsed.
    let served = b"this is not JSON {{{";

    let error = ReleaseManifest::parse_pinned(&pinned, served).unwrap_err();
    assert_eq!(
        error,
        UpdateError::ManifestDigestMismatch {
            pinned,
            found: ArtifactDigest::of_bytes(served),
        }
    );
}

#[test]
fn an_oversize_manifest_is_refused_on_size_rather_than_on_its_digest() {
    let pinned = ArtifactDigest::of_bytes(b"unrelated");
    let served = vec![b' '; 256 * 1024 + 1];

    assert_eq!(
        ReleaseManifest::parse_pinned(&pinned, &served).unwrap_err(),
        UpdateError::ManifestTooLarge { limit: 256 * 1024 }
    );
}

#[test]
fn an_admitted_manifest_reports_the_release_and_the_document_it_came_from() {
    let bytes = artifact_bytes("1.0.0");
    let document = manifest_json("1.0.0", 18, &bytes);
    let manifest = admit(&document);

    assert_eq!(manifest.schema_version(), RELEASE_MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest.version(), &version("1.0.0"));
    assert_eq!(manifest.config_schema_version(), 18);
    assert_eq!(
        manifest.digest(),
        &ArtifactDigest::of_bytes(document.as_bytes())
    );
    assert_eq!(
        manifest.artifact_for(&platform()).unwrap().digest(),
        &ArtifactDigest::of_bytes(&bytes)
    );
}

#[test]
fn one_malformed_artifact_row_rejects_the_whole_manifest() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let first = artifact_row(PLATFORM, &digest);
    let malformed = artifact_row("Not A Platform", &digest);
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{first},{malformed}]}}"#
    );

    assert_eq!(
        parse(&document).unwrap_err(),
        UpdateError::InvalidArtifact {
            artifact: 1,
            field: "platform",
            reason: "must be lowercase alphanumeric segments joined by hyphens",
        },
        "a partially admitted manifest would let a publisher suppress a platform"
    );
}

#[test]
fn the_same_platform_offered_twice_rejects_the_manifest() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let other = ArtifactDigest::of_bytes(b"y");
    let first = artifact_row(PLATFORM, &digest);
    let second = artifact_row(PLATFORM, &other);
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{first},{second}]}}"#
    );

    assert_eq!(
        parse(&document).unwrap_err(),
        UpdateError::InvalidArtifact {
            artifact: 1,
            field: "platform",
            reason: "an earlier row already offers this platform",
        },
        "two digests for one platform means the pin has no single meaning"
    );
}

#[test]
fn an_unsupported_manifest_schema_is_refused_rather_than_read_partially() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let row = artifact_row(PLATFORM, &digest);
    let document = format!(
        r#"{{"schema_version":3,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{row}]}}"#
    );

    assert_eq!(
        parse(&document).unwrap_err(),
        UpdateError::UnsupportedManifestSchema {
            found: 3,
            supported: RELEASE_MANIFEST_SCHEMA_VERSION,
        }
    );
}

/// A release offering no artifact at all cannot be installed anywhere, and
/// admitting it would defer the failure to install time.
#[test]
fn a_release_offering_no_artifact_is_refused() {
    let document = r#"{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[]}"#;

    assert_eq!(
        parse(document).unwrap_err(),
        UpdateError::InvalidField {
            field: "artifacts",
            reason: "a release must offer at least one artifact",
        }
    );
}

/// There is no nearest-match: a release that does not build for this host
/// fails rather than handing back somebody else's binary.
#[test]
fn an_artifact_for_another_platform_is_not_substituted_for_this_host() {
    let manifest = admit(&manifest_json("1.0.0", 18, &artifact_bytes("1.0.0")));
    let other = ReleasePlatform::new("otheros-otherarch").unwrap();

    assert_eq!(
        manifest.artifact_for(&other).unwrap_err(),
        UpdateError::NoArtifactForPlatform {
            version: version("1.0.0"),
            platform: "otheros-otherarch".to_owned(),
        }
    );
}

#[test]
fn an_artifact_records_the_required_bundle_scheme_and_digest() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let row = artifact_row(PLATFORM, &digest);
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{row}]}}"#
    );

    let manifest = parse(&document).unwrap();
    assert_eq!(
        manifest.artifacts()[0].attestation().scheme(),
        SignatureScheme::GithubSigstoreBundleV1
    );
    assert_eq!(
        manifest.artifacts()[0].attestation().bundle_digest(),
        &ArtifactDigest::of_bytes(b"fixture-bundle")
    );
}

#[test]
fn an_artifact_without_an_attestation_is_rejected() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{{"platform":"{PLATFORM}","digest":"{digest}"}}]}}"#
    );
    assert_eq!(parse(&document).unwrap_err(), UpdateError::InvalidDocument);
}

#[test]
fn a_rejected_manifest_never_echoes_its_own_bytes_into_the_failure() {
    const CANARY: &str = "canary-7b2e4d-do-not-echo";
    let digest = ArtifactDigest::of_bytes(b"x");
    let bundle = ArtifactDigest::of_bytes(b"fixture-bundle");
    let document = format!(
        r#"{{"schema_version":2,"version":"not.a.version","config_schema_version":18,"plugin_api_version":1,"artifacts":[{{"platform":"{PLATFORM}","digest":"{digest}","attestation":{{"scheme":"github_sigstore_bundle_v1","bundle_digest":"{bundle}"}},"unknown":"{CANARY}"}}]}}"#
    );
    assert!(
        document.contains(CANARY),
        "the fixture must actually carry the canary or this test proves nothing"
    );

    let error = parse(&document).unwrap_err();
    assert!(!format!("{error}").contains(CANARY));
    assert!(!format!("{error:?}").contains(CANARY));
}

#[test]
fn a_malformed_release_version_rejects_the_manifest() {
    let digest = ArtifactDigest::of_bytes(b"x");
    let row = artifact_row(PLATFORM, &digest);
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{row}]}}"#
    );

    assert_eq!(
        parse(&document).unwrap_err(),
        UpdateError::InvalidField {
            field: "version",
            reason: "must be major.minor.patch with an optional bounded pre-release",
        }
    );
}

/// A version names a directory under the install root, so a version that is
/// not a single portable path component is a traversal waiting to happen.
#[test]
fn a_version_that_could_escape_the_versions_directory_is_refused() {
    for escape in ["../../etc", "1.0.0/../../etc", "..", "1.0.0/x"] {
        assert!(
            heycode_install::ReleaseVersion::parse(escape).is_err(),
            "`{escape}` must not be accepted as a version"
        );
    }
}

#[test]
fn a_trust_policy_refuses_a_workflow_outside_the_workflow_directory() {
    assert_eq!(
        ReleaseTrustPolicy::github("owner/repository", "../release.yml").unwrap_err(),
        UpdateError::InvalidField {
            field: "trust.workflow",
            reason: "must be one normalized workflow path under .github/workflows",
        }
    );
}

#[test]
fn a_malformed_artifact_digest_rejects_the_manifest() {
    let bundle = ArtifactDigest::of_bytes(b"fixture-bundle");
    let document = format!(
        r#"{{"schema_version":2,"version":"1.0.0","config_schema_version":18,"plugin_api_version":1,"artifacts":[{{"platform":"{PLATFORM}","digest":"sha256:nothex","attestation":{{"scheme":"github_sigstore_bundle_v1","bundle_digest":"{bundle}"}}}}]}}"#
    );

    assert_eq!(
        parse(&document).unwrap_err(),
        UpdateError::InvalidArtifact {
            artifact: 0,
            field: "digest",
            reason: "must be sha256 followed by 64 lowercase hexadecimal digits",
        }
    );
}
