//! ATT01 durable attachment metadata and projection decision.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    AttachmentAudioMetadata, AttachmentContentId, AttachmentDimensions, AttachmentMediaType,
    AttachmentMetadata, ProviderProtocol, RequestId,
};
use heycode_session::{
    RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, Session, SessionEventKind, derive_messages,
};

fn metadata() -> AttachmentMetadata {
    AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x42; 32]),
        AttachmentMediaType::new("image/png").unwrap(),
        100,
        Some("image.png".to_owned()),
        Some(AttachmentDimensions::new(10, 10).unwrap()),
    )
    .unwrap()
}

fn audio_metadata(byte: u8, name: &str) -> AttachmentMetadata {
    AttachmentMetadata::new_audio(
        AttachmentContentId::from_sha256([byte; 32]),
        AttachmentMediaType::new("audio/wav").unwrap(),
        16_044,
        Some(name.to_owned()),
        AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
    )
    .unwrap()
}

fn audio_request_header() -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        "fixture",
        "fixture-audio-model",
        ProviderProtocol::OpenAiChatCompletions,
        RequestTargetSnapshot::Http {
            base_url: "https://audio.example.test/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::AdapterOwned,
        None,
        Vec::new(),
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned(), "audio".to_owned()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: Vec::new(),
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: Some(64),
            defaulted_max_output_tokens: false,
            purpose: "conversation".to_owned(),
            retry: None,
        },
    )
    .unwrap()
}

#[test]
fn attachment_metadata_round_trips_v2_and_stays_out_of_model_projection_until_att02() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let committed = session
        .append(SessionEventKind::AttachmentAdded {
            attachment: Box::new(metadata()),
        })
        .unwrap();
    assert_eq!(committed.kind.name(), "attachment/added");
    assert!(derive_messages(session.events()).is_empty());
    let session_dir = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let resumed = Session::open(session_dir).unwrap();
    assert!(matches!(
        &resumed.events()[0].kind,
        SessionEventKind::AttachmentAdded { attachment } if attachment.as_ref() == &metadata()
    ));
}

#[test]
fn v1_attachment_kind_is_rejected_instead_of_skipped() {
    let root = tempfile::tempdir().unwrap();
    let session_dir = root.path().join("v1-attachment");
    std::fs::create_dir(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "v":1,
                "seq":0,
                "time_ms":1,
                "kind":"attachment/added",
                "data":{"attachment":metadata()}
            })
        ),
    )
    .unwrap();
    match Session::open(session_dir) {
        Err(heycode_session::OpenError::UnknownKind { kind, .. }) => {
            assert_eq!(kind, "attachment/added")
        }
        Err(error) => panic!("unexpected v1 attachment error: {error:?}"),
        Ok(_) => panic!("v1 attachment kind must not open"),
    }
}

#[test]
fn selected_attachments_commit_atomically_bind_next_message_and_refuse_invalid_refs() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let attachment = metadata();
    session
        .append(SessionEventKind::AttachmentAdded {
            attachment: Box::new(attachment.clone()),
        })
        .unwrap();
    let published = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = published.clone();
    session
        .bus()
        .on(move |event: &heycode_session::SessionEvent| {
            sink.lock().unwrap().push(event.kind.name());
        });
    let user = session
        .append_user_message_with_attachments("describe it", vec![attachment.clone()])
        .unwrap();
    assert_eq!(user.kind.name(), "user/message");
    assert_eq!(
        published.lock().unwrap().as_slice(),
        ["user/attachments", "user/message"]
    );
    let messages = derive_messages(session.events());
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].attachments.as_slice(),
        std::slice::from_ref(&attachment)
    );

    let selection_index = session.events().len() - 2;
    assert!(matches!(
        session.fork(
            root.path(),
            heycode_session::ForkBoundary::EventCount(selection_index as u64 + 1),
        ),
        Err(heycode_session::ForkError::Projection)
    ));
    let session_dir = session.path().parent().unwrap().to_path_buf();
    drop(session);
    assert!(Session::open(session_dir).is_ok());

    let other = tempfile::tempdir().unwrap();
    let mut unadmitted = Session::create(other.path()).unwrap();
    assert!(
        unadmitted
            .append_user_message_with_attachments("no", vec![attachment])
            .is_err()
    );
    assert!(unadmitted.events().is_empty());
}

