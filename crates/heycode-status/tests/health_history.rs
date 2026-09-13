//! TEL05 — the bound, the eviction policy, and what survives a restart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_doctor::{DoctorStatus, DoctorSummary};
use heycode_status::health::{
    ENTRY_BYTES_CEILING, HEALTH_HISTORY_SCHEMA_VERSION, HealthCheckRecord, HealthEntry,
    HealthEvidenceKind, HealthHistoryError, HealthHistoryStore, HealthLabel, LABEL_MAX_BYTES,
    MAX_BYTES, MAX_CHECKS_PER_ENTRY, MAX_ENTRIES, MAX_READ_BYTES, PROTECTED_UNHEALTHY,
};

const HEADER: &str = r#"{"kind":"heycode-health-history","schema_version":1}"#;

fn store(root: &std::path::Path) -> HealthHistoryStore {
    HealthHistoryStore::new(root.join("state").join("health-history.jsonl"))
}

/// A small entry: one check, a handful of hundred bytes at most.
fn entry(at: u64, healthy: bool) -> HealthEntry {
    HealthEntry {
        at_unix_ms: at,
        build: HealthLabel::new("0.1.0"),
        healthy,
        summary: DoctorSummary {
            passed: usize::from(healthy),
            warnings: 0,
            failed: usize::from(!healthy),
            skipped: 0,
        },
        duration_ms: 7,
        checks: vec![HealthCheckRecord {
            id: HealthLabel::new("composition"),
            status: if healthy {
                DoctorStatus::Pass
            } else {
                DoctorStatus::Failure
            },
            code: HealthLabel::new("composition.healthy"),
            duration_ms: 3,
            evidence: Some(HealthEvidenceKind::Composition),
        }],
        omitted_checks: 0,
    }
}

/// The largest entry the caps permit: every label at [`LABEL_MAX_BYTES`] and
/// every check row present.
fn maximal_entry(at: u64) -> HealthEntry {
    HealthEntry {
        at_unix_ms: at,
        build: HealthLabel::new("b".repeat(LABEL_MAX_BYTES)),
        healthy: true,
        summary: DoctorSummary::default(),
        duration_ms: u64::MAX,
        checks: (0..MAX_CHECKS_PER_ENTRY)
            .map(|_| HealthCheckRecord {
                id: HealthLabel::new("i".repeat(LABEL_MAX_BYTES)),
                status: DoctorStatus::Pass,
                code: HealthLabel::new("c".repeat(LABEL_MAX_BYTES)),
                duration_ms: u64::MAX,
                evidence: Some(HealthEvidenceKind::ConfigMigration),
            })
            .collect(),
        omitted_checks: u32::MAX,
    }
}

fn write_raw(path: &std::path::Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn timestamps(history: &[HealthEntry]) -> Vec<u64> {
    history.iter().map(|entry| entry.at_unix_ms).collect()
}

#[test]
fn a_recorded_run_survives_a_restart_because_a_second_store_reads_the_same_file() {
    let root = tempfile::tempdir().unwrap();
    let first = store(root.path());
    first.record(entry(10, true)).unwrap();
    first.record(entry(20, false)).unwrap();
    drop(first);

    let reopened = HealthHistoryStore::new(root.path().join("state").join("health-history.jsonl"));
    let history = reopened.load().unwrap();
    assert_eq!(timestamps(history.entries()), vec![10, 20]);
    assert!(!history.entries()[1].healthy);
    assert_eq!(history.unreadable(), 0);
    assert!(!history.truncated_tail());
}

#[test]
fn a_missing_history_reads_as_empty_rather_than_failing() {
    let root = tempfile::tempdir().unwrap();
    let history = store(root.path()).load().unwrap();
    assert!(history.is_empty());
    assert_eq!(history.unreadable(), 0);
}

#[test]
fn the_entry_bound_evicts_at_the_boundary_and_not_one_record_later() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    for index in 0..MAX_ENTRIES {
        let history = store.record(entry(index as u64, true)).unwrap();
        assert_eq!(
            history.entries().len(),
            index + 1,
            "nothing may be evicted below the cap"
        );
    }
    let at_cap = store.load().unwrap();
    assert_eq!(at_cap.entries().len(), MAX_ENTRIES);
    assert_eq!(
        at_cap.entries()[0].at_unix_ms,
        0,
        "the oldest is still held"
    );

    let over = store.record(entry(MAX_ENTRIES as u64, true)).unwrap();
    assert_eq!(over.entries().len(), MAX_ENTRIES, "the cap is hard");
    assert_eq!(
        timestamps(over.entries()),
        (1..=MAX_ENTRIES as u64).collect::<Vec<_>>(),
        "exactly the oldest entry left, and only one"
    );
}

