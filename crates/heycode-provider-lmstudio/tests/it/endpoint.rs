//! Documented endpoint constants, URL derivation and configuration bounds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use heycode_provider_lmstudio::{
    LM_STUDIO_DEFAULT_BASE_URL, LM_STUDIO_MAX_TIMEOUT, LmStudioAuth, LmStudioConfig,
    LmStudioConfigError, LmStudioEndpoint, LmStudioSurface,
};

use super::support;

#[test]
fn the_default_origin_is_the_documented_localhost_1234_server() {
    assert_eq!(LM_STUDIO_DEFAULT_BASE_URL, "http://localhost:1234");
}

#[test]
fn every_surface_probes_its_documented_model_list_path() {
    assert_eq!(LmStudioSurface::NativeRestV1.path(), "/api/v1/models");
    assert_eq!(LmStudioSurface::NativeRestV0.path(), "/api/v0/models");
    assert_eq!(LmStudioSurface::OpenAiCompatible.path(), "/v1/models");
}

#[test]
fn surface_diagnostic_ids_are_stable() {
    assert_eq!(LmStudioSurface::NativeRestV1.as_str(), "native-rest-v1");
    assert_eq!(LmStudioSurface::NativeRestV0.as_str(), "native-rest-v0");
    assert_eq!(
        LmStudioSurface::OpenAiCompatible.as_str(),
        "openai-compatible"
    );
}

#[test]
fn probe_urls_join_the_origin_to_the_documented_paths() {
    let endpoint = LmStudioEndpoint::local();
    assert_eq!(
        endpoint.url(LmStudioSurface::NativeRestV1),
        "http://localhost:1234/api/v1/models"
    );
    assert_eq!(
        endpoint.url(LmStudioSurface::NativeRestV0),
        "http://localhost:1234/api/v0/models"
    );
    assert_eq!(
        endpoint.url(LmStudioSurface::OpenAiCompatible),
        "http://localhost:1234/v1/models"
    );
}

#[test]
fn a_trailing_slash_on_the_origin_does_not_double_the_path_separator() {
    let endpoint = LmStudioEndpoint::new("http://127.0.0.1:4321/").unwrap();
    assert_eq!(endpoint.base_url(), "http://127.0.0.1:4321");
    assert_eq!(
        endpoint.url(LmStudioSurface::NativeRestV1),
        "http://127.0.0.1:4321/api/v1/models"
    );
}

#[test]
fn an_origin_that_is_not_host_qualified_http_is_rejected() {
    for base_url in [
        "",
        "localhost:1234",
        "ftp://localhost:1234",
        "http://user:token@localhost:1234",
    ] {
        assert_eq!(
            LmStudioEndpoint::new(base_url),
            Err(LmStudioConfigError::BaseUrl),
            "{base_url} must be rejected"
        );
    }
}

#[test]
fn a_rejected_origin_never_echoes_the_configured_url() {
    let error = LmStudioEndpoint::new("http://user:token@localhost:1234").unwrap_err();
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("token"), "{rendered}");
    assert!(!rendered.contains("localhost"), "{rendered}");
}

#[test]
fn a_detection_budget_outside_its_bounds_is_rejected_not_clamped() {
    let config = LmStudioConfig::local();
    assert_eq!(
        config.clone().with_timeout(Duration::ZERO),
        Err(LmStudioConfigError::Timeout)
    );
    assert_eq!(
        config
            .clone()
            .with_timeout(LM_STUDIO_MAX_TIMEOUT + Duration::from_millis(1)),
        Err(LmStudioConfigError::Timeout)
    );
    assert_eq!(
        config
            .with_timeout(LM_STUDIO_MAX_TIMEOUT)
            .unwrap()
            .timeout(),
        LM_STUDIO_MAX_TIMEOUT
    );
}

#[test]
fn the_documented_default_posture_requires_no_credential() {
    assert_eq!(LmStudioConfig::local().auth(), &LmStudioAuth::None);
}

#[test]
fn a_configured_bearer_token_replaces_the_no_credential_posture() {
    let config = LmStudioConfig::local().with_bearer_token(support::query());
    assert_eq!(
        config.auth(),
        &LmStudioAuth::BearerToken(support::query()),
        "an explicitly configured token must be visible in the posture"
    );
}
