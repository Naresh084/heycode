//! Detection verdicts. Every case drives an injected transport; none uses the
//! network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::ProviderProtocol;
use heycode_llm::CapabilitySupport;
use heycode_provider_lmstudio::{
    LM_STUDIO_NATIVE_REST_V1_RELEASE, LmStudioConfig, LmStudioCredentialState, LmStudioDetector,
    LmStudioHealth, LmStudioProbe, LmStudioRestApi, LmStudioServerReport, LmStudioSurface,
    LmStudioVersion,
};
use tokio_util::sync::CancellationToken;

use super::support::{
    self, NATIVE_V0_BODY, NATIVE_V1_BODY, OPENAI_BODY, Outcome, ScriptedTransport,
    UNKNOWN_PATH_BODY,
};

/// Run detection against a scripted transport with the default posture.
async fn detect(transport: Arc<ScriptedTransport>) -> LmStudioServerReport {
    LmStudioDetector::new(support::service(&transport), None, LmStudioConfig::local())
        .detect(CancellationToken::new())
        .await
}

/// A budget short enough that a hung probe settles inside one test.
fn brief() -> LmStudioConfig {
    LmStudioConfig::local()
        .with_timeout(Duration::from_millis(50))
        .expect("50ms is inside the accepted budget range")
}

/// Generous outer bound for a run whose own budget is milliseconds.
const OUTER_BOUND: Duration = Duration::from_secs(5);

/// Await one run under an outer bound.
///
/// A detector that lost its own timeout would otherwise spin forever against a
/// server that never answers, and a hung suite is indistinguishable from a slow
/// machine. This turns that failure into a named one.
async fn settle(detector: &LmStudioDetector) -> LmStudioServerReport {
    match tokio::time::timeout(OUTER_BOUND, detector.detect(CancellationToken::new())).await {
        Ok(report) => report,
        Err(_) => panic!("detection never settled within {OUTER_BOUND:?}: the probe is unbounded"),
    }
}

/// A complete, current LM Studio 0.4.x server: every documented surface answers
/// with its documented envelope.
fn current_server() -> Arc<ScriptedTransport> {
    Arc::new(
        ScriptedTransport::new(Outcome::Response(200, Some("text/plain"), ""))
            .on(LmStudioSurface::NativeRestV1, Outcome::Json(NATIVE_V1_BODY))
            .on(LmStudioSurface::NativeRestV0, Outcome::Json(NATIVE_V0_BODY))
            .on(
                LmStudioSurface::OpenAiCompatible,
                Outcome::Json(OPENAI_BODY),
            ),
    )
}

#[tokio::test]
async fn a_refused_connection_reports_not_running_instead_of_failing() {
    let report = detect(Arc::new(ScriptedTransport::new(Outcome::Refused))).await;
    assert_eq!(report.health(), LmStudioHealth::NotRunning);
    for observation in report.observations() {
        assert_eq!(observation.probe, LmStudioProbe::Unreachable);
    }
}

#[tokio::test]
async fn a_hung_server_is_indeterminate_and_never_reported_as_not_running() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Hang));
    let report = settle(&LmStudioDetector::new(
        support::service(&transport),
        None,
        brief(),
    ))
    .await;
    assert_eq!(report.health(), LmStudioHealth::Indeterminate);
    for observation in report.observations() {
        assert_eq!(observation.probe, LmStudioProbe::Indeterminate);
    }
}

#[tokio::test]
async fn the_detection_budget_is_shared_by_the_whole_run_not_granted_per_probe() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Hang));
    let started = std::time::Instant::now();
    settle(&LmStudioDetector::new(
        support::service(&transport),
        None,
        brief(),
    ))
    .await;
    assert_eq!(
        transport.requests().len(),
        1,
        "the first probe consumed the whole budget, so no later probe may be sent"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "three probes must not each get their own budget"
    );
}