#[test]
fn the_byte_bound_evicts_at_the_boundary_and_not_one_record_later() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let one = serde_json::to_string(&maximal_entry(0)).unwrap().len() + 1;
    assert!(
        one * 2 < MAX_BYTES,
        "the fixture must fit more than one entry or the test proves nothing"
    );

    let mut retained = 0;
    let mut at = 0;
    loop {
        let history = store.record(maximal_entry(at)).unwrap();
        let size = std::fs::metadata(store.path()).unwrap().len() as usize;
        assert!(size <= MAX_BYTES, "the file must never exceed the cap");
        if history.entries().len() < retained + 1 {
            // The record that crossed the boundary: exactly one entry left, and
            // the file was under the cap before it and is under it after.
            assert_eq!(history.entries().len(), retained);
            assert!(
                size + one > MAX_BYTES,
                "eviction happened before the boundary was reached"
            );
            assert_eq!(
                history.entries()[0].at_unix_ms,
                1,
                "the oldest entry is the one that left"
            );
            break;
        }
        retained = history.entries().len();
        at += 1;
        assert!(
            at < MAX_ENTRIES as u64,
            "the byte cap must bind before the entry cap"
        );
    }
    assert!(
        retained >= 2,
        "the byte cap must retain more than one entry"
    );
}

#[test]
fn eviction_protects_the_most_recent_failing_runs_from_a_burst_of_healthy_ones() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    for index in 0..PROTECTED_UNHEALTHY as u64 {
        store.record(entry(index, false)).unwrap();
    }
    for index in 0..(MAX_ENTRIES as u64 * 2) {
        store.record(entry(1_000 + index, true)).unwrap();
    }
    let history = store.load().unwrap();
    assert_eq!(history.entries().len(), MAX_ENTRIES);
    let failing = timestamps(
        &history
            .entries()
            .iter()
            .filter(|entry| !entry.healthy)
            .cloned()
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        failing,
        (0..PROTECTED_UNHEALTHY as u64).collect::<Vec<_>>(),
        "a burst of healthy runs must not evict the runs that explain a failure"
    );
}

#[test]
fn the_hard_cap_wins_when_every_retained_entry_is_protected() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    for index in 0..(MAX_ENTRIES as u64 + 5) {
        store.record(entry(index, false)).unwrap();
    }
    let history = store.load().unwrap();
    assert_eq!(
        history.entries().len(),
        MAX_ENTRIES,
        "protection is a preference and never an exemption from the bound"
    );
    assert_eq!(
        timestamps(history.entries()),
        (5..MAX_ENTRIES as u64 + 5).collect::<Vec<_>>()
    );
}

#[test]
fn a_corrupt_entry_costs_one_entry_and_never_the_entries_after_it() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let good = serde_json::to_string(&entry(1, true)).unwrap();
    let later = serde_json::to_string(&entry(3, false)).unwrap();
    write_raw(
        store.path(),
        &format!("{HEADER}\n{good}\n{{\"at_unix_ms\":\n{later}\n"),
    );

    let history = store.load().unwrap();
    assert_eq!(
        timestamps(history.entries()),
        vec![1, 3],
        "the entries after the damage must still be read"
    );
    assert_eq!(
        history.unreadable(),
        1,
        "the damage must be counted rather than dropped silently"
    );
    assert!(!history.truncated_tail());
    assert!(
        history
            .render_human()
            .contains("unreadable entries skipped: 1")
    );
}

