//! The default world cannot emit, proved against the crate itself.
//!
//! The unit tests show that a local-off service hands nothing to an exporter.
//! That is a statement about one code path. This file makes the stronger claim
//! the row actually needs — *there is no code path* — by pinning the two facts
//! that would have to change first:
//!
//! 1. the crate's *direct* dependency set contains nothing that reaches
//!    off-process, so nothing here can name a socket or a process launcher —
//!    a transitive crate that could is unreachable without editing this
//!    manifest, which is what makes the manifest the thing worth pinning;
//! 2. these sources contain no call that reaches off-process directly.
//!
//! It lives in `tests/` so the scan never reads its own needles.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Everything `heycode-telemetry` is allowed to name. Each is a data or
/// composition crate; none exposes a socket or a process launcher.
const ALLOWED_DEPENDENCIES: &[&str] = &["heycode-core", "heycode-settings", "serde", "serde_json"];

/// Code-shaped needles for reaching off-process. Prose is safe: these are the
/// spellings a call site actually uses.
const FORBIDDEN_CALLS: &[&str] = &[
    "std::net",
    "tokio::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "SocketAddr",
    "std::process::Command",
    "Command::new",
    "reqwest",
    "ureq",
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest() -> String {
    std::fs::read_to_string(crate_root().join("Cargo.toml")).expect("the crate manifest must exist")
}

/// Keys declared under a top-level manifest `section`.
fn table_keys(manifest: &str, section: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == section;
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            names.insert(name.trim().to_owned());
        }
    }
    names
}

fn source_files(directory: &Path, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory).expect("the source directory must be readable");
    for entry in entries {
        let path = entry.expect("a directory entry must be readable").path();
        if path.is_dir() {
            source_files(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

#[test]
fn the_crate_depends_on_nothing_that_can_reach_off_process() {
    let declared = table_keys(&manifest(), "[dependencies]");
    let allowed: BTreeSet<String> = ALLOWED_DEPENDENCIES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        declared, allowed,
        "the default provider's guarantee is that egress is unimplementable here; \
         adding a dependency that can reach off-process breaks it"
    );
}

#[test]
fn no_source_file_reaches_off_process() {
    let mut files = Vec::new();
    source_files(&crate_root().join("src"), &mut files);
    assert!(!files.is_empty(), "the scan must actually read something");
    for file in files {
        let source = std::fs::read_to_string(&file).expect("a source file must be readable");
        for needle in FORBIDDEN_CALLS {
            assert!(
                !source.contains(needle),
                "{} mentions {needle}; the local-off default must not be able to emit",
                file.display()
            );
        }
    }
}

#[test]
fn the_crate_inherits_the_workspace_lints_that_forbid_unsafe() {
    let manifest = manifest();
    let lints = table_keys(&manifest, "[lints]");
    assert!(
        lints.contains("workspace"),
        "without the workspace lint table this crate loses `unsafe_code = forbid`, \
         and forbidding unsafe is part of why the dependency scan is sufficient"
    );
}

#[test]
fn the_public_surface_offers_no_constructor_that_supplies_its_own_exporter() {
    // `local_off` takes nothing, so no argument can carry egress in; `exporting`
    // is the only other constructor and it takes egress from the caller. This
    // asserts the shape a reader would otherwise have to check by eye.
    let service = heycode_telemetry::TelemetryService::local_off();
    assert_eq!(
        service.egress(),
        heycode_telemetry::EgressKind::LocalOff,
        "the no-argument constructor must be the local-off one"
    );

    let plugin = heycode_telemetry::telemetry_plugin();
    assert_eq!(
        plugin.provides(),
        &[heycode_telemetry::SERVICE_TELEMETRY],
        "the default provider publishes the local-off service"
    );
}

#[test]
fn the_otel_provider_added_by_tel03_still_takes_its_egress_from_the_caller() {
    // TEL03 shipped a second plugin that *does* emit, which invalidates the
    // premise of the assertion above that this crate ships only one — but not
    // the invariant behind it. The invariant was never "there is one plugin"; it
    // is "no constructor here supplies its own egress", and it survives:
    // `otel_telemetry_plugin` demands an `OtlpTransport`, and this crate
    // implements that trait nowhere, so a caller must write one in a crate that
    // can reach a wire. That is why this file's other two tests — the dependency
    // set and the source scan — remain the whole guarantee.
    let manifest = manifest();
    assert!(
        !manifest.contains("opentelemetry") && !manifest.contains("otlp"),
        "the OTLP payload is built from serde_json; adding an exporter SDK here \
         would put egress back inside the crate that must not have it"
    );

    let mut files = Vec::new();
    source_files(&crate_root().join("src"), &mut files);
    let mut seen_test_impl = false;
    for file in &files {
        let source = std::fs::read_to_string(file).expect("a source file must be readable");
        // Only the part of the file above its `#[cfg(test)]` module ships. A
        // whole-file check would pass for a production impl living in a file
        // that also happens to have tests, which is every file here — a check
        // that cannot fail is worse than no check (GOTCHAS #160).
        let shipped = source
            .split_once("#[cfg(test)]")
            .map_or(source.as_str(), |(before, _)| before);
        assert!(
            !shipped.contains("impl OtlpTransport for"),
            "{} implements OtlpTransport outside its tests; that would be an \
             exporter this crate supplies to itself",
            file.display()
        );
        seen_test_impl |= source.contains("impl OtlpTransport for");
    }
    assert!(
        seen_test_impl,
        "no file implements OtlpTransport at all, so this scan proved nothing"
    );
}
