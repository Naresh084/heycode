//! PGCP07 Claude on Google Cloud profile and evidence boundary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_authorization_gcp::testing::MapGcpEnvironment;
use heycode_authorization_gcp::{
    ENV_GOOGLE_APPLICATION_CREDENTIALS, GcpAuthProfile, GcpAuthService, GcpHealth, GcpHostPlatform,
    GcpMetadataPolicy, GcpProfileRequest,
};
use heycode_credentials::{CredentialKind, CredentialQuery, CredentialReference};
use heycode_http::{HttpService, HttpSseRequest, HttpTransport, SseEvent, SseEventStream};
use heycode_llm::{
    CallPurpose, CapabilitySupport, ChatMessage, InferenceEvent, InferenceInput, InputModality,
    ModelLifecycleStatus, Provider, ReasoningEffortId, RequestDraft, ResolveError, ToolSpec,
};
use heycode_provider_google::{
    CLAUDE_VERTEX_ANTHROPIC_VERSION, CLAUDE_VERTEX_DEFAULT_MODEL, CLAUDE_VERTEX_OAUTH_SCOPE,
    CLAUDE_VERTEX_PROVIDER, ClaudeVertexControls, ClaudeVertexEffort, ClaudeVertexError,
    ClaudeVertexProfile, ClaudeVertexProvider, ClaudeVertexThinking,
    GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE,
};
use tokio_util::sync::CancellationToken;

use super::support::{TEST_SECRET, credentials};

fn token_query() -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE).unwrap(),
        CredentialKind::new("oauth-token").unwrap(),
    )
}

async fn auth_profile(project: Option<&str>, location: Option<&str>) -> GcpAuthProfile {
    let adc_path = "/fixture/application_default_credentials.json";
    let environment = MapGcpEnvironment::new()
        .with_var(ENV_GOOGLE_APPLICATION_CREDENTIALS, adc_path)
        .with_file(adc_path, br#"{"type":"authorized_user"}"#.to_vec());
    let service = GcpAuthService::new(
        Arc::new(environment),
        HttpService::new(Arc::new(ScriptedSse::new(Vec::new()))),
    );
    service
        .resolve(
            GcpProfileRequest {
                project: project.map(str::to_owned),
                location: location.map(str::to_owned),
                platform: GcpHostPlatform::Unix,
                metadata: GcpMetadataPolicy::Disabled,
            },
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn the_profile_keeps_configured_adc_distinct_from_live_authentication() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    assert_eq!(profile.auth_preflight(), GcpHealth::Unknown);
    assert!(!profile.project_confirmed());
    assert_eq!(profile.project(), "vertex-fixture");
    assert_eq!(profile.location(), "global");
    assert_eq!(
        CLAUDE_VERTEX_OAUTH_SCOPE,
        "https://www.googleapis.com/auth/cloud-platform"
    );
    assert_eq!(
        profile.endpoint(),
        "https://aiplatform.googleapis.com/v1/projects/vertex-fixture/locations/global/\
publishers/anthropic/models/claude-sonnet-5:streamRawPredict"
    );

    let safe = profile.provider_profile();
    assert_eq!(safe.registry_name, CLAUDE_VERTEX_PROVIDER);
    assert_eq!(safe.descriptor.id, CLAUDE_VERTEX_PROVIDER);
    assert_eq!(safe.default_model, CLAUDE_VERTEX_DEFAULT_MODEL);
    assert_eq!(
        safe.credential_reference.as_deref(),
        Some(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE)
    );
}

#[tokio::test]
async fn the_maintained_sonnet_model_uses_only_current_primary_source_facts() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let model = profile.model();
    assert_eq!(model.id, CLAUDE_VERTEX_DEFAULT_MODEL);
    assert_eq!(model.display_name, "Claude Sonnet 5");
    assert_eq!(model.context_window, Some(1_000_000));
    assert_eq!(model.max_output_tokens, Some(128_000));
    assert_eq!(model.lifecycle.status, ModelLifecycleStatus::Stable);
    assert_eq!(model.capabilities.tools, CapabilitySupport::Supported);
    assert_eq!(model.capabilities.reasoning, CapabilitySupport::Supported);
    assert_eq!(
        model.capabilities.prompt_cache,
        CapabilitySupport::Supported
    );
    assert_eq!(model.capabilities.image_input, CapabilitySupport::Supported);
    assert_eq!(
        model.capabilities.document_input,
        CapabilitySupport::Supported
    );
    // The model card's provider-hosted web feature is not enough to make the
    // current heycode route able to serialize that server tool.
    assert_eq!(model.capabilities.native_web, CapabilitySupport::Unknown);
    assert!(model.pricing.components().next().is_none());
}

#[tokio::test]
async fn sonnet_five_refuses_a_regional_location_the_current_card_does_not_offer() {
    let auth = auth_profile(Some("vertex-fixture"), Some("us-east5")).await;
    assert_eq!(
        ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap_err(),
        ClaudeVertexError::UnsupportedLocation
    );
}

#[tokio::test]
async fn the_profile_refuses_a_non_oauth_credential_kind() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let query = CredentialQuery::new(
        CredentialReference::new("not-an-access-token").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    );
    assert_eq!(
        ClaudeVertexProfile::from_gcp(&auth, query).unwrap_err(),
        ClaudeVertexError::CredentialKindMismatch
    );
}