#[tokio::test]
async fn detection_stays_bounded_against_a_server_that_never_answers() {
    // The load-bearing local-first rule: a hung LM Studio must not hang heycode.
    // Pinned as its own contract, because losing the probe's timeout would make
    // every other hang case wait forever rather than fail.
    let transport = Arc::new(ScriptedTransport::new(Outcome::Hang));
    let started = std::time::Instant::now();
    let report = settle(&LmStudioDetector::new(
        support::service(&transport),
        None,
        brief(),
    ))
    .await;
    assert!(
        started.elapsed() < OUTER_BOUND,
        "detection must settle under its own budget, not the outer bound"
    );
    assert_eq!(report.health(), LmStudioHealth::Indeterminate);
}

#[tokio::test]
async fn a_two_hundred_on_an_unrouted_path_is_unrecognized_not_recognized() {
    // LM Studio answers `200` on paths it does not route, so a status alone is
    // never evidence: bug-tracker issue 1323.
    let report = detect(Arc::new(ScriptedTransport::new(Outcome::Json(
        UNKNOWN_PATH_BODY,
    ))))
    .await;
    assert_eq!(report.health(), LmStudioHealth::RunningUnrecognized);
    assert_eq!(report.rest_api(), LmStudioRestApi::Unknown);
    assert_eq!(report.version(), LmStudioVersion::Unknown);
    assert!(!report.identified_lm_studio());
    assert_eq!(
        report.protocol(ProviderProtocol::OpenAiChatCompletions),
        CapabilitySupport::Unknown
    );
}

#[tokio::test]
async fn a_recognized_native_v1_list_bounds_the_version_at_its_published_release() {
    let report = detect(current_server()).await;
    assert_eq!(report.rest_api(), LmStudioRestApi::V1);
    assert_eq!(
        report.version(),
        LmStudioVersion::AtLeast(LM_STUDIO_NATIVE_REST_V1_RELEASE)
    );
    assert_eq!(LM_STUDIO_NATIVE_REST_V1_RELEASE, "0.4.0");
}

#[tokio::test]
async fn a_recognized_native_v0_list_alone_proves_no_application_version() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Json(UNKNOWN_PATH_BODY))
            .on(LmStudioSurface::NativeRestV0, Outcome::Json(NATIVE_V0_BODY)),
    );
    let report = detect(transport).await;
    assert_eq!(report.rest_api(), LmStudioRestApi::V0);
    assert_eq!(report.version(), LmStudioVersion::Unknown);
    assert!(report.identified_lm_studio());
}

#[tokio::test]
async fn each_surface_is_recognized_only_by_its_own_documented_envelope() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Json(UNKNOWN_PATH_BODY))
            // v0's envelope served on the v1 path, and the reverse.
            .on(LmStudioSurface::NativeRestV1, Outcome::Json(NATIVE_V0_BODY))
            .on(LmStudioSurface::NativeRestV0, Outcome::Json(NATIVE_V1_BODY)),
    );
    let report = detect(transport).await;
    assert_eq!(
        report.observation(LmStudioSurface::NativeRestV1),
        Some(LmStudioProbe::Unrecognized)
    );
    assert_eq!(
        report.observation(LmStudioSurface::NativeRestV0),
        Some(LmStudioProbe::Unrecognized)
    );
    assert_eq!(report.rest_api(), LmStudioRestApi::Unknown);
}

#[tokio::test]
async fn a_non_json_body_on_a_documented_path_is_unrecognized() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Response(
        200,
        Some("text/html"),
        NATIVE_V1_BODY,
    )));
    let report = detect(transport).await;
    assert_eq!(
        report.observation(LmStudioSurface::NativeRestV1),
        Some(LmStudioProbe::Unrecognized)
    );
    assert_eq!(report.health(), LmStudioHealth::RunningUnrecognized);
}

