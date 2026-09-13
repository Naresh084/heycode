//! Q14/Q15 release management surface parsing and fail-loud admission.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use heycode_cli::release_cli::{ReleaseCommand, ReleaseOperation, parse};
use heycode_install::ReleaseChannel;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn apply_parses_every_explicit_trust_and_bundle_input() {
    let command = parse(&args(&[
        "apply",
        "--root",
        "/install",
        "--manifest",
        "/bundle/release-manifest.json",
        "--manifest-bundle",
        "/bundle/release-manifest.sigstore.json",
        "--artifact",
        "/bundle/heycode-macos-aarch64",
        "--artifact-bundle",
        "/bundle/macos-aarch64.sigstore.json",
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        "refs/tags/v1.2.3",
        "--channel",
        "stable",
        "--gh",
        "/opt/homebrew/bin/gh",
    ]))
    .unwrap();

    assert_eq!(command.operation(), ReleaseOperation::Apply);
    let ReleaseCommand::Apply(command) = command else {
        panic!("expected apply command");
    };
    assert_eq!(command.install_root.to_str(), Some("/install"));
    assert_eq!(command.repository, "openai/heycode");
    assert!(matches!(command.channel, ReleaseChannel::Stable));
}

#[test]
fn rollback_has_no_artifact_input_and_pinned_channel_is_exact() {
    let command = parse(&args(&[
        "rollback",
        "--root",
        "/install",
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        "refs/tags/v1.2.3",
    ]))
    .unwrap();
    assert_eq!(command.operation(), ReleaseOperation::Rollback);

    let pinned = parse(&args(&[
        "apply",
        "--root",
        "/install",
        "--manifest",
        "/m",
        "--manifest-bundle",
        "/mb",
        "--artifact",
        "/a",
        "--artifact-bundle",
        "/ab",
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        "refs/tags/v1.2.3",
        "--channel",
        "pinned:1.2.3",
    ]))
    .unwrap();
    let ReleaseCommand::Apply(pinned) = pinned else {
        panic!("expected apply command");
    };
    assert!(
        matches!(pinned.channel, ReleaseChannel::Pinned(version) if version.as_str() == "1.2.3")
    );
}

#[test]
fn missing_duplicate_and_relative_security_inputs_fail_before_composition() {
    assert!(parse(&args(&["apply", "--root", "relative"])).is_err());
    assert!(
        parse(&args(&[
            "rollback",
            "--root",
            "/one",
            "--root",
            "/two",
            "--repo",
            "openai/heycode",
            "--workflow",
            ".github/workflows/release.yml",
            "--source-ref",
            "refs/tags/v1.2.3",
        ]))
        .is_err()
    );
    assert!(parse(&args(&["unknown"])).is_err());
}

#[test]
fn evidence_command_requires_absolute_unique_documents() {
    let parsed = parse(&args(&[
        "evidence",
        "/evidence/macos.json",
        "/evidence/linux.json",
        "/evidence/windows.json",
    ]))
    .unwrap();
    assert_eq!(parsed.operation(), ReleaseOperation::Evidence);
    assert!(parse(&args(&["evidence"])).is_err());
    assert!(parse(&args(&["evidence", "relative.json"])).is_err());
    assert!(parse(&args(&["evidence", "/same", "/same"])).is_err());
}

#[test]
fn evidence_command_evaluates_three_content_free_real_provider_runs() {
    let temp = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (platform, run_id) in [
        ("macos-aarch64", 101_u64),
        ("linux-x86_64", 102_u64),
        ("windows-x86_64", 103_u64),
    ] {
        let path = temp.path().join(format!("{platform}.json"));
        std::fs::write(
            &path,
            format!(
                r#"{{"schema_version":1,"platform":"{platform}","source":{{"kind":"hosted_native","run_id":{run_id}}},"checks":["attestation_verified","fresh_install","first_run_ready"],"turn":"real_provider"}}"#
            ),
        )
        .unwrap();
        paths.push(path);
    }
    let mut command_args = vec!["evidence".to_owned()];
    command_args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    let command = parse(&command_args).unwrap();

    assert_eq!(
        heycode_cli::release_cli::run(
            &command,
            temp.path().join("unused-settings"),
            temp.path().join("unused-cache"),
            heycode_config::ConfigVersionState::Current(25),
        )
        .unwrap(),
        "Q16 real-provider onboarding matrix passed"
    );
}

