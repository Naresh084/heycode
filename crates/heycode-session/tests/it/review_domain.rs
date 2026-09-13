#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    FindingOutcome, FindingReport, FindingReportId, FindingReportSource,
    FindingVerificationVerdict, ReportedFinding, ReviewChange, ReviewFailureReason, ReviewFinding,
    ReviewLevel, ReviewRunId, ReviewSeverity, Session, SessionEventKind, project_reviews,
};

#[test]
fn structured_review_round_trips_and_duplicate_settlement_fails() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let run = ReviewRunId::new("review-1").unwrap();
    session
        .append(SessionEventKind::ReviewChange {
            change: Box::new(
                ReviewChange::started(
                    run.clone(),
                    "codex",
                    "0123456789012345678901234567890123456789",
                    "diff --git a/src/lib.rs b/src/lib.rs\n",
                    "Focus on correctness",
                )
                .unwrap(),
            ),
        })
        .unwrap();
    session
        .append(SessionEventKind::ReviewChange {
            change: Box::new(
                ReviewChange::completed(
                    run.clone(),
                    "child-session",
                    "One issue found",
                    vec![
                        ReviewFinding::new(
                            "finding-1",
                            ReviewSeverity::High,
                            "src/lib.rs",
                            10,
                            12,
                            "Unchecked boundary",
                            "The new branch skips validation.",
                        )
                        .unwrap(),
                    ],
                )
                .unwrap(),
            ),
        })
        .unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(directory).unwrap();
    let reviews = project_reviews(reopened.events()).unwrap();
    let review = reviews.run(&run).unwrap();
    assert_eq!(review.summary(), Some("One issue found"));
    assert_eq!(review.findings().len(), 1);

    let mut events = reopened.events().to_vec();
    let mut duplicate = events.last().unwrap().clone();
    duplicate.seq += 1;
    duplicate.kind = SessionEventKind::ReviewChange {
        change: Box::new(ReviewChange::failed(run, ReviewFailureReason::Runtime).unwrap()),
    };
    events.push(duplicate);
    assert!(project_reviews(&events).is_err());
}

#[test]
fn finding_paths_cannot_escape_the_review_workspace() {
    assert!(
        ReviewFinding::new(
            "finding",
            ReviewSeverity::Medium,
            "../outside.rs",
            1,
            1,
            "Escape",
            "Invalid path",
        )
        .is_err()
    );
    assert!(
        ReviewFinding::new(
            "finding",
            ReviewSeverity::Medium,
            "/absolute.rs",
            1,
            1,
            "Escape",
            "Invalid path",
        )
        .is_err()
    );
}

fn reported(
    id: &str,
    path: &str,
    revision: &str,
    failure: &str,
) -> Result<ReportedFinding, heycode_session::ReviewMetadataError> {
    ReportedFinding::new(
        id,
        ReviewSeverity::High,
        path,
        4,
        5,
        revision,
        "Unchecked boundary",
        "Call the parser with a truncated frame.",
        failure,
        "The session terminates and drops pending work.",
    )
}

#[test]
fn direct_finding_reports_replay_with_authority_derived_source_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let report_id = FindingReportId::new("report-1").unwrap();
    let source = FindingReportSource::workspace(
        heycode_core::SessionId::from_raw(session.id().as_str()),
        7,
        1,
        Some("crates/parser".to_owned()),
    )
    .unwrap();
    let report = FindingReport::new(
        report_id.clone(),
        source,
        vec![
            reported(
                "finding-1",
                "crates/parser/src/lib.rs",
                &"a".repeat(64),
                "The length subtraction underflows.",
            )
            .unwrap(),
        ],
    )
    .unwrap();
    session
        .append(SessionEventKind::ReviewChange {
            change: Box::new(ReviewChange::findings_reported(report.clone()).unwrap()),
        })
        .unwrap();
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let reopened = Session::open(directory).unwrap();
    let projection = project_reviews(reopened.events()).unwrap();
    assert_eq!(projection.report(&report_id), Some(&report));
    assert_eq!(projection.reports(), &[report]);
    assert_eq!(projection.reports()[0].level(), None);
    assert_eq!(projection.reports()[0].findings()[0].category(), None);
    assert_eq!(projection.reports()[0].findings()[0].verdict(), None);
    assert_eq!(projection.reports()[0].findings()[0].outcome(), None);

    let old_shape = serde_json::to_value(&projection.reports()[0]).unwrap();
    assert!(old_shape.get("level").is_none());
    assert!(old_shape["findings"][0].get("category").is_none());
    assert!(old_shape["findings"][0].get("verdict").is_none());
    assert!(old_shape["findings"][0].get("outcome").is_none());
    let replayed: FindingReport = serde_json::from_value(old_shape).unwrap();
    assert_eq!(replayed.level(), None);
    assert_eq!(replayed.findings()[0].category(), None);
}

