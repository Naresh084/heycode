//! QSEC03 — adversarial profile-shape matrix for the E10 sandbox backends.
//!
//! These cases attack the *construction* of each backend's confinement
//! argument rather than its runtime behaviour, so they run on every host and
//! need no OS sandbox facility. The runtime half lives in
//! `tests/it/escape_matrix.rs` and is macOS-gated.
//!
//! Backends are constructed by struct literal on purpose: `new()` refuses to
//! build off its own platform, and the profile text is platform-independent
//! data that every host must be able to check.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use heycode_exec::{Sandbox, SandboxMode, SandboxPolicy};

use crate::landlock::{LandlockSandbox, profile_for};
use crate::{BwrapSandbox, SeatbeltSandbox};

/// A workspace root whose *name* closes the `(subpath "…")` form the Seatbelt
/// profile builds, appends a blanket write grant, and reopens the form so the
/// remainder of the profile still parses. Every character in it is legal in a
/// macOS directory name.
const CRAFTED_ROOT: &str = "/private/tmp/dshx-qsec03/ws\"))(allow file-write*)(allow file-write* (subpath \
     \"/private/tmp/dshx-qsec03/ws";

/// The clause the crafted root injects, in the exact form a profile parser
/// sees it: an unconditional filesystem write grant.
const INJECTED_CLAUSE: &str = "(allow file-write*)";

fn seatbelt() -> SeatbeltSandbox {
    SeatbeltSandbox {
        binary: PathBuf::from("/usr/bin/sandbox-exec"),
    }
}

fn bwrap() -> BwrapSandbox {
    BwrapSandbox {
        binary: PathBuf::from("/usr/bin/bwrap"),
    }
}

fn policy(mode: SandboxMode, root: &str) -> SandboxPolicy {
    SandboxPolicy {
        mode,
        workspace_root: PathBuf::from(root),
    }
}

/// The emitted Seatbelt profile, with the crafted root left uncanonicalised
/// (`confine` canonicalises when the path exists and otherwise passes it
/// through, so this is the shape a nonexistent-but-named root produces).
fn seatbelt_profile(mode: SandboxMode, root: &str) -> String {
    let argv = seatbelt()
        .confine(&["true".to_owned()], &policy(mode, root))
        .expect("seatbelt argv builds");
    argv[2].clone()
}

fn occurrences_outside_strings(value: &str, needle: &str) -> usize {
    let bytes = value.as_bytes();
    let needle = needle.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut count = 0;
    for index in 0..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if bytes.get(index..index.saturating_add(needle.len())) == Some(needle) {
            count += 1;
        }
    }
    count
}

