//! V2 request header/context durability and validation contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ProviderRequestOption,
    RequestId, ToolSpec,
};
use heycode_session::{
    OpenError, RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, Session, SessionEventKind, SnapshotError,
};

fn header() -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        "openrouter",
        "z-ai/glm-5.3-flash",
        ProviderProtocol::OpenAiChatCompletions,
        RequestTargetSnapshot::Http {
            base_url: "https://openrouter.ai/api/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::Credential {
            reference: "openrouter/api-key".to_owned(),
        },
        Some("You are heycode.\nUse tools carefully.".to_owned()),
        vec![ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned()],
            reasoning_effort: Some("high".to_owned()),
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: vec!["web".to_owned()],
            native_tool_routes: vec![
                NativeToolRoute::new(
                    "web_search",
                    "client:web_search",
                    NativeToolImplementationKind::Client,
                    None,
                )
                .unwrap(),
            ],
            provider_options: vec![
                ProviderRequestOption::new(
                    "openrouter",
                    "routing",
                    serde_json::json!({"allow_fallbacks": false, "zdr": true}),
                )
                .unwrap(),
            ],
            temperature: Some(0.2),
            max_output_tokens: Some(4_096),
            defaulted_max_output_tokens: false,
            purpose: "conversation".to_owned(),
            retry: None,
        },
    )
    .unwrap()
}

fn context() -> RequestContextSnapshot {
    RequestContextSnapshot::new(
        Some(1_048_576),
        Some(131_072),
        Some(7),
        Some(1_777_777_777_777),
        20,
    )
    .unwrap()
}

#[test]
fn request_header_and_context_round_trip_every_durable_field() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let request_id = RequestId::from_raw("req_1");
    let expected_header = header();
    let expected_context = context();
    session
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 2,
            request_id: request_id.clone(),
            header: Box::new(expected_header.clone()),
        })
        .unwrap();
    session
        .append(SessionEventKind::RequestContext {
            request_id: request_id.clone(),
            context: expected_context.clone(),
        })
        .unwrap();
    let dir = session.path().parent().unwrap().to_path_buf();
    let raw = std::fs::read_to_string(session.path()).unwrap();
    drop(session);

    assert!(
        raw.lines()
            .all(|line| { serde_json::from_str::<serde_json::Value>(line).unwrap()["v"] == 2 })
    );
    assert!(raw.contains("You are heycode."));
    assert!(raw.contains("z-ai/glm-5.3-flash"));
    assert!(raw.contains("allow_fallbacks"));
    assert!(raw.contains("client:web_search"));
    assert!(raw.contains("prompt_sha256"));
    let reopened = Session::open(dir).unwrap();
    match &reopened.events()[0].kind {
        SessionEventKind::RequestHeader {
            turn,
            step,
            request_id: found_id,
            header,
        } => {
            assert_eq!((*turn, *step), (1, 2));
            assert_eq!(found_id, &request_id);
            assert_eq!(header.as_ref(), &expected_header);
            assert_eq!(header.prompt_sha256.len(), 64);
            assert_eq!(header.tools[0].name, "read");
            assert_eq!(header.options.purpose, "conversation");
        }
        other => panic!("expected request/header, got {other:?}"),
    }
    match &reopened.events()[1].kind {
        SessionEventKind::RequestContext {
            request_id: found_id,
            context,
        } => {
            assert_eq!(found_id, &request_id);
            assert_eq!(context, &expected_context);
        }
        other => panic!("expected request/context, got {other:?}"),
    }
}

#[test]
fn request_kinds_are_v2_only() {
    let line = serde_json::json!({
        "v":1,"seq":0,"time_ms":1,"kind":"request/context",
        "data":{
            "request_id":"req_1",
            "context":{
                "context_window":100,
                "max_output_tokens":10,
                "catalog_revision":1,
                "catalog_fetched_at_ms":2
            }
        }
    });
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("v1-request-kind");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("session.jsonl"), line.to_string() + "\n").unwrap();
    assert!(matches!(
        Session::open(dir),
        Err(OpenError::UnknownKind { kind, .. }) if kind == "request/context"
    ));
}

