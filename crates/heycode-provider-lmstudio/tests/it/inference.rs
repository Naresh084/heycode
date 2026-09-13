//! Preparation refuses implicit model loading before inference admission.
#![allow(clippy::unwrap_used)]
use super::support::{self, Outcome, ScriptedTransport};
use heycode_llm::{ModelDescriptor, Provider, ProviderOptionContext};
use heycode_provider_lmstudio::{LmStudioConfig, LmStudioInference, LmStudioSurface};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn preparation_refuses_downloaded_only_models() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}"))
        .on(LmStudioSurface::OpenAiCompatible, Outcome::Json(r#"{"object":"list","data":[{"id":"local-model","object":"model"}]}"#))
        .on(LmStudioSurface::NativeRestV1, Outcome::Json(r#"{"models":[{"type":"llm","key":"local-model","display_name":"Local","loaded_instances":[],"capabilities":{"trained_for_tool_use":true}}]}"#)));
    let provider = LmStudioInference::new(
        support::service(&transport),
        None,
        LmStudioConfig::local(),
        "local-model",
    )
    .unwrap();
    let model = ModelDescriptor::unknown("local-model");
    let result = provider
        .prepare_inference(
            ProviderOptionContext::new(&model, &[]),
            CancellationToken::new(),
        )
        .await;
    assert!(
        result.err().unwrap().to_string().contains("load"),
        "selection must not trigger JIT loading"
    );
}

#[tokio::test]
async fn preparation_accepts_a_loaded_tool_model_with_observed_chat_surface() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Response(404, Some("application/json"), "{}"))
        .on(LmStudioSurface::OpenAiCompatible, Outcome::Json(r#"{"object":"list","data":[{"id":"local-model","object":"model"}]}"#))
        .on(LmStudioSurface::NativeRestV1, Outcome::Json(r#"{"models":[{"type":"llm","key":"local-model","display_name":"Local","loaded_instances":[{"id":"local-model","config":{}}],"capabilities":{"trained_for_tool_use":true}}]}"#)));
    let provider = LmStudioInference::new(
        support::service(&transport),
        None,
        LmStudioConfig::local(),
        "local-model",
    )
    .unwrap();
    let model = ModelDescriptor::unknown("local-model");
    assert!(
        provider
            .prepare_inference(
                ProviderOptionContext::new(&model, &[]),
                CancellationToken::new()
            )
            .await
            .is_ok()
    );
}