#[tokio::test]
async fn missing_adc_project_or_location_fails_before_a_profile_exists() {
    for (project, location, expected) in [
        (None, Some("global"), ClaudeVertexError::ProjectUnavailable),
        (
            Some("vertex-fixture"),
            None,
            ClaudeVertexError::LocationUnavailable,
        ),
    ] {
        let auth = auth_profile(project, location).await;
        assert_eq!(
            ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap_err(),
            expected
        );
    }

    let service = GcpAuthService::new(
        Arc::new(MapGcpEnvironment::new()),
        HttpService::new(Arc::new(ScriptedSse::new(Vec::new()))),
    );
    let absent = service
        .resolve(
            GcpProfileRequest {
                project: Some("vertex-fixture".to_owned()),
                location: Some("global".to_owned()),
                platform: GcpHostPlatform::Unix,
                metadata: GcpMetadataPolicy::Disabled,
            },
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        ClaudeVertexProfile::from_gcp(&absent, token_query()).unwrap_err(),
        // With the ambient metadata probe deliberately disabled, PGCP01 has
        // not ruled out an attached service account. Unknown must not become
        // the stronger negative claim that ADC is absent.
        ClaudeVertexError::AccountUndetermined
    );
}

#[tokio::test]
async fn vertex_body_rewrite_moves_model_to_the_url_and_version_to_the_body() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let prepared = profile
        .prepare_messages_body(serde_json::json!({
            "model": CLAUDE_VERTEX_DEFAULT_MODEL,
            "max_tokens": 4096,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }],
            "tools": [{
                "name": "echo",
                "description": "echo",
                "input_schema": { "type": "object" }
            }],
            "thinking": { "type": "adaptive" },
            "output_config": { "effort": "high" }
        }))
        .unwrap();
    assert!(prepared.get("model").is_none());
    assert_eq!(
        prepared["anthropic_version"],
        CLAUDE_VERTEX_ANTHROPIC_VERSION
    );
    assert_eq!(prepared["tools"][0]["name"], "echo");
    assert_eq!(prepared["thinking"]["type"], "adaptive");
    assert_eq!(prepared["output_config"]["effort"], "high");
}

