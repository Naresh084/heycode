//! ATT02 image-composer and durable replay behavior.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_agent::{AttachmentComposerAction, UiEvent};
use heycode_core::{
    AttachmentAudioMetadata, AttachmentContentId, AttachmentDimensions, AttachmentMediaType,
    AttachmentMetadata, RequestId,
};
use heycode_session::{CURRENT_SESSION_LOG_VERSION, SessionEvent, SessionEventKind};
use heycode_tui::app::{AppState, Item};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn metadata(byte: u8, name: &str) -> AttachmentMetadata {
    AttachmentMetadata::new(
        AttachmentContentId::from_sha256([byte; 32]),
        AttachmentMediaType::new("image/png").unwrap(),
        4,
        Some(name.to_owned()),
        Some(AttachmentDimensions::new(1, 1).unwrap()),
    )
    .unwrap()
}

fn document_metadata(byte: u8, media_type: &str, name: &str) -> AttachmentMetadata {
    AttachmentMetadata::new(
        AttachmentContentId::from_sha256([byte; 32]),
        AttachmentMediaType::new(media_type).unwrap(),
        4,
        Some(name.to_owned()),
        None,
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

fn frame_text(state: &mut AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn composer_stages_unique_images_and_clears_only_after_durable_echo() {
    let attachment = metadata(1, "pixel.png");
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));

    state.apply(&UiEvent::AttachmentComposerRequested {
        action: AttachmentComposerAction::Add(attachment.clone()),
    });
    state.apply(&UiEvent::AttachmentComposerRequested {
        action: AttachmentComposerAction::Add(attachment.clone()),
    });
    assert_eq!(
        state.pending_attachments.as_slice(),
        std::slice::from_ref(&attachment)
    );
    assert!(
        matches!(state.items.last(), Some(Item::Info(text)) if text == "attachment is already staged")
    );
    assert!(frame_text(&mut state).contains("1 attachment(s)"));

    state.apply(&UiEvent::UserAttachmentsEcho {
        attachments: vec![attachment.clone()],
        document_routes: Vec::new(),
    });
    assert!(state.pending_attachments.is_empty());
    assert!(matches!(
        state.items.last(),
        Some(Item::Attachments { attachments, document_routes })
            if attachments == &[attachment] && document_routes.is_empty()
    ));

    state.apply(&UiEvent::AttachmentComposerRequested {
        action: AttachmentComposerAction::Add(metadata(2, "second.png")),
    });
    state.apply(&UiEvent::AttachmentComposerRequested {
        action: AttachmentComposerAction::Clear,
    });
    assert!(state.pending_attachments.is_empty());
}

#[test]
fn replay_keeps_attachment_metadata_immediately_before_its_user_message() {
    let attachment = metadata(3, "replayed.png");
    let events = vec![
        SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: 0,
            time_ms: 1,
            kind: SessionEventKind::UserAttachments {
                attachments: vec![attachment],
                document_routes: Vec::new(),
            },
        },
        SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: 1,
            time_ms: 2,
            kind: SessionEventKind::UserMessage {
                text: "describe it".to_owned(),
            },
        },
    ];
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));

    state.replay(&events);

    assert!(
        matches!(state.items.as_slice(), [Item::Attachments { .. }, Item::User(text)] if text == "describe it")
    );
    let rendered = frame_text(&mut state);
    assert!(rendered.contains("replayed.png"), "{rendered}");
    assert!(rendered.contains("image/png"), "{rendered}");
    assert!(rendered.contains("1×1"), "{rendered}");
}

#[test]
fn committed_document_route_renders_native_or_extracted_policy_explicitly() {
    let source = document_metadata(4, "application/pdf", "guide.pdf");
    let selected = document_metadata(5, "text/plain", "guide.pdf");
    let route = heycode_core::DocumentInputRoute::extracted(source, selected.clone()).unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.pending_attachments.push(selected.clone());

    state.apply(&UiEvent::UserAttachmentsEcho {
        attachments: vec![selected],
        document_routes: vec![route],
    });

    assert!(state.pending_attachments.is_empty());
    let rendered = frame_text(&mut state);
    assert!(rendered.contains("guide.pdf"), "{rendered}");
    assert!(
        rendered.contains("locally extracted document"),
        "{rendered}"
    );
}

#[test]
fn assistant_audio_live_and_replay_render_metadata_without_bytes_or_content_id() {
    let audio = audio_metadata(6, "answer.wav");
    let mut live = AppState::new("model", std::path::PathBuf::from("/workspace"));
    live.apply(&UiEvent::AssistantAudio {
        attachments: vec![audio.clone()],
    });
    assert!(matches!(
        live.items.last(),
        Some(Item::AudioOutput { attachments }) if attachments == std::slice::from_ref(&audio)
    ));
    let rendered = frame_text(&mut live);
    assert!(rendered.contains("assistant audio"), "{rendered}");
    assert!(rendered.contains("1.00 s"), "{rendered}");
    assert!(rendered.contains("8 kHz"), "{rendered}");
    assert!(
        !rendered.contains(audio.content_id().as_str()),
        "{rendered}"
    );

    let events = vec![SessionEvent {
        v: CURRENT_SESSION_LOG_VERSION,
        seq: 0,
        time_ms: 1,
        kind: SessionEventKind::AssistantAudio {
            turn: 1,
            step: 0,
            request_id: RequestId::from_raw("audio-request-1"),
            attachments: vec![audio.clone()],
        },
    }];
    let mut replay = AppState::new("model", std::path::PathBuf::from("/workspace"));
    replay.replay(&events);
    assert!(matches!(
        replay.items.as_slice(),
        [Item::AudioOutput { attachments }] if attachments == &[audio]
    ));
}