#[test]
fn tampered_prompt_hash_is_rejected_on_read() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    session
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 1,
            request_id: RequestId::from_raw("req_1"),
            header: Box::new(header()),
        })
        .unwrap();
    let path = session.path().to_path_buf();
    let dir = path.parent().unwrap().to_path_buf();
    drop(session);
    let raw = std::fs::read_to_string(&path).unwrap();
    let tampered = raw.replace("You are heycode.", "You are evil.");
    std::fs::write(&path, tampered).unwrap();

    assert!(matches!(
        Session::open(dir),
        Err(OpenError::InvalidEvent { line_no: 1, message })
            if message.contains("prompt_sha256")
    ));
}

#[test]
fn snapshot_constructors_reject_invalid_tools_options_and_context() {
    let mut invalid_tool = header();
    invalid_tool.tools[0].parameters = serde_json::json!([]);
    assert!(matches!(
        invalid_tool.validate(),
        Err(SnapshotError::InvalidField { field: "tools", .. })
    ));

    let mut invalid_options = header();
    invalid_options.options.temperature = Some(f32::NAN);
    assert!(matches!(
        invalid_options.validate(),
        Err(SnapshotError::InvalidField {
            field: "temperature",
            ..
        })
    ));

    assert!(RequestContextSnapshot::new(Some(0), None, None, None, 1).is_err());
    assert!(RequestContextSnapshot::new(None, Some(0), None, None, 1).is_err());
}

/// A header written before the retry field existed still reads, and says it
/// was not recorded rather than claiming a policy it never ran under.
#[test]
fn an_older_header_without_a_retry_row_still_parses_as_not_recorded() {
    let legacy = serde_json::json!({
        "input_modalities": ["text"],
        "defaulted_reasoning_effort": false,
        "native_features": [],
        "purpose": "conversation"
    });
    let options: heycode_session::RequestOptionsSnapshot = serde_json::from_value(legacy).unwrap();
    assert_eq!(options.retry, None);

    let current = heycode_session::RequestOptionsSnapshot {
        retry: Some(heycode_session::RequestRetrySnapshot {
            max_attempts: 3,
            safety: heycode_session::RequestRetrySafetySnapshot::StatelessPreOutput,
        }),
        ..options
    };
    let line = serde_json::to_value(&current).unwrap();
    assert_eq!(line["retry"]["max_attempts"], 3);
    assert_eq!(line["retry"]["safety"], "stateless_pre_output");
    assert_eq!(
        serde_json::from_value::<heycode_session::RequestOptionsSnapshot>(line).unwrap(),
        current,
        "the row round-trips"
    );
}

#[test]
fn request_configuration_tracks_exact_components_and_survives_reopen() {
    use heycode_session::RequestConfigurationChange as Change;
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let mut first = header();
    first.record_configuration(None).unwrap();
    assert_eq!(
        first.configuration.as_ref().unwrap().changed,
        vec![Change::Initial]
    );
    let mut same = first.clone();
    same.record_configuration(Some(&first)).unwrap();
    assert_eq!(same.configuration.as_ref().unwrap().revision, 1);
    assert!(same.configuration.as_ref().unwrap().changed.is_empty());
    let mut changed = same.clone();
    changed.tools[0].description.push_str(" with continuation");
    changed.record_configuration(Some(&same)).unwrap();
    assert_eq!(changed.configuration.as_ref().unwrap().revision, 2);
    assert_eq!(
        changed.configuration.as_ref().unwrap().changed,
        vec![Change::Tools]
    );
    let mut route = changed.clone();
    route.model = "another-model".into();
    route.record_configuration(Some(&changed)).unwrap();
    assert_eq!(route.configuration.as_ref().unwrap().revision, 3);
    assert_eq!(
        route.configuration.as_ref().unwrap().changed,
        vec![Change::Route]
    );
    for (index, header) in [first, same, changed, route.clone()]
        .into_iter()
        .enumerate()
    {
        let request_id = RequestId::from_raw(format!("config-{index}"));
        session
            .append(SessionEventKind::RequestHeader {
                turn: 1,
                step: index as u32,
                request_id: request_id.clone(),
                header: Box::new(header),
            })
            .unwrap();
        session
            .append(SessionEventKind::RequestContext {
                request_id,
                context: context(),
            })
            .unwrap();
    }
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(directory).unwrap();
    let requests = heycode_session::project_requests(reopened.events()).unwrap();
    assert_eq!(
        requests.last().unwrap().header.configuration,
        route.configuration
    );
    let mut tampered = route.clone();
    tampered.tools[0].description.push('!');
    assert!(
        tampered.validate().is_err(),
        "mismatched fingerprint must not pass validation"
    );
    let mut events = reopened.events().to_vec();
    if let SessionEventKind::RequestHeader { header, .. } = &mut events[6].kind {
        header.configuration.as_mut().unwrap().revision = 1;
    }
    assert!(
        heycode_session::project_requests(&events).is_err(),
        "revision regression must not be accepted"
    );
    let mut old = serde_json::to_value(route).unwrap();
    old.as_object_mut().unwrap().remove("configuration");
    let legacy: RequestHeaderSnapshot = serde_json::from_value(old).unwrap();
    assert!(legacy.configuration.is_none());
    legacy.validate().unwrap();
}

