//! TEL04 committed provider/tool/compaction/cache metric attribution.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_core::{Context, Plugin};
use heycode_session::{
    RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, SessionCreationMetadata, SessionEventKind,
    SessionSource,
};
use heycode_telemetry::{Dimension, TelemetryEvent, TelemetryEventName, TelemetryExporter};

#[derive(Default)]
struct Capture(Mutex<Vec<TelemetryEvent>>);

impl TelemetryExporter for Capture {
    fn export(&self, event: &TelemetryEvent) {
        self.0.lock().unwrap().push(event.clone());
    }

    fn flush(&self) {}
}

fn telemetry_plugin(capture: Arc<Capture>) -> Box<dyn Plugin> {
    struct CapturePlugin(Arc<Capture>);
    impl Plugin for CapturePlugin {
        fn name(&self) -> &'static str {
            "telemetry-capture"
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_telemetry::SERVICE_TELEMETRY]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            context.provide(
                heycode_telemetry::SERVICE_TELEMETRY,
                self.name(),
                heycode_telemetry::TelemetryService::exporting(self.0.clone()),
            )
        }
    }
    Box::new(CapturePlugin(capture))
}

fn header() -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        "openrouter",
        "z-ai/glm-5.3-flash",
        heycode_core::ProviderProtocol::OpenAiChatCompletions,
        RequestTargetSnapshot::Http {
            base_url: "https://openrouter.ai/api/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::None,
        None,
        Vec::new(),
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: vec!["web".to_owned()],
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            defaulted_max_output_tokens: false,
            purpose: "evaluation".to_owned(),
            retry: None,
        },
    )
    .unwrap()
}

fn labels(event: &TelemetryEvent) -> Vec<(Dimension, String)> {
    event
        .dimensions()
        .iter()
        .map(|(dimension, label)| (*dimension, label.as_str().to_owned()))
        .collect()
}

#[test]
fn committed_events_emit_closed_purpose_lineage_and_execution_metrics_only() {
    let root = tempfile::tempdir().unwrap();
    let capture = Arc::new(Capture::default());
    let plugins = [
        heycode_session::session_with_metadata_plugin(
            root.path().to_path_buf(),
            SessionCreationMetadata::new(
                Some(root.path().canonicalize().unwrap()),
                Some("native".to_owned()),
                SessionSource::Subagent,
            )
            .unwrap(),
        ),
        telemetry_plugin(capture.clone()),
        heycode_agent::telemetry_metrics_plugin(),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let session = context
        .get::<std::sync::Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let request_id = heycode_core::RequestId::from_raw("request_1");
    let server_id = heycode_core::CallId::from_raw("server_1");
    let local_id = heycode_core::CallId::from_raw("local_1");
    {
        let mut session = session.lock().unwrap();
        session
            .append(SessionEventKind::RequestHeader {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                header: Box::new(header()),
            })
            .unwrap();
        session
            .append(SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: RequestContextSnapshot::new(None, None, None, None, 1).unwrap(),
            })
            .unwrap();
        session
            .append(SessionEventKind::ToolCall {
                turn: 1,
                call_id: local_id,
                name: "read".to_owned(),
                args: serde_json::json!({"secret":"credential-canary"}),
            })
            .unwrap();
        session
            .append(SessionEventKind::ServerToolCall {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                output_index: 0,
                call: Box::new(
                    heycode_core::ServerToolCall::new(
                        server_id,
                        "web_search",
                        "openrouter:web_search",
                        serde_json::json!({"query":"credential-canary"}),
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(SessionEventKind::ServerToolUsage {
                turn: 1,
                step: 1,
                request_id: request_id.clone(),
                usage: Box::new(
                    heycode_core::ServerToolUsage::new(
                        "web_search",
                        2,
                        heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                        heycode_core::ServerToolUsageCost::Unknown,
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(SessionEventKind::AssistantResponseMetadata {
                turn: 1,
                step: 1,
                request_id,
                metadata: Box::new(
                    heycode_core::ProviderResponseMetadata::new(
                        Some(heycode_core::ProviderCacheUsage::new(10, 1, 4, 0).unwrap()),
                        Vec::new(),
                        None,
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(SessionEventKind::CompactionApplied {
                summary: "credential-canary".to_owned(),
                replaced_upto_seq: 1,
            })
            .unwrap();
    }

    let events = capture.0.lock().unwrap().clone();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name() == TelemetryEventName::ProviderRequest)
            .count(),
        1
    );
    let request = events
        .iter()
        .find(|event| event.name() == TelemetryEventName::ProviderRequest)
        .unwrap();
    let request_labels = labels(request);
    for expected in [
        (Dimension::Provider, "openrouter"),
        (Dimension::Model, "z-ai/glm-5.3-flash"),
        (Dimension::Purpose, "evaluation"),
        (Dimension::Lineage, "subagent"),
        (Dimension::Runtime, "native"),
    ] {
        assert!(request_labels.contains(&(expected.0, expected.1.to_owned())));
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name() == TelemetryEventName::ToolInvoked)
            .count(),
        3
    );
    assert!(events.iter().any(|event| {
        event.name() == TelemetryEventName::ToolInvoked
            && labels(event).contains(&(Dimension::Execution, "provider_aggregate".to_owned()))
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name() == TelemetryEventName::CacheObserved)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name() == TelemetryEventName::CompactionCompleted)
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("credential-canary")
    );

    context.shutdown();
    let before = capture.0.lock().unwrap().len();
    session
        .lock()
        .unwrap()
        .append(SessionEventKind::ToolCall {
            turn: 2,
            call_id: heycode_core::CallId::from_raw("after_shutdown"),
            name: "read".to_owned(),
            args: serde_json::json!({}),
        })
        .unwrap();
    assert_eq!(capture.0.lock().unwrap().len(), before);
}