#[tokio::test]
async fn production_provider_reuses_shared_messages_tool_thinking_and_state_parser() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(vec![
        event(serde_json::json!({
            "type":"message_start",
            "message":{
                "id":"msg_vertex_shared","type":"message","role":"assistant",
                "model":CLAUDE_VERTEX_DEFAULT_MODEL,"content":[],
                "stop_reason":null,"stop_sequence":null,
                "usage":{"input_tokens":3,"output_tokens":1}
            }
        })),
        event(serde_json::json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"thinking","thinking":"","signature":""}
        })),
        event(serde_json::json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"thinking_delta","thinking":"check"}
        })),
        event(serde_json::json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"signature_delta","signature":"opaque-vertex-signature"}
        })),
        event(serde_json::json!({"type":"content_block_stop","index":0})),
        event(serde_json::json!({
            "type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","id":"toolu_vertex","name":"echo","input":{}}
        })),
        event(serde_json::json!({
            "type":"content_block_delta","index":1,
            "delta":{"type":"input_json_delta","partial_json":"{\"value\":true}"}
        })),
        event(serde_json::json!({"type":"content_block_stop","index":1})),
        event(serde_json::json!({
            "type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},
            "usage":{"output_tokens":3}
        })),
        event(serde_json::json!({"type":"message_stop"})),
    ]));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    let provider = ClaudeVertexProvider::new(
        profile.clone(),
        HttpService::new(scripted.clone()),
        credentials.as_ref(),
    )
    .unwrap();
    let request = RequestDraft {
        provider: CLAUDE_VERTEX_PROVIDER.to_owned(),
        model: CLAUDE_VERTEX_DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("call echo"))],
        tools: vec![ToolSpec {
            name: "echo".to_owned(),
            description: "Echo one boolean.".to_owned(),
            parameters: serde_json::json!({
                "type":"object","properties":{"value":{"type":"boolean"}},
                "required":["value"],"additionalProperties":false
            }),
        }],
        input_modalities: vec![InputModality::Text],
        reasoning_effort: Some(ReasoningEffortId::new("high").unwrap()),
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(4_096),
        purpose: CallPurpose::Conversation,
    };
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, profile.model())
        .unwrap();
    let events = provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();

    let recorded = scripted.take_request();
    assert_eq!(recorded.url, profile.endpoint());
    assert!(recorded.headers.iter().any(|(name, value)| {
        name == "authorization" && value == &format!("Bearer {TEST_SECRET}")
    }));
    assert!(
        recorded
            .headers
            .iter()
            .all(|(name, _)| name != "anthropic-version")
    );
    let body: serde_json::Value = serde_json::from_slice(&recorded.body).unwrap();
    assert!(body.get("model").is_none());
    assert_eq!(body["anthropic_version"], CLAUDE_VERTEX_ANTHROPIC_VERSION);
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["output_config"]["effort"], "high");
    assert!(events.iter().any(|event| matches!(
        event,
        InferenceEvent::ToolCallDelta { id: Some(id), name: Some(name), .. }
            if id.as_str() == "toolu_vertex" && name == "echo"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        InferenceEvent::ProviderState(state)
            if state.data()["content"][0]["signature"] == "opaque-vertex-signature"
    )));
}

#[tokio::test]
async fn production_provider_keeps_disabled_thinking_and_its_selected_effort() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(successful_text_events()));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    let provider = ClaudeVertexProvider::new_with_controls(
        profile.clone(),
        HttpService::new(scripted.clone()),
        credentials.as_ref(),
        ClaudeVertexControls::new(ClaudeVertexThinking::Disabled, ClaudeVertexEffort::XHigh),
    )
    .unwrap();
    let request = RequestDraft {
        provider: CLAUDE_VERTEX_PROVIDER.to_owned(),
        model: CLAUDE_VERTEX_DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("answer briefly"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: None,
        max_output_tokens: Some(4_096),
        purpose: CallPurpose::Conversation,
    };
    let call = provider
        .inference_adapter()
        .unwrap()
        .resolve(request, profile.model())
        .unwrap();
    assert_eq!(
        call.reasoning_effort().map(ReasoningEffortId::as_str),
        Some("xhigh")
    );
    provider
        .inference_adapter()
        .unwrap()
        .stream(call)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .for_each(|event| {
            event.unwrap();
        });

    let body: serde_json::Value = serde_json::from_slice(&scripted.take_request().body).unwrap();
    assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    assert_eq!(body["output_config"]["effort"], "xhigh");
}