#[test]
fn extracted_document_route_is_explicit_and_reconstructs_from_prior_admissions() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let source = heycode_core::AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x51; 32]),
        AttachmentMediaType::new("application/pdf").unwrap(),
        20,
        Some("guide.pdf".to_owned()),
        None,
    )
    .unwrap();
    let extracted = heycode_core::AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x52; 32]),
        AttachmentMediaType::new("text/plain").unwrap(),
        12,
        Some("guide.pdf".to_owned()),
        None,
    )
    .unwrap();
    for attachment in [&source, &extracted] {
        session
            .append(SessionEventKind::AttachmentAdded {
                attachment: Box::new(attachment.clone()),
            })
            .unwrap();
    }
    let route = heycode_core::DocumentInputRoute::extracted(source, extracted.clone()).unwrap();

    session
        .append_user_message_with_attachment_routes(
            "summarize",
            vec![extracted.clone()],
            vec![route.clone()],
        )
        .unwrap();

    let messages = derive_messages(session.events());
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].attachments, [extracted]);
    assert_eq!(messages[0].document_routes, [route]);
    let session_dir = session.path().parent().unwrap().to_path_buf();
    drop(session);
    assert!(Session::open(session_dir).is_ok());
}

#[test]
fn audio_input_and_output_associations_are_durable_metadata_only() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let input = audio_metadata(0x61, "question.wav");
    session
        .append(SessionEventKind::AttachmentAdded {
            attachment: Box::new(input.clone()),
        })
        .unwrap();
    session
        .append_user_message_with_attachments("transcribe", vec![input.clone()])
        .unwrap();
    let messages = derive_messages(session.events());
    assert_eq!(messages[0].attachments, [input]);
    assert!(messages[0].document_routes.is_empty());

    let request_id = RequestId::from_raw("audio-request-1");
    session
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 0,
            request_id: request_id.clone(),
            header: Box::new(audio_request_header()),
        })
        .unwrap();
    session
        .append(SessionEventKind::RequestContext {
            request_id: request_id.clone(),
            context: RequestContextSnapshot::new(None, None, Some(1), Some(2), 3).unwrap(),
        })
        .unwrap();
    let output = audio_metadata(0x62, "answer.wav");
    session
        .append(SessionEventKind::AttachmentAdded {
            attachment: Box::new(output.clone()),
        })
        .unwrap();
    let event = session
        .append_assistant_audio(1, 0, request_id.clone(), vec![output.clone()])
        .unwrap();
    assert_eq!(event.kind.name(), "assistant/audio");
    let encoded = serde_json::to_string(&event).unwrap();
    assert!(!encoded.contains("data:audio"));
    assert!(!encoded.contains("base64"));

    let session_dir = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(session_dir).unwrap();
    assert!(matches!(
        &reopened.events().last().unwrap().kind,
        SessionEventKind::AssistantAudio {
            turn: 1,
            step: 0,
            request_id: found,
            attachments,
        } if found == &request_id && attachments == &[output]
    ));
}

#[test]
fn assistant_audio_is_v2_only_and_requires_a_prior_exact_admission() {
    let root = tempfile::tempdir().unwrap();
    let session_dir = root.path().join("v1-audio");
    std::fs::create_dir(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "v":1,
                "seq":0,
                "time_ms":1,
                "kind":"assistant/audio",
                "data":{
                    "turn":1,
                    "step":0,
                    "request_id":"request-1",
                    "attachments":[audio_metadata(0x63, "answer.wav")]
                }
            })
        ),
    )
    .unwrap();
    assert!(matches!(
        Session::open(session_dir),
        Err(heycode_session::OpenError::UnknownKind { kind, .. }) if kind == "assistant/audio"
    ));

    let other = tempfile::tempdir().unwrap();
    let mut session = Session::create(other.path()).unwrap();
    assert!(
        session
            .append_assistant_audio(
                1,
                0,
                RequestId::from_raw("request-1"),
                vec![audio_metadata(0x64, "orphan.wav")],
            )
            .is_err()
    );
    assert!(session.events().is_empty());
}