#[cfg(unix)]
#[test]
fn signed_apply_update_and_rollback_cross_the_minimal_plugin_world() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let gh = temp.path().join("gh-fixture");
    std::fs::write(&gh, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let install = temp.path().join("install");
    let settings = temp.path().join("settings.toml");
    let cache = temp.path().join("plugins");

    let first = write_bundle(temp.path(), "1.0.0", "first");
    let first_command = apply_command(&install, &gh, &first);
    assert_eq!(
        heycode_cli::release_cli::run(
            &first_command,
            settings.clone(),
            cache.clone(),
            heycode_config::ConfigVersionState::Current(25),
        )
        .unwrap(),
        "installed heycode 1.0.0"
    );

    let second = write_bundle(temp.path(), "2.0.0", "second");
    let second_command = apply_command(&install, &gh, &second);
    assert_eq!(
        heycode_cli::release_cli::run(
            &second_command,
            settings.clone(),
            cache.clone(),
            heycode_config::ConfigVersionState::Current(25),
        )
        .unwrap(),
        "updated heycode 2.0.0"
    );

    let rollback = parse(&args(&[
        "rollback",
        "--root",
        install.to_str().unwrap(),
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        "refs/tags/v1.0.0",
        "--gh",
        gh.to_str().unwrap(),
    ]))
    .unwrap();
    assert_eq!(
        heycode_cli::release_cli::run(
            &rollback,
            settings,
            cache,
            heycode_config::ConfigVersionState::Current(25),
        )
        .unwrap(),
        "rolled heycode back to 1.0.0"
    );
}

#[cfg(unix)]
#[test]
fn missing_bundle_after_manager_composition_leaves_install_root_absent() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let gh = temp.path().join("gh-fixture");
    std::fs::write(&gh, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let install = temp.path().join("install");
    let command = parse(&args(&[
        "apply",
        "--root",
        install.to_str().unwrap(),
        "--manifest",
        temp.path().join("missing-manifest").to_str().unwrap(),
        "--manifest-bundle",
        temp.path()
            .join("missing-manifest-bundle")
            .to_str()
            .unwrap(),
        "--artifact",
        temp.path().join("missing-artifact").to_str().unwrap(),
        "--artifact-bundle",
        temp.path()
            .join("missing-artifact-bundle")
            .to_str()
            .unwrap(),
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        "refs/tags/v1.0.0",
        "--channel",
        "stable",
        "--gh",
        gh.to_str().unwrap(),
    ]))
    .unwrap();

    assert!(
        heycode_cli::release_cli::run(
            &command,
            temp.path().join("settings.toml"),
            temp.path().join("plugins"),
            heycode_config::ConfigVersionState::Current(25),
        )
        .is_err()
    );
    assert!(!install.exists());
}

#[cfg(unix)]
struct BundlePaths {
    version: String,
    manifest: std::path::PathBuf,
    manifest_bundle: std::path::PathBuf,
    artifact: std::path::PathBuf,
    artifact_bundle: std::path::PathBuf,
}

#[cfg(unix)]
fn write_bundle(root: &std::path::Path, version: &str, label: &str) -> BundlePaths {
    let artifact = root.join(format!("{label}-heycode"));
    let artifact_bundle = root.join(format!("{label}-artifact.sigstore.json"));
    let manifest = root.join(format!("{label}-release-manifest.json"));
    let manifest_bundle = root.join(format!("{label}-manifest.sigstore.json"));
    let artifact_bytes = format!("heycode-{version}").into_bytes();
    let artifact_bundle_bytes = format!("artifact-bundle-{version}").into_bytes();
    let platform = heycode_install::ReleasePlatform::host().unwrap();
    let document = format!(
        r#"{{"schema_version":2,"version":"{version}","config_schema_version":25,"plugin_api_version":1,"artifacts":[{{"platform":"{}","digest":"{}","attestation":{{"scheme":"github_sigstore_bundle_v1","bundle_digest":"{}"}}}}]}}"#,
        platform.as_str(),
        heycode_install::ArtifactDigest::of_bytes(&artifact_bytes),
        heycode_install::ArtifactDigest::of_bytes(&artifact_bundle_bytes),
    );
    std::fs::write(&artifact, artifact_bytes).unwrap();
    std::fs::write(&artifact_bundle, artifact_bundle_bytes).unwrap();
    std::fs::write(&manifest, document).unwrap();
    std::fs::write(&manifest_bundle, format!("manifest-bundle-{version}")).unwrap();
    BundlePaths {
        version: version.to_owned(),
        manifest,
        manifest_bundle,
        artifact,
        artifact_bundle,
    }
}

#[cfg(unix)]
fn apply_command(
    install: &std::path::Path,
    gh: &std::path::Path,
    bundle: &BundlePaths,
) -> ReleaseCommand {
    let source_ref = format!("refs/tags/v{}", bundle.version);
    parse(&args(&[
        "apply",
        "--root",
        install.to_str().unwrap(),
        "--manifest",
        bundle.manifest.to_str().unwrap(),
        "--manifest-bundle",
        bundle.manifest_bundle.to_str().unwrap(),
        "--artifact",
        bundle.artifact.to_str().unwrap(),
        "--artifact-bundle",
        bundle.artifact_bundle.to_str().unwrap(),
        "--repo",
        "openai/heycode",
        "--workflow",
        ".github/workflows/release.yml",
        "--source-ref",
        &source_ref,
        "--channel",
        "stable",
        "--gh",
        gh.to_str().unwrap(),
    ]))
    .unwrap()
}