#[tokio::test]
async fn production_provider_refuses_non_default_sonnet_sampling_before_transport() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(Vec::new()));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    let provider = ClaudeVertexProvider::new(
        profile.clone(),
        HttpService::new(scripted.clone()),
        credentials.as_ref(),
    )
    .unwrap();
    let request = RequestDraft {
        provider: CLAUDE_VERTEX_PROVIDER.to_owned(),
        model: CLAUDE_VERTEX_DEFAULT_MODEL.to_owned(),
        catalog_revision: Some(1),
        catalog_fetched_at_ms: Some(1),
        effective_at_ms: 2,
        system: None,
        inputs: vec![InferenceInput::Message(ChatMessage::user("answer briefly"))],
        tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        reasoning_effort: None,
        structured_output: None,
        native_features: Vec::new(),
        native_tool_routes: Vec::new(),
        provider_options: Vec::new(),
        temperature: Some(0.5),
        max_output_tokens: Some(4_096),
        purpose: CallPurpose::Conversation,
    };
    assert!(matches!(
        provider
            .inference_adapter()
            .unwrap()
            .resolve(request, profile.model()),
        Err(ResolveError::InvalidRequest {
            field: "temperature",
            ..
        })
    ));
    assert!(!scripted.has_request());
}

#[tokio::test]
async fn live_claude_vertex_probe_is_double_gated_and_reads_no_ambient_token_by_default() {
    if std::env::var("HEYCODE_E2E").ok().as_deref() != Some("1")
        || std::env::var("HEYCODE_E2E_CLAUDE_VERTEX").ok().as_deref() != Some("1")
    {
        return;
    }
    let (Ok(project), Ok(access_token)) = (
        std::env::var("GOOGLE_CLOUD_PROJECT"),
        std::env::var("GOOGLE_CLOUD_ACCESS_TOKEN"),
    ) else {
        return;
    };
    let auth = auth_profile(Some(&project), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let (_owner, credentials) = credentials(Some(&access_token));
    let transport = heycode_http::ReqwestHttpTransport::new().unwrap();
    let evidence = profile
        .probe(
            &HttpService::new(Arc::new(transport)),
            credentials.as_ref(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(evidence.authenticated());
    assert!(evidence.tool_use_observed());
    assert!(evidence.thinking_observed());
}

#[tokio::test]
async fn explicit_sonnet_controls_materialize_every_documented_choice() {
    assert_eq!(
        ClaudeVertexEffort::SUPPORTED.map(ClaudeVertexEffort::as_str),
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        ClaudeVertexThinking::SUPPORTED.map(ClaudeVertexThinking::as_str),
        ["adaptive", "disabled"]
    );

    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let prepared = profile
        .prepare_messages_body_with_controls(
            serde_json::json!({
                "model": CLAUDE_VERTEX_DEFAULT_MODEL,
                "messages": []
            }),
            ClaudeVertexControls::new(ClaudeVertexThinking::Disabled, ClaudeVertexEffort::XHigh),
        )
        .unwrap();
    assert_eq!(
        prepared["thinking"],
        serde_json::json!({ "type": "disabled" })
    );
    assert_eq!(prepared["output_config"]["effort"], "xhigh");
}

#[tokio::test]
async fn a_body_for_another_model_or_version_is_refused() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    assert_eq!(
        profile
            .prepare_messages_body(serde_json::json!({
                "model": "claude-opus-5",
                "messages": []
            }))
            .unwrap_err(),
        ClaudeVertexError::ModelMismatch
    );
    assert_eq!(
        profile
            .prepare_messages_body(serde_json::json!({
                "model": CLAUDE_VERTEX_DEFAULT_MODEL,
                "anthropic_version": "2023-06-01",
                "messages": []
            }))
            .unwrap_err(),
        ClaudeVertexError::VersionMismatch
    );
}

#[tokio::test]
async fn sonnet_five_refuses_manual_thinking_and_unknown_effort_before_transport() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let manual = profile.prepare_messages_body(serde_json::json!({
        "model": CLAUDE_VERTEX_DEFAULT_MODEL,
        "messages": [],
        "thinking": { "type": "enabled", "budget_tokens": 4096 },
        "output_config": { "effort": "high" }
    }));
    assert_eq!(manual.unwrap_err(), ClaudeVertexError::ThinkingMismatch);

    let unknown = profile.prepare_messages_body(serde_json::json!({
        "model": CLAUDE_VERTEX_DEFAULT_MODEL,
        "messages": [],
        "thinking": { "type": "adaptive" },
        "output_config": { "effort": "ultra" }
    }));
    assert_eq!(unknown.unwrap_err(), ClaudeVertexError::EffortMismatch);

    let sampling = profile.prepare_messages_body(serde_json::json!({
        "model": CLAUDE_VERTEX_DEFAULT_MODEL,
        "messages": [],
        "temperature": 0.5
    }));
    assert_eq!(
        sampling.unwrap_err(),
        ClaudeVertexError::SamplingUnsupported
    );
}

#[tokio::test]
async fn successful_probe_evidence_requires_auth_model_tool_and_thinking() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(successful_probe_events("{\"ok\":true}")));
    let http = HttpService::new(scripted.clone());
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    let evidence = profile
        .probe(&http, credentials.as_ref(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(evidence.model(), CLAUDE_VERTEX_DEFAULT_MODEL);
    assert!(evidence.authenticated());
    assert!(evidence.tool_use_observed());
    assert!(evidence.thinking_observed());

    let recorded = scripted.take_request();
    assert_eq!(recorded.url, profile.endpoint());
    assert!(recorded.headers.iter().any(|(name, value)| {
        name == "authorization" && value == &format!("Bearer {TEST_SECRET}")
    }));
    assert!(
        recorded
            .headers
            .iter()
            .all(|(name, _)| name != "anthropic-version")
    );
    let body: serde_json::Value = serde_json::from_slice(&recorded.body).unwrap();
    assert!(body.get("model").is_none());
    assert_eq!(body["anthropic_version"], CLAUDE_VERTEX_ANTHROPIC_VERSION);
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["output_config"]["effort"], "max");
    assert_eq!(body["tool_choice"]["name"], "heycode_vertex_probe");
}