#[tokio::test]
async fn an_openai_compatible_list_alone_never_identifies_lm_studio() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}")).on(
            LmStudioSurface::OpenAiCompatible,
            Outcome::Json(OPENAI_BODY),
        ),
    );
    let report = detect(transport).await;
    assert_eq!(report.health(), LmStudioHealth::Running);
    assert!(
        !report.identified_lm_studio(),
        "many local runtimes serve an OpenAI-compatible model list"
    );
    assert_eq!(report.rest_api(), LmStudioRestApi::Unknown);
    assert_eq!(report.version(), LmStudioVersion::Unknown);
    assert_eq!(
        report.protocol(ProviderProtocol::OpenAiChatCompletions),
        CapabilitySupport::Supported
    );
}

#[tokio::test]
async fn a_post_only_protocol_with_no_observable_surface_stays_unknown() {
    let report = detect(current_server()).await;
    assert_eq!(
        report.protocol(ProviderProtocol::OpenAiResponses),
        CapabilitySupport::Unknown,
        "`POST /v1/responses` has no GET surface, so a complete v1 server still proves nothing"
    );
    assert_eq!(
        report.protocol(ProviderProtocol::AnthropicMessages),
        CapabilitySupport::Unknown,
        "`POST /v1/messages` has no GET surface"
    );
}

#[tokio::test]
async fn a_supported_protocol_names_the_surface_that_proved_it() {
    let report = detect(current_server()).await;
    let row = report
        .protocols()
        .iter()
        .find(|row| row.protocol == ProviderProtocol::OpenAiChatCompletions)
        .unwrap();
    assert_eq!(row.support, CapabilitySupport::Supported);
    assert_eq!(row.evidence, Some(LmStudioSurface::OpenAiCompatible));
}

#[tokio::test]
async fn an_unproved_protocol_carries_no_evidence() {
    let report = detect(Arc::new(ScriptedTransport::new(Outcome::Refused))).await;
    for row in report.protocols() {
        assert_eq!(row.support, CapabilitySupport::Unknown);
        assert_eq!(row.evidence, None, "{:?} must cite nothing", row.protocol);
    }
}

#[tokio::test]
async fn no_protocol_is_ever_reported_unsupported() {
    // A probe can prove a surface is present; it can never prove one is absent,
    // so `Unsupported` is not a verdict this crate may reach.
    let transports = [
        Arc::new(ScriptedTransport::new(Outcome::Refused)),
        Arc::new(ScriptedTransport::new(Outcome::Json(UNKNOWN_PATH_BODY))),
        Arc::new(ScriptedTransport::new(Outcome::Response(
            404,
            Some("application/json"),
            "{}",
        ))),
        Arc::new(ScriptedTransport::new(Outcome::Response(
            401,
            Some("application/json"),
            "{}",
        ))),
        current_server(),
    ];
    for transport in transports {
        let report = detect(transport).await;
        for row in report.protocols() {
            assert_ne!(
                row.support,
                CapabilitySupport::Unsupported,
                "{:?} must never be Unsupported",
                row.protocol
            );
        }
    }
}

#[tokio::test]
async fn the_provider_descriptor_lists_only_proved_protocols() {
    let descriptor = detect(current_server()).await.provider_descriptor();
    assert_eq!(descriptor.id, "lmstudio");
    assert_eq!(descriptor.display_name, "LM Studio");
    assert_eq!(
        descriptor.protocols,
        vec![ProviderProtocol::OpenAiChatCompletions]
    );
}

#[tokio::test]
async fn the_provider_descriptor_reports_unknown_when_nothing_was_proved() {
    let descriptor = detect(Arc::new(ScriptedTransport::new(Outcome::Refused)))
        .await
        .provider_descriptor();
    assert_eq!(descriptor.protocols, vec![ProviderProtocol::Unknown]);
}

#[tokio::test]
async fn an_accepted_unauthenticated_probe_proves_no_credential_is_required() {
    let report = detect(current_server()).await;
    assert_eq!(report.credential(), LmStudioCredentialState::NotRequired);
}

