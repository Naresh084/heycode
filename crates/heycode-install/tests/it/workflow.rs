//! Q14–Q16 workflow causality: the shipping candidate installs and the stable path runs.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repository_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn native_onboarding_uses_the_downloaded_candidate_as_the_release_manager() {
    let workflow = std::fs::read_to_string(repository_file(
        ".github/workflows/release-verification.yml",
    ))
    .expect("release workflow");

    assert!(
        workflow.contains("  onboarding-native:\n"),
        "a job that executes native evidence must not be named as a definition"
    );
    assert!(
        !workflow.contains("cargo run -q -p heycode-cli -- release apply"),
        "building a checkout-owned manager in the onboarding job bypasses the downloaded candidate"
    );
    assert!(
        workflow.contains("HEYCODE_HOME=\"$product_home\" \"$artifact\" release apply"),
        "the Unix candidate must invoke its own shipping release command"
    );
    assert!(
        workflow.contains("& $artifact release apply"),
        "the Windows candidate must invoke its own shipping release command"
    );
    assert!(workflow.contains("if ($LASTEXITCODE -ne 0) {"));
    assert!(workflow.contains("    needs: onboarding-native\n"));
    let verified = workflow
        .find("      - name: Verify binary and manifest attestations\n")
        .expect("external attestation verification step");
    let installed = workflow
        .find("      - name: Install through the shipping release manager (Unix)\n")
        .expect("shipping install step");
    assert!(
        verified < installed,
        "the candidate must verify before it runs"
    );

    let powershell = std::fs::read_to_string(repository_file(
        "release-support/run-fresh-machine-smoke.ps1",
    ))
    .expect("PowerShell onboarding smoke");
    assert!(!powershell.contains("Copy-Item"));
    assert!(powershell.contains("$output = & $installed --restricted-workspace"));
}

#[test]
fn distribution_build_is_compact_measured_and_staged_from_its_own_profile() {
    let manifest =
        std::fs::read_to_string(repository_file("Cargo.toml")).expect("workspace manifest");
    let distribution = manifest
        .split_once("[profile.dist]\n")
        .map(|(_, profile)| profile)
        .expect("dedicated distribution profile");
    let distribution = distribution
        .split_once("\n[")
        .map_or(distribution, |(profile, _)| profile);

    for required in [
        "inherits = \"release\"",
        "opt-level = \"z\"",
        "lto = \"fat\"",
        "codegen-units = 1",
        "strip = \"symbols\"",
        "debug = false",
        "incremental = false",
        "panic = \"unwind\"",
    ] {
        assert!(
            distribution.lines().any(|line| line.trim() == required),
            "distribution profile is missing `{required}`"
        );
    }

    let workflow = std::fs::read_to_string(repository_file(
        ".github/workflows/release-verification.yml",
    ))
    .expect("release workflow");
    assert!(workflow.contains("      CARGO_TARGET_DIR: ${{ runner.temp }}/heycode-dist-target\n"));
    assert!(workflow.contains("cargo build --profile dist --locked -p heycode-cli"));
    assert!(!workflow.contains("cargo build --release --locked -p heycode-cli"));
    assert!(
        workflow
            .contains("cp \"$CARGO_TARGET_DIR/dist/heycode${{ steps.native.outputs.extension }}\"")
    );
    assert!(!workflow.contains("cp \"target/dist/heycode${{ steps.native.outputs.extension }}\""));
    assert!(workflow.contains("      - name: Record exact distribution size\n"));
    assert!(workflow.contains("binary_bytes=$(wc -c < \"$binary\" | tr -d '[:space:]')"));
    assert!(workflow.contains("\"profile\":\"dist\""));
    assert!(workflow.contains("macos-aarch64) maximum_binary_bytes=20971520 ;;"));
    assert!(workflow.contains("*) maximum_binary_bytes= ;;"));
    assert!(workflow.contains(
        "if [ -n \"$maximum_binary_bytes\" ] && [ \"$binary_bytes\" -gt \"$maximum_binary_bytes\" ]; then"
    ));
    assert!(workflow.contains("distribution artifact exceeds the observed platform budget"));

    let staged = workflow
        .find("      - name: Stage exact release subject\n")
        .expect("distribution staging step");
    let measured = workflow
        .find("      - name: Record exact distribution size\n")
        .expect("distribution size step");
    let attested = workflow
        .find("      - name: Attest binary provenance\n")
        .expect("binary attestation step");
    assert!(staged < measured && measured < attested);
}

#[cfg(unix)]
#[test]
fn unix_smoke_executes_the_exact_release_manager_installed_path() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    use heycode_install::{FreshMachineMatrix, OnboardingPlatform, parse_fresh_machine_evidence};

    let temp = tempfile::tempdir().unwrap();
    let installed = temp.path().join("installed-heycode");
    let trace = temp.path().join("executed-path");
    let evidence = temp.path().join("evidence.json");
    std::fs::write(
        &installed,
        "#!/bin/sh\nprintf '%s' \"$0\" > \"$HEYCODE_TRACE_FILE\"\nprintf 'FAKE-REPLY\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o700)).unwrap();

    let status = Command::new("bash")
        .arg(repository_file(
            "release-support/run-fresh-machine-smoke.sh",
        ))
        .arg(&installed)
        .arg("macos-aarch64")
        .arg(&evidence)
        .env("HEYCODE_TRACE_FILE", &trace)
        .env("GITHUB_RUN_ID", "4242")
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap(),
        installed.to_string_lossy(),
        "copying the stable binary before execution bypasses the exact path the release manager published"
    );

    let raw = std::fs::read(&evidence).unwrap();
    let matrix = FreshMachineMatrix::evaluate(parse_fresh_machine_evidence(&raw).unwrap()).unwrap();
    assert!(
        matrix
            .platform(OnboardingPlatform::Macos)
            .deterministic_native_complete()
    );
    let rendered = String::from_utf8(raw).unwrap();
    assert!(!rendered.contains(&installed.to_string_lossy().to_string()));
    assert!(!rendered.contains("FAKE-REPLY"));
}