#[test]
fn canonical_tool_catalog_preserves_values_and_array_order() {
    let a = ToolSpec {
        name: "a".into(),
        description: "A".into(),
        parameters: serde_json::json!({"type":"object","properties":{"z":{"enum":["b","a"]},"a":{"type":"string"}}}),
    };
    let b = ToolSpec {
        name: "b".into(),
        description: "B".into(),
        parameters: serde_json::json!({"type":"object"}),
    };
    let left = heycode_core::canonical_tool_specs(vec![b.clone(), a.clone()]);
    let right = heycode_core::canonical_tool_specs(vec![a.clone(), b]);
    assert_eq!(
        serde_json::to_string(&left).unwrap(),
        serde_json::to_string(&right).unwrap()
    );
    assert_eq!(left[0].parameters, a.parameters);
    assert_eq!(
        left[0].parameters["properties"]["z"]["enum"],
        serde_json::json!(["b", "a"])
    );
}

#[test]
fn contributor_measurements_survive_reopen_and_reject_partial_or_duplicate_inventory() {
    use heycode_session::{
        RequestContributorMeasurement as Measurement, RequestContributorSnapshot,
    };
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let request_id = RequestId::from_raw("req_contributors");
    let mut context = context();
    context.contributors = Some(
        [
            "system",
            "messages",
            "tool_results",
            "tools",
            "provider_state",
            "attachments",
        ]
        .into_iter()
        .map(|name| RequestContributorSnapshot {
            contributor: name.into(),
            measurement: match name {
                "provider_state" => Measurement::Uncounted {
                    reason: "unmeasurable".into(),
                },
                "tool_results" => Measurement::Estimated {
                    tokens: 1700,
                    method: "utf8_byte_ratio".into(),
                },
                _ => Measurement::Exact { tokens: 0 },
            },
            refusals: if name == "messages" {
                vec!["provider:unsupported transcript".into()]
            } else {
                vec![]
            },
        })
        .collect(),
    );
    session
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 0,
            request_id: request_id.clone(),
            header: Box::new(header()),
        })
        .unwrap();
    session
        .append(SessionEventKind::RequestContext {
            request_id: request_id.clone(),
            context: context.clone(),
        })
        .unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let mut reopened = Session::open(directory).unwrap();
    let projected = heycode_session::project_requests(reopened.events()).unwrap();
    assert_eq!(projected[0].context, context);
    let mut partial = context.clone();
    partial.contributors.as_mut().unwrap().pop();
    assert!(
        reopened
            .append(SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: partial
            })
            .is_err()
    );
    let mut duplicate = context;
    let first = duplicate.contributors.as_ref().unwrap()[0].clone();
    duplicate.contributors.as_mut().unwrap().push(first);
    assert!(
        reopened
            .append(SessionEventKind::RequestContext {
                request_id,
                context: duplicate
            })
            .is_err()
    );
}
