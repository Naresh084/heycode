//! ATT01 validated content-address and durable metadata vocabulary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{
    AttachmentAudioMetadata, AttachmentContentId, AttachmentDimensions, AttachmentMediaType,
    AttachmentMetadata, AttachmentSourceMetadata, DocumentInputRoute, DocumentInputRouteKind,
};

#[test]
fn content_ids_media_types_dimensions_and_metadata_validate_and_round_trip() {
    let content_id = AttachmentContentId::from_sha256([0xab; 32]);
    assert_eq!(content_id.as_str(), format!("sha256-{}", "ab".repeat(32)));
    assert_eq!(content_id.digest_hex(), "ab".repeat(32));
    assert!(AttachmentContentId::new("sha256-not-a-digest").is_err());

    let media_type = AttachmentMediaType::new("IMAGE/PNG").unwrap();
    assert_eq!(media_type.as_str(), "image/png");
    assert!(AttachmentMediaType::new("image/png; charset=utf-8").is_err());
    assert!(AttachmentMediaType::new("image png").is_err());

    let dimensions = AttachmentDimensions::new(640, 480).unwrap();
    assert_eq!(dimensions.width(), 640);
    assert_eq!(dimensions.height(), 480);
    assert!(AttachmentDimensions::new(0, 480).is_err());

    let metadata = AttachmentMetadata::new(
        content_id.clone(),
        media_type,
        1_024,
        Some("diagram.png".to_owned()),
        Some(dimensions),
    )
    .unwrap()
    .with_source(
        AttachmentSourceMetadata::new(
            "https://example.test/image.png",
            Some("Diagram".to_owned()),
            1_000,
            false,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    metadata.validate().unwrap();
    assert_eq!(metadata.content_id(), &content_id);
    assert_eq!(metadata.byte_len(), 1_024);
    assert_eq!(metadata.display_name(), Some("diagram.png"));
    assert_eq!(metadata.dimensions(), Some(dimensions));
    assert_eq!(
        metadata.source().unwrap().url(),
        "https://example.test/image.png"
    );

    let encoded = serde_json::to_vec(&metadata).unwrap();
    let decoded: AttachmentMetadata = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, metadata);
    let debug = format!("{metadata:?}");
    assert!(!debug.contains(content_id.as_str()));
    assert!(!debug.contains("diagram.png"));
    assert!(!debug.contains("example.test"));
}

#[test]
fn metadata_rejects_unsafe_names_size_and_incoherent_dimensions() {
    let id = AttachmentContentId::from_sha256([0x11; 32]);
    let png = AttachmentMediaType::new("image/png").unwrap();
    let text = AttachmentMediaType::new("text/plain").unwrap();
    let dimensions = AttachmentDimensions::new(1, 1).unwrap();

    assert!(AttachmentMetadata::new(id.clone(), png, 1, None, None).is_err());
    assert!(AttachmentMetadata::new(id.clone(), text.clone(), 1, None, Some(dimensions),).is_err());
    assert!(AttachmentMetadata::new(id.clone(), text.clone(), 0, None, None).is_err());
    assert!(
        AttachmentMetadata::new(
            id.clone(),
            text.clone(),
            1,
            Some("../secret.txt".to_owned()),
            None,
        )
        .is_err()
    );
    assert!(AttachmentMetadata::new(id, text, 1, Some("bad\nname".to_owned()), None).is_err());
}

#[test]
fn audio_metadata_is_bounded_coherent_and_redacted_at_the_attachment_boundary() {
    let id = AttachmentContentId::from_sha256([0x44; 32]);
    let media_type = AttachmentMediaType::new("AUDIO/WAV").unwrap();
    let audio = AttachmentAudioMetadata::new(1_250, 48_000, 2, 16).unwrap();
    let metadata = AttachmentMetadata::new_audio(
        id.clone(),
        media_type,
        240_044,
        Some("voice.wav".to_owned()),
        audio,
    )
    .unwrap();

    assert!(metadata.media_type().is_audio());
    assert_eq!(metadata.audio(), Some(audio));
    assert_eq!(audio.duration_ms(), 1_250);
    assert_eq!(audio.sample_rate_hz(), 48_000);
    assert_eq!(audio.channels(), 2);
    assert_eq!(audio.bits_per_sample(), 16);
    let encoded = serde_json::to_vec(&metadata).unwrap();
    assert_eq!(
        serde_json::from_slice::<AttachmentMetadata>(&encoded).unwrap(),
        metadata
    );
    let debug = format!("{metadata:?}");
    assert!(!debug.contains(id.as_str()));
    assert!(!debug.contains("voice.wav"));

    assert!(AttachmentAudioMetadata::new(0, 48_000, 2, 16).is_err());
    assert!(AttachmentAudioMetadata::new(1, 7_999, 2, 16).is_err());
    assert!(AttachmentAudioMetadata::new(1, 48_000, 0, 16).is_err());
    assert!(AttachmentAudioMetadata::new(1, 48_000, 2, 12).is_err());
    assert!(
        AttachmentMetadata::new(
            AttachmentContentId::from_sha256([0x45; 32]),
            AttachmentMediaType::new("audio/wav").unwrap(),
            44,
            None,
            None,
        )
        .is_err()
    );
}

#[test]
fn document_routes_distinguish_exact_native_bytes_from_durable_extraction() {
    let source = AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x22; 32]),
        AttachmentMediaType::new("application/pdf").unwrap(),
        20,
        Some("manual.pdf".to_owned()),
        None,
    )
    .unwrap();
    let extracted = AttachmentMetadata::new(
        AttachmentContentId::from_sha256([0x33; 32]),
        AttachmentMediaType::new("text/plain").unwrap(),
        10,
        Some("manual.pdf".to_owned()),
        None,
    )
    .unwrap();

    let native = DocumentInputRoute::native(source.clone()).unwrap();
    assert_eq!(native.kind(), DocumentInputRouteKind::Native);
    assert_eq!(native.source(), native.selected());
    let portable = DocumentInputRoute::extracted(source.clone(), extracted.clone()).unwrap();
    assert_eq!(portable.kind(), DocumentInputRouteKind::Extracted);
    assert_eq!(portable.source(), &source);
    assert_eq!(portable.selected(), &extracted);
    let encoded = serde_json::to_vec(&portable).unwrap();
    assert_eq!(
        serde_json::from_slice::<DocumentInputRoute>(&encoded).unwrap(),
        portable
    );
    let debug = format!("{portable:?}");
    assert!(!debug.contains(source.content_id().as_str()));
    assert!(!debug.contains(extracted.content_id().as_str()));
    assert!(DocumentInputRoute::native(extracted.clone()).is_err());
    assert!(DocumentInputRoute::extracted(source, extracted.clone()).is_ok());
    assert!(DocumentInputRoute::extracted(extracted.clone(), extracted).is_err());
}