#[tokio::test]
async fn unsettled_or_reordered_blocks_never_mint_live_evidence() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    // Both blocks are opened and neither is settled. Merely seeing their type
    // names is not evidence that the Messages stream can carry a valid tool or
    // thinking response through the shared parser boundary.
    let scripted = Arc::new(ScriptedSse::new(vec![
        event(serde_json::json!({
            "type": "message_start",
            "message": { "model": CLAUDE_VERTEX_DEFAULT_MODEL }
        })),
        event(serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "", "signature": "" }
        })),
        event(serde_json::json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_probe",
                "name": "heycode_vertex_probe",
                "input": {}
            }
        })),
        event(serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "tool_use" }
        })),
        event(serde_json::json!({ "type": "message_stop" })),
    ]));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::InvalidEvent
    );
}

#[tokio::test]
async fn the_live_probe_requires_the_exact_forced_tool_input() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(successful_probe_events("{\"ok\":false}")));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::InvalidEvent
    );
}

#[tokio::test]
async fn the_forced_probe_tool_must_run_exactly_once() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let mut events = successful_probe_events("{\"ok\":true}");
    events.splice(
        8..8,
        [
            event(serde_json::json!({
                "type": "content_block_start",
                "index": 2,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_probe_2",
                    "name": "heycode_vertex_probe",
                    "input": { "ok": true }
                }
            })),
            event(serde_json::json!({ "type": "content_block_stop", "index": 2 })),
        ],
    );
    let scripted = Arc::new(ScriptedSse::new(events));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::InvalidEvent
    );
}

#[tokio::test]
async fn the_sse_event_name_must_match_the_messages_payload_type() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let mut events = successful_probe_events("{\"ok\":true}");
    events[0].event = "content_block_start".to_owned();
    let scripted = Arc::new(ScriptedSse::new(events));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::InvalidEvent
    );
}

#[tokio::test]
async fn a_successful_http_stream_without_tool_or_thinking_is_not_live_evidence() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(vec![
        event(serde_json::json!({
            "type": "message_start",
            "message": { "model": CLAUDE_VERTEX_DEFAULT_MODEL }
        })),
        event(serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" }
        })),
        event(serde_json::json!({ "type": "message_stop" })),
    ]));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::IncompleteLiveEvidence
    );
}

