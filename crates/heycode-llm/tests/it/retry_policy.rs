//! P08 stable provider error taxonomy and deterministic retry decisions.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, UNIX_EPOCH};

use heycode_http::{HttpErrorMetadata, HttpRetryAfter, TransportError};
use heycode_llm::{
    ProviderErrorClass, RetryAttempt, RetryDecision, RetryDelaySource, RetryJitter, RetrySafety,
    RetrySpec, RetryStopReason, classify_transport_error,
};

fn http_error(
    status: u16,
    body: &str,
    retry_after: Option<HttpRetryAfter>,
    should_retry: Option<bool>,
) -> TransportError {
    TransportError::http(
        status,
        body,
        HttpErrorMetadata::new(retry_after, should_retry),
    )
}

#[test]
fn transport_and_provider_shapes_map_to_stable_body_free_classes() {
    let cases = [
        (
            http_error(
                401,
                r#"{"error":{"type":"authentication_error","message":"secret-a"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::Authentication,
        ),
        (
            http_error(
                429,
                r#"{"error":{"type":"rate_limit_error","message":"secret-b"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::RateLimited,
        ),
        (
            http_error(
                529,
                r#"{"error":{"type":"overloaded_error","message":"secret-c"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::Overloaded,
        ),
        (
            http_error(
                500,
                r#"{"error":{"code":"server_error","message":"secret-d"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::Server,
        ),
        (
            http_error(
                504,
                r#"{"error":{"type":"timeout_error","message":"secret-e"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::Timeout,
        ),
        (
            http_error(
                400,
                r#"{"error":{"code":"context_length_exceeded","message":"secret-f"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::ContextWindowExceeded,
        ),
        (
            http_error(
                400,
                r#"{"error":{"type":"invalid_request_error","message":"secret-g"}}"#,
                None,
                None,
            ),
            ProviderErrorClass::InvalidRequest,
        ),
        (
            http_error(403, "{}", None, None),
            ProviderErrorClass::InvalidRequest,
        ),
        (
            http_error(408, "{}", None, None),
            ProviderErrorClass::Timeout,
        ),
        (
            http_error(409, "{}", None, None),
            ProviderErrorClass::Conflict,
        ),
        (
            http_error(413, "{}", None, None),
            ProviderErrorClass::Overflow,
        ),
        (
            http_error(422, "{}", None, None),
            ProviderErrorClass::InvalidRequest,
        ),
        (
            http_error(502, "{}", None, None),
            ProviderErrorClass::Server,
        ),
        (
            http_error(503, "{}", None, None),
            ProviderErrorClass::Overloaded,
        ),
        (
            TransportError::Network {
                message: "private network diagnostic".to_owned(),
            },
            ProviderErrorClass::Network,
        ),
        (TransportError::Timeout, ProviderErrorClass::Timeout),
        (
            TransportError::InvalidSse {
                message: "private framing diagnostic".to_owned(),
            },
            ProviderErrorClass::Protocol,
        ),
        (
            TransportError::ResponseTooLarge { max_bytes: 8 },
            ProviderErrorClass::Overflow,
        ),
        (TransportError::Cancelled, ProviderErrorClass::Cancelled),
    ];
    for (raw, expected) in cases {
        let error = classify_transport_error(raw);
        assert_eq!(error.class(), expected, "{error:?}");
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret-"), "{rendered}");
        assert!(!rendered.contains("private"), "{rendered}");
    }
}

#[test]
fn structured_error_code_is_bounded_safe_fact_not_diagnostic_text() {
    let error = classify_transport_error(http_error(
        400,
        r#"{"error":{"code":"context_length_exceeded","message":"do not echo me"}}"#,
        None,
        None,
    ));
    let failure = error.provider_failure().unwrap();
    assert_eq!(failure.code().unwrap().as_str(), "context_length_exceeded");
    assert!(!format!("{error:?} {error}").contains("do not echo me"));
}

#[test]
fn authoritative_http_status_cannot_be_overridden_by_a_contradictory_body_code() {
    for (status, code, expected) in [
        (401, "server_error", ProviderErrorClass::Authentication),
        (429, "authentication_error", ProviderErrorClass::RateLimited),
        (500, "invalid_request_error", ProviderErrorClass::Server),
        (
            503,
            "context_length_exceeded",
            ProviderErrorClass::Overloaded,
        ),
    ] {
        let body = format!(r#"{{"error":{{"code":"{code}"}}}}"#);
        let error = classify_transport_error(http_error(status, &body, None, None));
        assert_eq!(error.class(), expected, "status {status} with code {code}");
        assert_eq!(
            error
                .provider_failure()
                .and_then(|failure| failure.code())
                .map(heycode_llm::ProviderErrorCode::as_str),
            Some(code)
        );
    }
}

#[test]
fn retry_after_backoff_jitter_safety_and_terminal_stops_are_explicit() {
    let spec = RetrySpec::new(
        3,
        Duration::from_millis(100),
        Duration::from_secs(1),
        Duration::from_secs(10),
        RetryJitter::None,
        RetrySafety::StatelessPreOutput,
    )
    .unwrap();
    let rate = classify_transport_error(http_error(
        429,
        r#"{"error":{"type":"rate_limit_error"}}"#,
        Some(HttpRetryAfter::Delay(Duration::from_secs(5))),
        None,
    ));
    let attempt = RetryAttempt::new(1, false, 1_000, 77).unwrap();
    assert_eq!(
        spec.decide(&rate, attempt),
        RetryDecision::Retry {
            next_attempt: 2,
            delay: Duration::from_secs(5),
            source: RetryDelaySource::RetryAfter,
        }
    );

    let overloaded = classify_transport_error(http_error(503, "{}", None, None));
    assert_eq!(
        spec.decide(&overloaded, RetryAttempt::new(1, false, 1_000, 77).unwrap()),
        RetryDecision::Retry {
            next_attempt: 2,
            delay: Duration::from_millis(100),
            source: RetryDelaySource::ExponentialBackoff,
        }
    );
    assert_eq!(
        spec.decide(&overloaded, RetryAttempt::new(3, false, 1_000, 77).unwrap()),
        RetryDecision::DoNotRetry(RetryStopReason::AttemptsExhausted)
    );
    assert_eq!(
        spec.decide(&overloaded, RetryAttempt::new(1, true, 1_000, 77).unwrap()),
        RetryDecision::DoNotRetry(RetryStopReason::OutputAlreadyEmitted)
    );

    let too_long = classify_transport_error(http_error(
        429,
        "{}",
        Some(HttpRetryAfter::Delay(Duration::from_secs(11))),
        None,
    ));
    assert_eq!(
        spec.decide(&too_long, RetryAttempt::new(1, false, 1_000, 77).unwrap()),
        RetryDecision::DoNotRetry(RetryStopReason::RetryAfterExceedsLimit)
    );

    let dated = classify_transport_error(http_error(
        429,
        "{}",
        Some(HttpRetryAfter::At(
            UNIX_EPOCH + Duration::from_millis(6_000),
        )),
        None,
    ));
    assert_eq!(
        spec.decide(&dated, RetryAttempt::new(1, false, 1_000, 0).unwrap()),
        RetryDecision::Retry {
            next_attempt: 2,
            delay: Duration::from_secs(5),
            source: RetryDelaySource::RetryAfter,
        }
    );

    for retry_after in [
        HttpRetryAfter::Delay(Duration::ZERO),
        HttpRetryAfter::At(UNIX_EPOCH + Duration::from_millis(999)),
    ] {
        let immediate = classify_transport_error(http_error(429, "{}", Some(retry_after), None));
        assert_eq!(
            spec.decide(&immediate, RetryAttempt::new(1, false, 1_000, 0).unwrap()),
            RetryDecision::Retry {
                next_attempt: 2,
                delay: Duration::ZERO,
                source: RetryDelaySource::RetryAfter,
            }
        );
    }
}

#[test]
fn network_retry_requires_replay_safety_and_jitter_is_deterministic() {
    let network = classify_transport_error(TransportError::Network {
        message: "offline".to_owned(),
    });
    let definitive_only = RetrySpec::new(
        3,
        Duration::from_millis(100),
        Duration::from_secs(1),
        Duration::from_secs(10),
        RetryJitter::None,
        RetrySafety::DefinitiveFailuresOnly,
    )
    .unwrap();
    assert_eq!(
        definitive_only.decide(&network, RetryAttempt::new(1, false, 1_000, 25).unwrap()),
        RetryDecision::DoNotRetry(RetryStopReason::ReplaySafetyUnproven)
    );

    let jittered = RetrySpec::new(
        3,
        Duration::from_millis(100),
        Duration::from_secs(1),
        Duration::from_secs(10),
        RetryJitter::Full,
        RetrySafety::StatelessPreOutput,
    )
    .unwrap();
    assert_eq!(
        jittered.decide(&network, RetryAttempt::new(1, false, 1_000, 25).unwrap()),
        RetryDecision::Retry {
            next_attempt: 2,
            delay: Duration::from_millis(25),
            source: RetryDelaySource::ExponentialBackoffWithJitter,
        }
    );

    for raw in [
        classify_transport_error(TransportError::Cancelled),
        classify_transport_error(TransportError::ResponseTooLarge { max_bytes: 1 }),
        classify_transport_error(http_error(401, "{}", None, None)),
        classify_transport_error(http_error(
            400,
            r#"{"error":{"code":"context_length_exceeded"}}"#,
            None,
            None,
        )),
    ] {
        assert!(matches!(
            jittered.decide(&raw, RetryAttempt::new(1, false, 1_000, 25).unwrap()),
            RetryDecision::DoNotRetry(_)
        ));
    }
}

#[test]
fn explicit_provider_retry_veto_wins_over_transient_status() {
    let error = classify_transport_error(http_error(503, "{}", None, Some(false)));
    let spec = RetrySpec::standard();
    assert_eq!(
        spec.decide(&error, RetryAttempt::new(1, false, 1_000, 0).unwrap()),
        RetryDecision::DoNotRetry(RetryStopReason::ProviderVeto)
    );

    let approved = classify_transport_error(http_error(400, "{}", None, Some(true)));
    assert!(matches!(
        spec.decide(&approved, RetryAttempt::new(1, false, 1_000, 0).unwrap()),
        RetryDecision::Retry { .. }
    ));
}

#[test]
fn retry_configuration_rejects_unbounded_or_zero_identity_metadata() {
    for attempts in [0, 9] {
        assert!(
            RetrySpec::new(
                attempts,
                Duration::from_millis(1),
                Duration::from_secs(1),
                Duration::from_secs(10),
                RetryJitter::None,
                RetrySafety::StatelessPreOutput,
            )
            .is_err()
        );
    }
    assert!(
        RetrySpec::new(
            3,
            Duration::from_secs(2),
            Duration::from_secs(1),
            Duration::from_secs(10),
            RetryJitter::None,
            RetrySafety::StatelessPreOutput,
        )
        .is_err()
    );
    assert!(
        RetrySpec::new(
            3,
            Duration::from_millis(1),
            Duration::from_secs(1),
            Duration::from_secs(301),
            RetryJitter::None,
            RetrySafety::StatelessPreOutput,
        )
        .is_err()
    );
    assert!(RetryAttempt::new(0, false, 1, 1).is_err());
    assert!(
        RetrySpec::new(
            3,
            Duration::from_nanos(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            RetryJitter::None,
            RetrySafety::StatelessPreOutput,
        )
        .is_err()
    );
}

#[test]
fn actionable_provider_errors_keep_the_reason_without_echoing_response_content() {
    for (status, message, expected) in [
        (
            403,
            "This model requires you to complete the following before use: 18+ age confirmation. Confirm at https://openrouter.ai/settings/preferences. secret-canary",
            "18+ age confirmation",
        ),
        (
            402,
            "Insufficient credits secret-canary",
            "insufficient credits",
        ),
        (
            403,
            "Your data policy does not allow this model secret-canary",
            "data policy",
        ),
        (403, "Model access denied secret-canary", "denied access"),
        (404, "Model not found secret-canary", "model is unavailable"),
        (
            400,
            "Unsupported parameter secret-canary",
            "does not support",
        ),
        (401, "Invalid key secret-canary", "API key was rejected"),
    ] {
        let body = serde_json::json!({"error": {"code": status, "message": message}}).to_string();
        let error = classify_transport_error(http_error(status, &body, None, None));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(!format!("{error:?} {error}").contains("secret-canary"));
        assert_eq!(
            error.class(),
            if status == 401 {
                ProviderErrorClass::Authentication
            } else {
                ProviderErrorClass::InvalidRequest
            }
        );
    }
}

#[test]
fn provider_quota_and_parameter_errors_report_the_specific_recovery() {
    let quota = classify_transport_error(http_error(
        429,
        r#"{"error":{"code":"insufficient_quota","message":"private"}}"#,
        None,
        None,
    ));
    assert_eq!(quota.class(), ProviderErrorClass::InvalidRequest);
    assert!(quota.to_string().contains("credits"));
    let parameter = classify_transport_error(http_error(
        400,
        r#"{"error":{"message":"Unsupported parameter: temperature. private"}}"#,
        None,
        None,
    ));
    assert!(parameter.to_string().contains("`temperature`"));
    assert!(!parameter.to_string().contains("private"));
    let key = classify_transport_error(http_error(
        403,
        r#"{"error":{"code":"invalid_api_key","message":"private"}}"#,
        None,
        None,
    ));
    assert_eq!(key.class(), ProviderErrorClass::Authentication);
}
