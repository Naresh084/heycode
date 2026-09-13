//! Q08: the metadata and outcome an artifact must carry, and versioning.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use heycode_live_artifact::{
    ARTIFACT_SCHEMA_VERSION, ArtifactFault, ArtifactRecorder, LiveArtifact, LiveOutcome, RouteId,
    SkipReason,
};

fn route() -> RouteId {
    RouteId::new("anthropic", "claude-opus-5").unwrap()
}

#[test]
fn an_artifact_captures_route_outcome_and_latency() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            Some(Duration::from_millis(412)),
            None,
            &["three tool calls"],
        )
        .unwrap();

    assert_eq!(artifact.route().provider(), "anthropic");
    assert_eq!(artifact.route().model(), "claude-opus-5");
    assert!(artifact.outcome().passed());
    assert_eq!(artifact.latency_ms(), Some(412));
    assert_eq!(artifact.notes().len(), 1);
    assert_eq!(artifact.schema_version(), ARTIFACT_SCHEMA_VERSION);
}

/// An unmeasured latency is absent, not zero — the same rule the usage stack
/// keeps. A dashboard averaging in a fabricated 0ms would report a lie.
#[test]
fn an_unmeasured_latency_is_absent_rather_than_zero() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            None,
            &[],
        )
        .unwrap();
    assert_eq!(artifact.latency_ms(), None);
    let json = serde_json::to_string(&artifact).unwrap();
    assert!(
        !json.contains("latency_ms"),
        "an absent measurement must not appear at all: {json}"
    );
}

#[test]
fn a_skipped_exercise_records_why_it_did_not_run() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Skipped {
                reason: SkipReason::NoCredential,
            },
            1_700_000_000_000,
            None,
            None,
            &[],
        )
        .unwrap();
    assert!(!artifact.outcome().passed());
    let json = serde_json::to_string(&artifact).unwrap();
    assert!(json.contains("no_credential"), "{json}");
}

#[test]
fn an_artifact_round_trips_through_json() {
    let artifact = ArtifactRecorder::opted_in()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            Some(Duration::from_millis(9)),
            Some("hello from the model"),
            &["note"],
        )
        .unwrap();
    let json = serde_json::to_string(&artifact).unwrap();
    let parsed = LiveArtifact::from_json(&json).expect("round trip");
    assert_eq!(parsed, artifact);
    assert_eq!(parsed.content().body(), Some("hello from the model"));
}

#[test]
fn freshness_rejects_expired_and_future_dated_artifacts() {
    let artifact = ArtifactRecorder::withholding()
        .record(route(), LiveOutcome::Passed, 1_000, None, None, &[])
        .unwrap();
    assert_eq!(artifact.recorded_at_unix_ms(), 1_000);
    assert!(artifact.is_fresh_at(1_999, Duration::from_millis(999)));
    assert!(!artifact.is_fresh_at(2_000, Duration::from_millis(999)));
    assert!(!artifact.is_fresh_at(999, Duration::from_secs(10)));
}

/// A reader that cannot understand the version must refuse the file rather than
/// interpret fields that may have changed meaning.
#[test]
fn an_unreadable_schema_version_is_refused_not_guessed() {
    let json = r#"{"schema_version":99,"route":{"provider":"a","model":"b"},
        "recorded_at_unix_ms":1,"outcome":{"status":"passed"},
        "content":{"capture":"withheld","policy":"withhold"}}"#;
    let error = LiveArtifact::from_json(json).expect_err("a future version must be refused");
    assert!(error.to_string().contains("99"), "{error}");
}

#[test]
fn a_route_refuses_empty_and_control_bearing_fields() {
    for (provider, model) in [
        ("", "m"),
        ("   ", "m"),
        ("p", ""),
        ("p\u{0}", "m"),
        ("p", "m\n"),
    ] {
        assert_eq!(
            RouteId::new(provider, model).unwrap_err(),
            ArtifactFault::MalformedRoute,
            "accepted {provider:?}/{model:?}"
        );
    }
    assert!(RouteId::new("openai", "gpt-5").is_ok());
}