#[tokio::test]
async fn pre_cancelled_probe_stops_before_credential_or_transport_work() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let scripted = Arc::new(ScriptedSse::new(Vec::new()));
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        profile
            .probe(
                &HttpService::new(scripted.clone()),
                credentials.as_ref(),
                cancellation
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::Cancelled
    );
    assert!(!scripted.has_request());
}

#[tokio::test]
async fn cancellation_during_the_stream_keeps_its_own_failure_class() {
    let auth = auth_profile(Some("vertex-fixture"), Some("global")).await;
    let profile = ClaudeVertexProfile::from_gcp(&auth, token_query()).unwrap();
    let (_owner, credentials) = credentials(Some(TEST_SECRET));
    assert_eq!(
        profile
            .probe(
                &HttpService::new(Arc::new(CancelOnSse)),
                credentials.as_ref(),
                CancellationToken::new()
            )
            .await
            .unwrap_err(),
        ClaudeVertexError::Cancelled
    );
}

fn event(data: serde_json::Value) -> SseEvent {
    let event = data["type"].as_str().unwrap().to_owned();
    SseEvent {
        event,
        data: data.to_string(),
        id: None,
        retry_ms: None,
    }
}

fn successful_probe_events(tool_input: &str) -> Vec<SseEvent> {
    vec![
        event(serde_json::json!({
            "type": "message_start",
            "message": { "model": CLAUDE_VERTEX_DEFAULT_MODEL }
        })),
        event(serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "", "signature": "" }
        })),
        event(serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "Checking the probe." }
        })),
        event(serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "signature_delta", "signature": "opaque-signature" }
        })),
        event(serde_json::json!({ "type": "content_block_stop", "index": 0 })),
        event(serde_json::json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "tool_use",
                "id": "toolu_probe",
                "name": "heycode_vertex_probe",
                "input": {}
            }
        })),
        event(serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": tool_input }
        })),
        event(serde_json::json!({ "type": "content_block_stop", "index": 1 })),
        event(serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": "tool_use" }
        })),
        event(serde_json::json!({ "type": "message_stop" })),
    ]
}

fn successful_text_events() -> Vec<SseEvent> {
    vec![
        event(serde_json::json!({
            "type":"message_start",
            "message":{
                "id":"msg_vertex_text","type":"message","role":"assistant",
                "model":CLAUDE_VERTEX_DEFAULT_MODEL,"content":[],
                "stop_reason":null,"stop_sequence":null,
                "usage":{"input_tokens":3,"output_tokens":1}
            }
        })),
        event(serde_json::json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"text","text":""}
        })),
        event(serde_json::json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"text_delta","text":"ok"}
        })),
        event(serde_json::json!({"type":"content_block_stop","index":0})),
        event(serde_json::json!({
            "type":"message_delta",
            "delta":{"stop_reason":"end_turn","stop_sequence":null},
            "usage":{"output_tokens":2}
        })),
        event(serde_json::json!({"type":"message_stop"})),
    ]
}

struct RecordedSseRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct ScriptedSse {
    events: Mutex<Vec<SseEvent>>,
    request: Mutex<Option<RecordedSseRequest>>,
}

struct CancelOnSse;

impl HttpTransport for CancelOnSse {
    fn sse(&self, _request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        cancellation.cancel();
        Box::pin(futures::stream::empty())
    }
}

impl ScriptedSse {
    fn new(events: Vec<SseEvent>) -> Self {
        Self {
            events: Mutex::new(events),
            request: Mutex::new(None),
        }
    }

    fn take_request(&self) -> RecordedSseRequest {
        self.request.lock().unwrap().take().unwrap()
    }

    fn has_request(&self) -> bool {
        self.request.lock().unwrap().is_some()
    }
}

impl HttpTransport for ScriptedSse {
    fn sse(&self, request: HttpSseRequest, _cancellation: CancellationToken) -> SseEventStream {
        *self.request.lock().unwrap() = Some(RecordedSseRequest {
            url: request.url().to_owned(),
            headers: request
                .headers()
                .iter()
                .map(|header| (header.name().to_owned(), header.value().to_owned()))
                .collect(),
            body: request.body().unwrap_or_default().to_vec(),
        });
        let events = std::mem::take(&mut *self.events.lock().unwrap());
        Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
    }
}