#[tokio::test]
async fn a_rejected_probe_reports_a_required_credential_even_when_another_surface_answered() {
    let transport = Arc::new(
        ScriptedTransport::new(Outcome::Response(401, Some("application/json"), "{}")).on(
            LmStudioSurface::OpenAiCompatible,
            Outcome::Json(OPENAI_BODY),
        ),
    );
    let report = detect(transport).await;
    assert_eq!(report.credential(), LmStudioCredentialState::Required);
    assert_eq!(report.health(), LmStudioHealth::Running);
}

#[tokio::test]
async fn an_accepted_authenticated_probe_is_not_reported_as_no_credential_required() {
    let transport = current_server();
    let (_context, credentials) = support::credentials(Some("lmstudio-secret-token"));
    let report = LmStudioDetector::new(
        support::service(&transport),
        Some(credentials),
        LmStudioConfig::local().with_bearer_token(support::query()),
    )
    .detect(CancellationToken::new())
    .await;
    assert_eq!(
        report.credential(),
        LmStudioCredentialState::Accepted,
        "a token was sent, so acceptance does not prove the token was unnecessary"
    );
}

#[tokio::test]
async fn a_configured_bearer_token_is_sent_as_an_authorization_header() {
    let transport = current_server();
    let (_context, credentials) = support::credentials(Some("lmstudio-secret-token"));
    LmStudioDetector::new(
        support::service(&transport),
        Some(credentials),
        LmStudioConfig::local().with_bearer_token(support::query()),
    )
    .detect(CancellationToken::new())
    .await;
    let requests = transport.requests();
    assert_eq!(requests.len(), LmStudioSurface::ALL.len());
    for request in requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer lmstudio-secret-token")
        );
    }
}

#[tokio::test]
async fn a_bearer_query_that_resolves_to_nothing_falls_back_to_an_unauthenticated_probe() {
    let transport = current_server();
    let (_context, credentials) = support::credentials(None);
    let report = LmStudioDetector::new(
        support::service(&transport),
        Some(credentials),
        LmStudioConfig::local().with_bearer_token(support::query()),
    )
    .detect(CancellationToken::new())
    .await;
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.authorization.is_none())
    );
    assert_eq!(report.credential(), LmStudioCredentialState::NotRequired);
}

#[tokio::test]
async fn a_resolved_bearer_token_never_reaches_debug_output() {
    let transport = current_server();
    let (_context, credentials) = support::credentials(Some("lmstudio-secret-token"));
    let detector = LmStudioDetector::new(
        support::service(&transport),
        Some(credentials),
        LmStudioConfig::local().with_bearer_token(support::query()),
    );
    let report = detector.detect(CancellationToken::new()).await;
    let rendered = format!("{detector:?} {report:?}");
    assert!(!rendered.contains("lmstudio-secret-token"), "{rendered}");
}

#[tokio::test]
async fn cancellation_is_indeterminate_and_never_an_unreachable_server() {
    let transport = current_server();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let report = LmStudioDetector::new(support::service(&transport), None, LmStudioConfig::local())
        .detect(cancellation)
        .await;
    assert_eq!(report.health(), LmStudioHealth::Indeterminate);
    assert!(transport.requests().is_empty());
    for observation in report.observations() {
        assert_eq!(observation.probe, LmStudioProbe::Indeterminate);
    }
}

#[tokio::test]
async fn every_documented_surface_is_probed_at_its_own_url() {
    let transport = current_server();
    detect(transport.clone()).await;
    let urls: Vec<String> = transport
        .requests()
        .into_iter()
        .map(|request| request.url)
        .collect();
    assert_eq!(
        urls,
        vec![
            support::url(LmStudioSurface::NativeRestV1),
            support::url(LmStudioSurface::NativeRestV0),
            support::url(LmStudioSurface::OpenAiCompatible),
        ]
    );
}