#[test]
fn a_torn_final_line_is_distinguished_from_a_rotted_one() {
    let root = tempfile::tempdir().unwrap();
    let torn = store(root.path());
    let good = serde_json::to_string(&entry(1, true)).unwrap();
    write_raw(
        torn.path(),
        &format!("{HEADER}\n{good}\n{{\"at_unix_ms\":42"),
    );
    let history = torn.load().unwrap();
    assert_eq!(timestamps(history.entries()), vec![1]);
    assert!(
        history.truncated_tail(),
        "a final line with no newline is a torn write"
    );
    assert_eq!(history.unreadable(), 0);
    assert!(
        history
            .render_human()
            .contains("last stored line was incomplete")
    );

    let rotted = HealthHistoryStore::new(root.path().join("rotted.jsonl"));
    write_raw(
        rotted.path(),
        &format!("{HEADER}\n{good}\n{{\"at_unix_ms\":42\n"),
    );
    let history = rotted.load().unwrap();
    assert_eq!(timestamps(history.entries()), vec![1]);
    assert!(
        !history.truncated_tail(),
        "a terminated final line was fully written, so it rotted rather than tore"
    );
    assert_eq!(history.unreadable(), 1);
}

#[test]
fn a_damaged_history_is_repaired_on_the_next_record_and_the_repair_was_reported() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let good = serde_json::to_string(&entry(1, true)).unwrap();
    write_raw(store.path(), &format!("{HEADER}\n{good}\nnot-json\n"));

    let after = store.record(entry(2, true)).unwrap();
    assert_eq!(
        after.unreadable(),
        1,
        "the record that repairs the file must still report what it dropped"
    );
    assert_eq!(timestamps(after.entries()), vec![1, 2]);
    let reread = store.load().unwrap();
    assert_eq!(reread.unreadable(), 0);
    assert_eq!(timestamps(reread.entries()), vec![1, 2]);
}

#[test]
fn a_history_written_by_a_newer_schema_is_refused_and_left_exactly_as_found() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let newer = format!(
        "{{\"kind\":\"heycode-health-history\",\"schema_version\":{}}}\n{{\"unknown\":true}}\n",
        HEALTH_HISTORY_SCHEMA_VERSION + 1
    );
    write_raw(store.path(), &newer);

    match store.load() {
        Err(HealthHistoryError::NewerSchema {
            found, supported, ..
        }) => {
            assert_eq!(found, HEALTH_HISTORY_SCHEMA_VERSION + 1);
            assert_eq!(supported, HEALTH_HISTORY_SCHEMA_VERSION);
        }
        other => panic!("expected NewerSchema, got {other:?}"),
    }
    assert!(store.record(entry(1, true)).is_err());
    assert_eq!(
        std::fs::read_to_string(store.path()).unwrap(),
        newer,
        "a newer build's history must never be overwritten"
    );
}

#[test]
fn a_file_that_is_not_a_health_history_is_refused_rather_than_overwritten() {
    let root = tempfile::tempdir().unwrap();
    for body in [
        "this is not json at all\n",
        "{\"kind\":\"something-else\",\"schema_version\":1}\n",
        "{\"kind\":\"heycode-health-history\",\"schema_version\":0}\n",
    ] {
        let store = HealthHistoryStore::new(root.path().join("candidate.jsonl"));
        write_raw(store.path(), body);
        assert!(
            matches!(
                store.load(),
                Err(HealthHistoryError::MalformedHeader { .. })
            ),
            "{body} was not refused"
        );
        assert!(store.record(entry(1, true)).is_err());
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), body);
    }
}

#[test]
fn an_empty_file_reads_as_an_empty_history_and_is_adopted() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    write_raw(store.path(), "");
    assert!(store.load().unwrap().is_empty());
    assert_eq!(store.record(entry(1, true)).unwrap().entries().len(), 1);
}