#[test]
fn optional_finding_dimensions_round_trip_with_reference_spellings() {
    let source =
        FindingReportSource::workspace(heycode_core::SessionId::from_raw("session-1"), 3, 0, None)
            .unwrap();
    let finding = reported(
        "finding-1",
        "src/lib.rs",
        &"d".repeat(64),
        "The verified error path drops the queued item.",
    )
    .unwrap()
    .with_reference_dimensions(
        Some("test-coverage".to_owned()),
        Some(FindingVerificationVerdict::Confirmed),
        Some(FindingOutcome::NoChangeNeeded),
    )
    .unwrap();
    let report = FindingReport::new(
        FindingReportId::new("report-dimensions").unwrap(),
        source,
        vec![finding],
    )
    .unwrap()
    .with_level(ReviewLevel::Xhigh);

    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["level"], "xhigh");
    assert_eq!(value["findings"][0]["category"], "test-coverage");
    assert_eq!(value["findings"][0]["verdict"], "CONFIRMED");
    assert_eq!(value["findings"][0]["outcome"], "no_change_needed");

    let replayed: FindingReport = serde_json::from_value(value).unwrap();
    assert_eq!(replayed.level(), Some(ReviewLevel::Xhigh));
    assert_eq!(replayed.findings()[0].category(), Some("test-coverage"));
    assert_eq!(
        replayed.findings()[0].verdict(),
        Some(FindingVerificationVerdict::Confirmed)
    );
    assert_eq!(
        replayed.findings()[0].outcome(),
        Some(FindingOutcome::NoChangeNeeded)
    );
}

#[test]
fn direct_finding_metadata_refuses_unsafe_paths_revisions_and_control_text() {
    let revision = "b".repeat(64);
    for path in [
        "../outside.rs",
        "/absolute.rs",
        "src\\lib.rs",
        "C:src.rs",
        "src/escape\u{1b}.rs",
    ] {
        assert!(reported("finding", path, &revision, "Concrete failure").is_err());
    }
    for malformed in [
        String::new(),
        "abc".to_owned(),
        "A".repeat(64),
        "0".repeat(63),
    ] {
        assert!(reported("finding", "src/lib.rs", &malformed, "Concrete failure").is_err());
    }
    assert!(
        reported(
            "finding",
            "src/lib.rs",
            &revision,
            "Terminal escape \u{1b}[31m",
        )
        .is_err()
    );
    assert!(
        FindingReportSource::workspace(
            heycode_core::SessionId::from_raw("session-1"),
            0,
            0,
            Some("../foreign".to_owned()),
        )
        .is_err()
    );
    for category in [
        "",
        "-correctness",
        "correctness-",
        "test--coverage",
        "Correctness",
        "test_coverage",
    ] {
        assert!(
            reported("finding", "src/lib.rs", &revision, "Concrete failure")
                .unwrap()
                .with_reference_dimensions(Some(category.to_owned()), None, None)
                .is_err()
        );
    }
    assert!(
        reported("finding", "src/lib.rs", &revision, "Concrete failure")
            .unwrap()
            .with_reference_dimensions(Some("a".repeat(41)), None, None)
            .is_err()
    );
}

#[test]
fn exact_duplicate_findings_fail_but_distinct_defects_can_share_a_location() {
    let source =
        FindingReportSource::workspace(heycode_core::SessionId::from_raw("session-1"), 0, 0, None)
            .unwrap();
    let revision = "c".repeat(64);
    let first = reported(
        "finding-1",
        "src/lib.rs",
        &revision,
        "The length subtraction underflows.",
    )
    .unwrap();
    let duplicate = reported(
        "finding-2",
        "src/lib.rs",
        &revision,
        "The length subtraction underflows.",
    )
    .unwrap()
    .with_reference_dimensions(
        Some("correctness".to_owned()),
        Some(FindingVerificationVerdict::Plausible),
        Some(FindingOutcome::Skipped),
    )
    .unwrap();
    assert!(
        FindingReport::new(
            FindingReportId::new("duplicates").unwrap(),
            source.clone(),
            vec![first.clone(), duplicate],
        )
        .is_err()
    );
    let distinct = reported(
        "finding-2",
        "src/lib.rs",
        &revision,
        "The error branch leaks the retained buffer.",
    )
    .unwrap();
    assert!(
        FindingReport::new(
            FindingReportId::new("distinct").unwrap(),
            source,
            vec![first, distinct],
        )
        .is_ok()
    );
}

#[test]
fn durable_domain_retains_legacy_128_finding_capacity() {
    let source =
        FindingReportSource::workspace(heycode_core::SessionId::from_raw("session-1"), 0, 0, None)
            .unwrap();
    let revision = "e".repeat(64);
    let findings = (0..128)
        .map(|index| {
            reported(
                &format!("finding-{index}"),
                "src/lib.rs",
                &revision,
                &format!("Distinct legacy failure {index}."),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        FindingReport::new(
            FindingReportId::new("legacy-128").unwrap(),
            source.clone(),
            findings.clone(),
        )
        .is_ok()
    );
    let mut too_many = findings;
    too_many.push(
        reported(
            "finding-128",
            "src/lib.rs",
            &revision,
            "Distinct legacy failure 128.",
        )
        .unwrap(),
    );
    assert!(
        FindingReport::new(
            FindingReportId::new("legacy-129").unwrap(),
            source,
            too_many,
        )
        .is_err()
    );
}
