//! Q08: what a live artifact refuses to carry, and what it withholds by default.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_live_artifact::{
    ArtifactFault, ArtifactRecorder, ContentPolicy, FailureClass, LiveOutcome, RouteId,
    ScreenedText,
};

fn route() -> RouteId {
    RouteId::new("openrouter", "z-ai/glm-5.3-flash").unwrap()
}

/// A credential in a response body is the exact material this row exists to
/// keep out of CI logs and dashboards.
const LEAKY_BODY: &str = r#"{"error":"bad key sk-proj-AAAAAAAAAAAAAAAAAAAAAAAA"}"#;

#[test]
fn content_is_withheld_by_default_and_the_artifact_records_that_it_was() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            Some("a perfectly ordinary response body"),
            &[],
        )
        .expect("withholding never fails on a body it drops");

    assert!(artifact.content().body().is_none());
    let json = serde_json::to_string(&artifact).unwrap();
    assert!(
        !json.contains("perfectly ordinary"),
        "a withheld body must not reach the file: {json}"
    );
    assert!(
        json.contains("withhold"),
        "a reader must be able to tell content was withheld, not absent: {json}"
    );
}

/// The two gates are independent. Asking for bodies is not asking to publish
/// your API key, and this is the test that says so.
#[test]
fn an_opted_in_body_carrying_a_credential_is_refused_outright() {
    let error = ArtifactRecorder::opted_in()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            Some(LEAKY_BODY),
            &[],
        )
        .expect_err("an opt-in must not defeat the credential screen");
    assert_eq!(error, ArtifactFault::CredentialMaterial);
}

/// Refusing beats redacting here: a partial artifact that silently dropped the
/// offending field would leave a reader believing they saw the whole body.
#[test]
fn a_refused_body_produces_no_artifact_at_all() {
    assert!(
        ArtifactRecorder::opted_in()
            .record(
                route(),
                LiveOutcome::Passed,
                1_700_000_000_000,
                None,
                Some(LEAKY_BODY),
                &[],
            )
            .is_err()
    );
}

/// Under the default policy the body is dropped without being screened, so a
/// leaky body cannot even fail the run — the cheapest way to not leak text is
/// to never look at it.
#[test]
fn a_withholding_recorder_drops_a_leaky_body_without_failing() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            Some(LEAKY_BODY),
            &[],
        )
        .expect("a dropped body is never screened");
    assert!(artifact.content().body().is_none());
    assert!(
        !serde_json::to_string(&artifact)
            .unwrap()
            .contains("sk-proj")
    );
}

/// Notes are free text a runner writes itself, so they are screened under every
/// policy — withholding covers the provider's body, not the runner's prose.
#[test]
fn notes_are_screened_even_when_content_is_withheld() {
    let error = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            None,
            &["retried with ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"],
        )
        .expect_err("a note must not smuggle a credential past the policy");
    assert_eq!(error, ArtifactFault::CredentialMaterial);
}

/// The screen is shared with S15 rather than reimplemented, so every format the
/// settings wire screen knows is refused here too.
#[test]
fn every_recognized_credential_format_is_refused() {
    for material in [
        "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAA",
        "ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AKIAIOSFODNN7EXAMPLE",
        "xoxb-AAAAAAAAAAAAAAAAAAAAAAAA",
        "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----",
    ] {
        assert_eq!(
            ScreenedText::screen(material).unwrap_err(),
            ArtifactFault::CredentialMaterial,
            "unscreened: {material}"
        );
        assert_eq!(
            ScreenedText::screen(format!("prefix {material} suffix")).unwrap_err(),
            ArtifactFault::CredentialMaterial,
            "unscreened when embedded: {material}"
        );
    }
}

/// Ordinary prose must still be recordable, or the screen is useless in
/// practice and someone turns it off.
#[test]
fn ordinary_text_passes_the_screen() {
    let text = ScreenedText::screen("route answered in 412ms with 3 tool calls").unwrap();
    assert_eq!(text.as_str(), "route answered in 412ms with 3 tool calls");
}

/// A failure classification is a closed set precisely so the provider's error
/// message — which routinely echoes the request, headers included — cannot ride
/// into the artifact behind it.
#[test]
fn a_failure_carries_a_class_and_never_a_message() {
    let artifact = ArtifactRecorder::withholding()
        .record(
            route(),
            LiveOutcome::Failed {
                class: FailureClass::Unauthorized,
            },
            1_700_000_000_000,
            None,
            None,
            &[],
        )
        .unwrap();
    let json = serde_json::to_string(&artifact).unwrap();
    assert!(json.contains("unauthorized"));
    assert!(!json.contains("sk-"), "{json}");
    assert_eq!(
        json.matches("class").count(),
        1,
        "the failure carries exactly one field: {json}"
    );
}

/// The policy that was in force is itself metadata: a reader comparing two runs
/// must be able to tell "no content recorded" from "content was not requested".
#[test]
fn the_policy_in_force_is_recorded_alongside_the_withheld_content() {
    let artifact = ArtifactRecorder::opted_in()
        .record(
            route(),
            LiveOutcome::Passed,
            1_700_000_000_000,
            None,
            None,
            &[],
        )
        .unwrap();
    let json = serde_json::to_string(&artifact).unwrap();
    assert!(
        json.contains("record_opted_in"),
        "an opted-in run that produced no body still records its policy: {json}"
    );
    assert_eq!(
        ArtifactRecorder::opted_in().policy(),
        ContentPolicy::RecordOptedIn
    );
    assert_eq!(ContentPolicy::default(), ContentPolicy::Withhold);
}

/// Found by the TEL02 lane reviewing Q08: `ScreenedText` was
/// `#[serde(transparent)]`, so a hand-authored artifact could carry a
/// credential straight past the screen on the *read* path. A constructor
/// invariant that deserialization can bypass is not an invariant.
#[test]
fn a_hand_authored_artifact_cannot_smuggle_a_credential_past_the_screen() {
    let smuggled = format!(
        r#"{{"schema_version":1,"route":{{"provider":"p","model":"m"}},
           "recorded_at_unix_ms":1,"outcome":{{"status":"passed"}},
           "content":{{"capture":"recorded","body":"{LEAKY}"}}}}"#,
        LEAKY = "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAA"
    );
    let parsed = heycode_live_artifact::LiveArtifact::from_json(&smuggled);
    assert!(
        parsed.is_err(),
        "reading must re-screen: {:?}",
        parsed.map(|artifact| artifact.content().body().map(ToOwned::to_owned))
    );
}