/// QSEC03 finding S1 requirement. A workspace root is data inside exactly one
/// Seatbelt string literal; quotes, backslashes and parentheses must never
/// create another profile clause.
#[test]
fn qsec03_requirement_a_crafted_workspace_root_cannot_inject_a_seatbelt_clause() {
    let profile = seatbelt_profile(SandboxMode::WorkspaceWrite, CRAFTED_ROOT);
    assert_eq!(
        occurrences_outside_strings(&profile, INJECTED_CLAUSE),
        0,
        "the crafted root injected an unconditional write grant outside its string: {profile}"
    );
    assert_eq!(
        occurrences_outside_strings(&profile, "(allow file-write* (subpath \""),
        3,
        "workspace-write must retain exactly the workspace and two temp subpath grants: {profile}"
    );
    assert!(
        profile.contains(r#"ws\"))(allow file-write*)"#),
        "the quote must be escaped inside the path literal: {profile}"
    );
}

/// The clause count is the structural complement to the payload assertion:
/// changing the root value must not change the profile grammar.
#[test]
fn qsec03_requirement_no_workspace_root_can_add_or_remove_a_seatbelt_clause() {
    let benign = seatbelt_profile(SandboxMode::WorkspaceWrite, "/private/tmp/dshx-qsec03/ws");
    let crafted = seatbelt_profile(SandboxMode::WorkspaceWrite, CRAFTED_ROOT);
    assert_eq!(
        occurrences_outside_strings(&benign, "(allow file-write*"),
        occurrences_outside_strings(&crafted, "(allow file-write*"),
        "a workspace root must never change the number of write grants in the profile"
    );
    assert!(
        occurrences_outside_strings(&crafted, INJECTED_CLAUSE) == 0,
        "the crafted root injected an unconditional write grant outside its string: {crafted}"
    );
}

/// An ordinary quoted folder is the over-blocking control: it remains a valid
/// path value, carried with the quote escaped rather than refused or spliced.
#[test]
fn an_ordinary_quoted_folder_name_is_escaped_without_being_rejected() {
    let profile = seatbelt_profile(SandboxMode::WorkspaceWrite, "/Users/dev/My \"Project\"");
    assert!(
        profile.contains(r#"subpath "/Users/dev/My \"Project\"""#),
        "the path and its escaped quotes must remain present: {profile}"
    );
}

/// Backslashes are escaped for the same reason as quotes: an attacker must
/// not be able to escape the closing delimiter. Control-bearing paths fail
/// closed instead of relying on undocumented profile escapes.
#[test]
fn backslashes_are_escaped_and_control_bearing_roots_are_refused() {
    let profile = seatbelt_profile(SandboxMode::WorkspaceWrite, r#"/Users/dev/a\b"#);
    assert!(
        profile.contains(r#"subpath "/Users/dev/a\\b""#),
        "a path backslash must be escaped inside the literal: {profile}"
    );
    assert!(
        seatbelt()
            .confine(
                &["true".to_owned()],
                &policy(SandboxMode::WorkspaceWrite, "/private/tmp/line\nbreak"),
            )
            .is_err(),
        "a control-bearing root must fail closed"
    );
}

/// Every backend eventually carries the workspace through a UTF-8 `String`
/// boundary (profile text, JSON, or argv). A lossy conversion could grant a
/// different on-disk name containing U+FFFD, so an unrepresentable root must
/// fail instead of being rewritten.
#[cfg(unix)]
#[test]
fn every_backend_refuses_a_non_utf8_workspace_root_without_lossy_rewriting() {
    use std::os::unix::ffi::OsStringExt as _;

    let root = PathBuf::from(std::ffi::OsString::from_vec(
        b"/private/tmp/dshx-non-utf8-\xff".to_vec(),
    ));
    let policy = SandboxPolicy {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: root,
    };
    let argv = ["true".to_owned()];
    for backend in [
        &seatbelt() as &dyn Sandbox,
        &bwrap() as &dyn Sandbox,
        &LandlockSandbox,
    ] {
        assert!(
            backend.confine(&argv, &policy).is_err(),
            "{} rewrote an unrepresentable workspace root",
            backend.name()
        );
    }
}

/// Read-only profiles never serialize or bind the workspace root, so a path
/// that is irrelevant to that mode must not be rejected merely because the
/// workspace-write representation is UTF-8-only.
#[cfg(unix)]
#[test]
fn read_only_does_not_overblock_an_unrepresented_non_utf8_workspace_root() {
    use std::os::unix::ffi::OsStringExt as _;

    let policy = SandboxPolicy {
        mode: SandboxMode::ReadOnly,
        workspace_root: PathBuf::from(std::ffi::OsString::from_vec(
            b"/private/tmp/dshx-non-utf8-\xff".to_vec(),
        )),
    };
    let argv = ["true".to_owned()];
    for backend in [
        &seatbelt() as &dyn Sandbox,
        &bwrap() as &dyn Sandbox,
        &LandlockSandbox,
    ] {
        backend
            .confine(&argv, &policy)
            .unwrap_or_else(|_| panic!("{} rejected an unused workspace root", backend.name()));
    }
}

/// The differential that localises the defect: the same crafted root is inert
/// in both Linux backends, because one JSON-encodes it and the other passes it
/// as a distinct argv element. Only Seatbelt splices it into a grammar.
#[test]
fn the_same_crafted_root_is_inert_in_the_landlock_and_bwrap_backends() {
    let rules = profile_for(&policy(SandboxMode::WorkspaceWrite, CRAFTED_ROOT))
        .expect("build Landlock rules");
    let workspace = rules
        .grants
        .iter()
        .find(|grant| grant.path == CRAFTED_ROOT)
        .expect("landlock grants the crafted root as one opaque path");
    assert_eq!(
        workspace.path, CRAFTED_ROOT,
        "the path is carried, not parsed"
    );
    let encoded = serde_json::to_string(&rules).expect("landlock rules serialize");
    // Stated as a positive, because the negative form is unsatisfiable: the
    // correctly escaped JSON `\"))(allow` *contains* the bare `"))(allow`
    // substring, so asserting the bare form is absent fails on correct output.
    // What actually distinguishes escaped from spliced is the backslash.
    assert!(
        encoded.contains(r#"\"))(allow"#),
        "the crafted quote must be carried backslash-escaped, never emitted as \
         JSON structure: {encoded}"
    );
    let decoded: crate::LandlockRules =
        serde_json::from_str(&encoded).expect("landlock rules round-trip");
    assert_eq!(
        decoded
            .grants
            .iter()
            .filter(|grant| grant.path == CRAFTED_ROOT)
            .count(),
        1,
        "the round-trip must yield exactly one grant, not a split one"
    );

    let argv = bwrap()
        .confine(
            &["true".to_owned()],
            &policy(SandboxMode::WorkspaceWrite, CRAFTED_ROOT),
        )
        .expect("bwrap argv builds");
    assert_eq!(
        argv.iter()
            .filter(|element| *element == CRAFTED_ROOT)
            .count(),
        2,
        "bwrap must carry the root as two whole argv elements of --bind: {argv:?}"
    );
    // `CRAFTED_ROOT` *is* the payload, so forbidding "file-write" anywhere in
    // argv forbids the very string the case plants. The property that matters
    // is narrower and stronger: wherever the payload appears it is the whole
    // argv element, never a fragment of one bwrap built by concatenation.
    assert!(
        argv.iter()
            .filter(|element| element.contains("file-write"))
            .all(|element| element == CRAFTED_ROOT),
        "the payload may appear only as a whole argv element, never spliced \
         into a constructed argument: {argv:?}"
    );
}

/// Read-only must grant no path at all beyond the device sink, in every
/// backend. A stray `subpath` grant here would make the report's
/// `FileWriteScope::DeviceOnly` a lie.
#[test]
fn read_only_grants_no_writable_path_beyond_the_device_sink_in_any_backend() {
    let profile = seatbelt_profile(SandboxMode::ReadOnly, "/private/tmp/dshx-qsec03/ws");
    assert!(profile.contains("(deny file-write*)"), "{profile}");
    assert!(
        !profile.contains("subpath"),
        "read-only must grant no subpath: {profile}"
    );
    assert_eq!(
        profile.matches("allow file-write*").count(),
        1,
        "exactly one write grant, the device sink: {profile}"
    );
    assert!(profile.contains("(literal \"/dev/null\")"), "{profile}");

    let rules = profile_for(&policy(
        SandboxMode::ReadOnly,
        "/private/tmp/dshx-qsec03/ws",
    ))
    .expect("build read-only Landlock rules");
    const WRITE_FILE: u64 = 1 << 1;
    let writable: Vec<&str> = rules
        .grants
        .iter()
        .filter(|grant| grant.access & WRITE_FILE != 0)
        .map(|grant| grant.path.as_str())
        .collect();
    assert_eq!(
        writable,
        vec!["/dev/null"],
        "landlock read-only must grant write to the device sink only"
    );

    let argv = bwrap()
        .confine(
            &["true".to_owned()],
            &policy(SandboxMode::ReadOnly, "/private/tmp/dshx-qsec03/ws"),
        )
        .expect("bwrap argv builds");
    assert!(
        !argv.iter().any(|element| element == "--bind"),
        "bwrap read-only must not bind anything writable: {argv:?}"
    );
}

/// Workspace-write grants the shared host temp roots in addition to the
/// workspace, in both filesystem backends. That is wider than the mode's name
/// suggests, and it is exactly what `FileWriteScope::WorkspaceAndTemp`
/// promises — this test holds the two together so neither can drift.
#[test]
fn workspace_write_grants_the_shared_host_temp_roots_alongside_the_workspace() {
    let profile = seatbelt_profile(SandboxMode::WorkspaceWrite, "/private/tmp/dshx-qsec03/ws");
    for granted in ["/private/tmp/dshx-qsec03/ws", "/private/tmp", "/tmp"] {
        assert!(
            profile.contains(&format!("(subpath \"{granted}\")")),
            "workspace-write must grant {granted}: {profile}"
        );
    }

    let rules = profile_for(&policy(
        SandboxMode::WorkspaceWrite,
        "/private/tmp/dshx-qsec03/ws",
    ))
    .expect("build workspace-write Landlock rules");
    const ALL_V1: u64 = (1 << 13) - 1;
    let fully_writable: Vec<&str> = rules
        .grants
        .iter()
        .filter(|grant| grant.access == ALL_V1)
        .map(|grant| grant.path.as_str())
        .collect();
    assert!(
        fully_writable.contains(&"/private/tmp/dshx-qsec03/ws"),
        "{fully_writable:?}"
    );
    assert!(
        fully_writable.contains(&"/tmp") || !std::path::Path::new("/tmp").is_dir(),
        "an existing /tmp must be granted so the report and the ruleset agree: {fully_writable:?}"
    );
}

/// Full access adds no restriction, matching the report's `Host` scopes. If
/// this profile ever grew a `deny`, the report row would over-promise in the
/// other direction.
#[test]
fn full_access_emits_a_profile_that_denies_nothing() {
    let profile = seatbelt_profile(SandboxMode::Off, "/private/tmp/dshx-qsec03/ws");
    assert_eq!(profile, "(version 1)(allow default)");
    assert!(!profile.contains("deny"), "{profile}");
}

/// Confinement is all-or-nothing: an empty argv is refused rather than
/// producing a wrapper that would exec whatever follows it.
#[test]
fn every_backend_refuses_to_confine_an_empty_argv() {
    let empty: [String; 0] = [];
    let policy = policy(SandboxMode::WorkspaceWrite, "/private/tmp/dshx-qsec03/ws");
    assert!(seatbelt().confine(&empty, &policy).is_err());
    assert!(bwrap().confine(&empty, &policy).is_err());
    assert!(LandlockSandbox.confine(&empty, &policy).is_err());
}

/// The command separator must be present and the child argv must follow it
/// verbatim, so no argument of the wrapped command can be read as a flag of
/// the wrapper.
#[test]
fn the_wrapped_command_follows_a_separator_and_is_never_reinterpreted() {
    let policy = policy(SandboxMode::WorkspaceWrite, "/private/tmp/dshx-qsec03/ws");
    let hostile = [
        "bash".to_owned(),
        "-c".to_owned(),
        "echo hi".to_owned(),
        "-p".to_owned(),
        "(version 1)(allow default)".to_owned(),
        "--ro-bind".to_owned(),
    ];
    for argv in [
        seatbelt()
            .confine(&hostile, &policy)
            .expect("seatbelt argv"),
        bwrap().confine(&hostile, &policy).expect("bwrap argv"),
        LandlockSandbox
            .confine(&hostile, &policy)
            .expect("landlock argv"),
    ] {
        let separator = argv
            .iter()
            .position(|element| element == "--")
            .expect("a separator is present");
        assert_eq!(
            &argv[separator + 1..],
            &hostile,
            "the child argv must appear verbatim after the separator: {argv:?}"
        );
    }
}
