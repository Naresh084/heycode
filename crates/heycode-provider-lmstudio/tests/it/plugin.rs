//! Composition contract of the LM Studio detection plugin.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_core::{Context, ContributionKind, CoreError, Plugin, compose};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::SERVICE_HTTP;
use heycode_provider_lmstudio::{
    LmStudioConfig, LmStudioDetector, LmStudioHealth, LmStudioModelControl, SERVICE_LM_STUDIO,
    SERVICE_LM_STUDIO_MODEL_CONTROL, lmstudio_plugin,
};
use tokio_util::sync::CancellationToken;

use super::support::{self, Outcome, ScriptedTransport};

struct HttpPlugin(Arc<ScriptedTransport>);

impl Plugin for HttpPlugin {
    fn name(&self) -> &'static str {
        "test-http"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_HTTP, self.name(), support::service(&self.0))
    }
}

struct CredentialsPlugin;

impl Plugin for CredentialsPlugin {
    fn name(&self) -> &'static str {
        "test-credentials"
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        context.provide(SERVICE_CREDENTIALS, self.name(), CredentialsService::new())
    }
}

fn transport() -> Arc<ScriptedTransport> {
    Arc::new(ScriptedTransport::new(Outcome::Refused))
}

#[tokio::test]
async fn the_plugin_publishes_a_usable_detector_under_its_service_key() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(HttpPlugin(transport())),
        lmstudio_plugin(LmStudioConfig::local()),
    ];
    let context = compose(&plugins).unwrap();
    // The bare value, not an `Arc<Arc<_>>`: a wrong shape would answer `None`
    // here rather than fail to compile (GOTCHAS #27).
    let detector = context.get::<LmStudioDetector>(SERVICE_LM_STUDIO).unwrap();
    assert!(
        context
            .get::<LmStudioModelControl>(SERVICE_LM_STUDIO_MODEL_CONTROL)
            .is_some()
    );
    let report = detector.detect(CancellationToken::new()).await;
    assert_eq!(report.health(), LmStudioHealth::NotRunning);
}

#[test]
fn the_plugin_declares_the_service_it_provides() {
    let plugin = lmstudio_plugin(LmStudioConfig::local());
    assert_eq!(
        plugin.provides(),
        &[SERVICE_LM_STUDIO, SERVICE_LM_STUDIO_MODEL_CONTROL]
    );
    assert_eq!(plugin.name(), "provider-lmstudio");
    assert_eq!(plugin.descriptor().id, plugin.name());
}

#[test]
fn the_service_row_is_recorded_once_in_the_shared_inventory() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(HttpPlugin(transport())),
        lmstudio_plugin(LmStudioConfig::local()),
    ];
    let context = compose(&plugins).unwrap();
    let rows: Vec<_> = context
        .plugin_inventory()
        .snapshot()
        .unwrap()
        .contributions
        .into_iter()
        .filter(|row| row.plugin == "provider-lmstudio" && row.kind == ContributionKind::Service)
        .collect();
    assert_eq!(rows.len(), 2);
    let mut names = rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, vec!["lmstudio", "lmstudio/model-control"]);
}

#[test]
fn the_default_posture_injects_only_http() {
    let plugin = lmstudio_plugin(LmStudioConfig::local());
    assert_eq!(plugin.inject(), &[SERVICE_HTTP]);
}

#[test]
fn a_configured_bearer_token_additionally_injects_credentials() {
    let plugin = lmstudio_plugin(LmStudioConfig::local().with_bearer_token(support::query()));
    assert_eq!(plugin.inject(), &[SERVICE_HTTP, SERVICE_CREDENTIALS]);
}

#[test]
fn a_bearer_token_plugin_composes_once_credentials_are_present() {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(HttpPlugin(transport())),
        Box::new(CredentialsPlugin),
        lmstudio_plugin(LmStudioConfig::local().with_bearer_token(support::query())),
    ];
    assert!(compose(&plugins).is_ok());
}

#[test]
fn composition_fails_loud_naming_a_missing_injected_service() {
    let plugins: Vec<Box<dyn Plugin>> = vec![lmstudio_plugin(LmStudioConfig::local())];
    let error = compose(&plugins).err().unwrap();
    assert!(error.to_string().contains("http"), "{error}");
}
