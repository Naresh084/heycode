//! PLM04: explicit load/unload settings and no-surprise-load boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_http::HttpMethod;
use heycode_llm::CapabilitySupport;
use heycode_provider_lmstudio::{
    LmStudioControlError, LmStudioLoadPlan, LmStudioLoadSettings, LmStudioModelControl,
    LmStudioModelKind, LmStudioModelRecord, LmStudioModelState, LmStudioUnloadPlan,
};
use tokio_util::sync::CancellationToken;

use super::support::{Outcome, ScriptedTransport, service};

fn record(loaded: bool) -> LmStudioModelRecord {
    LmStudioModelRecord {
        key: "openai/gpt-oss-20b".to_owned(),
        display_name: "GPT OSS 20B".to_owned(),
        kind: LmStudioModelKind::Llm,
        publisher: Some("openai".to_owned()),
        architecture: Some("gpt_oss".to_owned()),
        quantization: None,
        size_bytes: None,
        params_string: Some("20B".to_owned()),
        format: Some("gguf".to_owned()),
        max_context_length: Some(131_072),
        loaded_instances: loaded
            .then(|| heycode_provider_lmstudio::LmStudioLoadedInstance {
                id: "gpt-oss-explicit".to_owned(),
                context_length: Some(16_384),
            })
            .into_iter()
            .collect(),
        tool_trained: CapabilitySupport::Supported,
        vision: CapabilitySupport::Unsupported,
        reasoning: CapabilitySupport::Supported,
        reasoning_options: vec!["low".to_owned(), "high".to_owned()],
    }
}

#[test]
fn planning_and_route_selection_never_load_a_downloaded_model() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Refused));
    let control = LmStudioModelControl::new(
        service(&transport),
        None,
        heycode_provider_lmstudio::LmStudioConfig::local(),
    );
    let downloaded = record(false);
    let plan = control
        .prepare_load(&downloaded, LmStudioLoadSettings::new())
        .unwrap();
    assert_eq!(plan.model(), "openai/gpt-oss-20b");
    assert!(transport.requests().is_empty());
    assert_eq!(downloaded.state(), LmStudioModelState::Downloaded);
    assert_eq!(
        control.require_loaded(&downloaded),
        Err(LmStudioControlError::ExplicitLoadRequired)
    );
    assert!(transport.requests().is_empty());

    let loaded = record(true);
    assert_eq!(
        control.require_loaded(&loaded).unwrap()[0].id,
        "gpt-oss-explicit"
    );
}

#[tokio::test]
async fn pre_cancelled_load_performs_no_request() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Refused));
    let control = LmStudioModelControl::new(
        service(&transport),
        None,
        heycode_provider_lmstudio::LmStudioConfig::local(),
    );
    let plan = control
        .prepare_load(&record(false), LmStudioLoadSettings::new())
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        control.load(plan, cancellation).await,
        Err(LmStudioControlError::Cancelled)
    );
    assert!(transport.requests().is_empty());
}

#[tokio::test]
async fn an_explicit_load_sends_exact_settings_and_verifies_the_echoed_configuration() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Json(
        r#"{
          "type":"llm","instance_id":"gpt-oss-explicit",
          "load_time_seconds":9.099,"status":"loaded",
          "load_config":{
            "context_length":16384,"eval_batch_size":512,
            "flash_attention":true,"num_experts":4,
            "offload_kv_cache_to_gpu":true
          }}"#,
    )));
    let control = LmStudioModelControl::new(
        service(&transport),
        None,
        heycode_provider_lmstudio::LmStudioConfig::local(),
    );
    let settings = LmStudioLoadSettings::new()
        .with_context_length(16_384)
        .unwrap()
        .with_eval_batch_size(512)
        .unwrap()
        .with_flash_attention(true)
        .with_num_experts(4)
        .unwrap()
        .with_offload_kv_cache_to_gpu(true);
    let plan = control.prepare_load(&record(false), settings).unwrap();
    let receipt = control.load(plan, CancellationToken::new()).await.unwrap();
    assert_eq!(receipt.instance_id(), "gpt-oss-explicit");
    assert_eq!(receipt.kind(), LmStudioModelKind::Llm);
    assert_eq!(receipt.load_time_seconds(), 9.099);
    assert_eq!(receipt.settings().context_length(), Some(16_384));

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[0].url, "http://localhost:1234/api/v1/models/load");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "model":"openai/gpt-oss-20b",
            "context_length":16384,
            "eval_batch_size":512,
            "flash_attention":true,
            "num_experts":4,
            "offload_kv_cache_to_gpu":true,
            "echo_load_config":true
        })
    );
}

#[test]
fn invalid_or_overlarge_explicit_settings_fail_before_a_plan_exists() {
    assert_eq!(
        LmStudioLoadSettings::new().with_context_length(0),
        Err(LmStudioControlError::InvalidSettings)
    );
    assert_eq!(
        LmStudioLoadSettings::new().with_eval_batch_size(0),
        Err(LmStudioControlError::InvalidSettings)
    );
    assert_eq!(
        LmStudioLoadSettings::new().with_num_experts(0),
        Err(LmStudioControlError::InvalidSettings)
    );
    let settings = LmStudioLoadSettings::new()
        .with_context_length(262_144)
        .unwrap();
    assert_eq!(
        LmStudioLoadPlan::prepare(&record(false), settings),
        Err(LmStudioControlError::ContextExceedsModel)
    );
}

#[tokio::test]
async fn unload_requires_and_echoes_one_explicit_instance_id() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Json(
        r#"{"instance_id":"gpt-oss-explicit"}"#,
    )));
    let control = LmStudioModelControl::new(
        service(&transport),
        None,
        heycode_provider_lmstudio::LmStudioConfig::local(),
    );
    let plan = LmStudioUnloadPlan::new("gpt-oss-explicit").unwrap();
    let receipt = control
        .unload(plan, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(receipt.instance_id(), "gpt-oss-explicit");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(
        requests[0].url,
        "http://localhost:1234/api/v1/models/unload"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap(),
        serde_json::json!({"instance_id":"gpt-oss-explicit"})
    );
}

#[tokio::test]
async fn a_mismatched_load_echo_is_refused_instead_of_publishing_false_settings() {
    let transport = Arc::new(ScriptedTransport::new(Outcome::Json(
        r#"{
          "type":"llm","instance_id":"gpt-oss-explicit",
          "load_time_seconds":1.0,"status":"loaded",
          "load_config":{"context_length":4096}}
        "#,
    )));
    let control = LmStudioModelControl::new(
        service(&transport),
        None,
        heycode_provider_lmstudio::LmStudioConfig::local(),
    );
    let settings = LmStudioLoadSettings::new()
        .with_context_length(16_384)
        .unwrap();
    let plan = control.prepare_load(&record(false), settings).unwrap();
    assert_eq!(
        control.load(plan, CancellationToken::new()).await,
        Err(LmStudioControlError::SettingsMismatch)
    );
}