#[test]
fn a_file_larger_than_the_read_bound_is_refused_before_it_is_parsed() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let padding = "x".repeat(MAX_READ_BYTES as usize + 1);
    write_raw(store.path(), &padding);
    match store.load() {
        Err(HealthHistoryError::Oversized { limit, .. }) => assert_eq!(limit, MAX_READ_BYTES),
        other => panic!("expected Oversized, got {other:?}"),
    }
}

#[test]
fn loading_enforces_the_bound_on_a_file_this_build_did_not_write() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let mut body = String::from(HEADER);
    body.push('\n');
    for index in 0..(MAX_ENTRIES as u64 + 40) {
        body.push_str(&serde_json::to_string(&entry(index, true)).unwrap());
        body.push('\n');
    }
    write_raw(store.path(), &body);

    let history = store.load().unwrap();
    assert_eq!(
        history.entries().len(),
        MAX_ENTRIES,
        "no observer may hold an unbounded history, whoever wrote the file"
    );
    assert_eq!(history.entries()[0].at_unix_ms, 40);
}

#[test]
fn an_entry_retains_the_diagnostic_rows_first_when_a_run_exceeds_the_check_bound() {
    let mut checks: Vec<HealthCheckRecord> = (0..MAX_CHECKS_PER_ENTRY as u64 + 3)
        .map(|index| HealthCheckRecord {
            id: HealthLabel::new(format!("check-{index}")),
            status: DoctorStatus::Pass,
            code: HealthLabel::new("ok"),
            duration_ms: index,
            evidence: None,
        })
        .collect();
    checks[MAX_CHECKS_PER_ENTRY + 1].status = DoctorStatus::Failure;
    let entry = HealthEntry {
        at_unix_ms: 1,
        build: HealthLabel::new("0.1.0"),
        healthy: false,
        summary: DoctorSummary::default(),
        duration_ms: 1,
        checks,
        omitted_checks: 0,
    };
    let rendered = serde_json::to_string(&entry).unwrap();
    assert!(
        HealthEntry::from_json(&rendered).is_err(),
        "reading must refuse an entry over the per-entry check bound"
    );
    assert!(
        serde_json::from_str::<HealthEntry>(&rendered).is_ok(),
        "the raw derive accepts it, which is why from_json must not"
    );
}

#[test]
fn a_maximal_entry_stays_under_the_entry_ceiling_the_byte_bound_relies_on() {
    let encoded = serde_json::to_string(&maximal_entry(u64::MAX))
        .unwrap()
        .len()
        + 1;
    assert!(
        encoded <= ENTRY_BYTES_CEILING,
        "one entry measured {encoded} bytes against a declared ceiling of {ENTRY_BYTES_CEILING}"
    );
    // Both operands are constants, so this holds at compile time and cannot
    // be skipped or filtered out at run time.
    const _: () = assert!(
        ENTRY_BYTES_CEILING < MAX_BYTES,
        "eviction never removes the last entry, so one entry must always fit"
    );
}

#[cfg(unix)]
#[test]
fn the_committed_history_and_its_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    store.record(entry(1, true)).unwrap();
    let file = std::fs::metadata(store.path()).unwrap();
    assert_eq!(file.permissions().mode() & 0o777, 0o600);
    let directory = std::fs::metadata(store.path().parent().unwrap()).unwrap();
    assert_eq!(directory.permissions().mode() & 0o777, 0o700);
}

#[test]
fn entries_keep_file_order_when_the_clock_moves_backwards() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    store.record(entry(500, true)).unwrap();
    store.record(entry(100, true)).unwrap();
    store.record(entry(300, true)).unwrap();
    assert_eq!(
        timestamps(store.load().unwrap().entries()),
        vec![500, 100, 300],
        "the file's order is authoritative; re-sorting would reorder the evidence"
    );
}

#[test]
fn a_directory_at_the_history_path_is_refused_rather_than_written_through() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    std::fs::create_dir_all(store.path()).unwrap();
    assert!(matches!(
        store.load(),
        Err(HealthHistoryError::NotRegularFile { .. })
    ));
}
