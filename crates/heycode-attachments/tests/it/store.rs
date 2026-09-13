//! ATT01 immutable storage, validation, session commit and lifecycle contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_attachments::{
    AttachmentInput, AttachmentStore, AttachmentStoreConfig, AttachmentStoreErrorClass,
    SERVICE_ATTACHMENTS, local_attachment_plugin,
};
use heycode_core::AttachmentDimensions;
use heycode_session::{Session, SessionEventKind};
use image::ImageEncoder as _;
use tokio_util::sync::CancellationToken;

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let pixels = vec![
        0_u8;
        usize::try_from(width)
            .unwrap()
            .checked_mul(usize::try_from(height).unwrap())
            .unwrap()
            .checked_mul(4)
            .unwrap()
    ];
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes
}

fn pcm_wav(sample_rate_hz: u32, channels: u16, frames: u32, marker: &[u8]) -> Vec<u8> {
    let bits_per_sample = 16_u16;
    let block_align = channels * (bits_per_sample / 8);
    let data_len = frames * u32::from(block_align);
    let mut data = vec![0_u8; usize::try_from(data_len).unwrap()];
    let copied = marker.len().min(data.len());
    data[..copied].copy_from_slice(&marker[..copied]);
    let riff_size = 36_u32 + data_len;
    let mut bytes = Vec::with_capacity(usize::try_from(riff_size + 8).unwrap());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_size.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate_hz * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&data);
    bytes
}

fn world(max_bytes: usize) -> (tempfile::TempDir, heycode_core::Context) {
    let root = tempfile::tempdir().unwrap();
    let config = AttachmentStoreConfig::new(root.path().join("objects-root"), max_bytes).unwrap();
    let plugins = vec![
        heycode_session::session_plugin(root.path().join("sessions")),
        local_attachment_plugin(config),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    (root, context)
}

#[test]
fn admission_commits_immutable_bytes_before_metadata_and_round_trips() {
    let (root, mut context) = world(1024 * 1024);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    let bytes = png(2, 3);
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let bus = session.lock().unwrap().bus();
    let observed_after_bytes = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = observed_after_bytes.clone();
    let observer_store = store.clone();
    bus.on(move |event: &heycode_session::SessionEvent| {
        if let SessionEventKind::AttachmentAdded { attachment } = &event.kind {
            assert!(
                observer_store
                    .read(attachment, CancellationToken::new())
                    .is_ok(),
                "session publication must observe already-durable bytes"
            );
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let input = AttachmentInput::new(bytes.clone(), Some("image/png"), Some("pixel.png")).unwrap();
    let first = store
        .admit(input.clone(), CancellationToken::new())
        .unwrap();
    let second = store.admit(input, CancellationToken::new()).unwrap();
    assert_eq!(first.metadata(), second.metadata());
    assert!(observed_after_bytes.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(first.metadata().byte_len(), bytes.len() as u64);
    assert_eq!(
        first.metadata().dimensions(),
        Some(AttachmentDimensions::new(2, 3).unwrap())
    );
    assert_eq!(
        store
            .read(first.metadata(), CancellationToken::new())
            .unwrap(),
        bytes
    );

    let events = session.lock().unwrap().events().to_vec();
    let attachments = events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AttachmentAdded { attachment } => Some(attachment.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attachments, [first.metadata(), second.metadata()]);

    let digest = first.metadata().content_id().digest_hex();
    let object = root
        .path()
        .join("objects-root/objects/sha256")
        .join(&digest[..2])
        .join(&digest[2..]);
    assert!(object.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(root.path().join("objects-root"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&object).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    context.shutdown();
    assert_eq!(
        store
            .read(first.metadata(), CancellationToken::new())
            .unwrap_err()
            .class(),
        AttachmentStoreErrorClass::Stopped
    );
}

#[test]
fn invalid_size_mime_name_image_and_cancellation_publish_nothing() {
    let (_root, context) = world(128);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    for input in [
        AttachmentInput::new(Vec::new(), Some("text/plain"), Some("empty.txt")),
        AttachmentInput::new(vec![b'x'; 129], Some("text/plain"), Some("large.txt")),
        AttachmentInput::new(
            b"plain text".to_vec(),
            Some("application/pdf"),
            Some("fake.pdf"),
        ),
        AttachmentInput::new(
            b"plain text".to_vec(),
            Some("text/plain"),
            Some("../bad.txt"),
        ),
        AttachmentInput::new(b"not an image".to_vec(), Some("image/png"), Some("bad.png")),
        AttachmentInput::new(
            b"\x89PNG\r\n\x1a\ntruncated".to_vec(),
            Some("image/png"),
            Some("truncated.png"),
        ),
        AttachmentInput::new(
            b"BMunsupported".to_vec(),
            Some("application/octet-stream"),
            Some("unsupported.bmp"),
        ),
    ] {
        match input {
            Ok(input) => assert!(store.admit(input, CancellationToken::new()).is_err()),
            Err(error) => assert!(matches!(
                error.class(),
                AttachmentStoreErrorClass::InvalidInput | AttachmentStoreErrorClass::SizeLimit
            )),
        }
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = store
        .admit(
            AttachmentInput::new(b"safe".to_vec(), Some("text/plain"), Some("safe.txt")).unwrap(),
            cancelled,
        )
        .unwrap_err();
    assert_eq!(error.class(), AttachmentStoreErrorClass::Cancelled);
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    assert!(session.lock().unwrap().events().is_empty());
}

#[test]
fn concurrent_duplicate_admission_publishes_one_object_and_each_durable_reference() {
    let (_root, context) = world(1024);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    let input = AttachmentInput::new(
        b"same immutable bytes".to_vec(),
        Some("text/plain"),
        Some("same.txt"),
    )
    .unwrap();
    let threads = (0..8)
        .map(|_| {
            let store = store.clone();
            let input = input.clone();
            std::thread::spawn(move || store.admit(input, CancellationToken::new()).unwrap())
        })
        .collect::<Vec<_>>();
    let admissions = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert!(
        admissions
            .windows(2)
            .all(|pair| pair[0].metadata() == pair[1].metadata())
    );
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    assert_eq!(
        session
            .lock()
            .unwrap()
            .events()
            .iter()
            .filter(|event| matches!(&event.kind, SessionEventKind::AttachmentAdded { .. }))
            .count(),
        8
    );
}

#[test]
fn explicit_image_path_is_race_checked_and_non_images_do_not_publish() {
    let (root, context) = world(1024 * 1024);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    let image_path = root.path().join("selected.png");
    std::fs::write(&image_path, png(1, 1)).unwrap();
    let admission = store
        .admit_image_path(&image_path, CancellationToken::new())
        .unwrap();
    assert_eq!(admission.metadata().display_name(), Some("selected.png"));
    assert_eq!(admission.metadata().media_type().as_str(), "image/png");

    let text_path = root.path().join("not-image.txt");
    std::fs::write(&text_path, b"plain text").unwrap();
    assert_eq!(
        store
            .admit_image_path(&text_path, CancellationToken::new())
            .unwrap_err()
            .class(),
        AttachmentStoreErrorClass::UnsupportedMedia
    );
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    assert_eq!(session.lock().unwrap().events().len(), 1);

    #[cfg(unix)]
    {
        let link = root.path().join("linked.png");
        std::os::unix::fs::symlink(&image_path, &link).unwrap();
        assert!(
            store
                .admit_image_path(&link, CancellationToken::new())
                .is_err()
        );
        assert_eq!(session.lock().unwrap().events().len(), 1);
    }
}

#[test]
fn wav_admission_sniffs_and_rechecks_exact_audio_metadata() {
    let (root, context) = world(1024 * 1024);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    let bytes = pcm_wav(48_000, 2, 48_000, b"RAW-AUDIO-CANARY");
    let path = root.path().join("selected.wav");
    std::fs::write(&path, &bytes).unwrap();

    let admission = store
        .admit_audio_path(&path, CancellationToken::new())
        .unwrap();
    assert_eq!(admission.metadata().media_type().as_str(), "audio/wav");
    let audio = admission.metadata().audio().unwrap();
    assert_eq!(audio.duration_ms(), 1_000);
    assert_eq!(audio.sample_rate_hz(), 48_000);
    assert_eq!(audio.channels(), 2);
    assert_eq!(audio.bits_per_sample(), 16);
    assert_eq!(
        store
            .read(admission.metadata(), CancellationToken::new())
            .unwrap(),
        bytes
    );
    let debug = format!("{admission:?}");
    assert!(!debug.contains("RAW-AUDIO-CANARY"));

    let truncated = root.path().join("truncated.wav");
    std::fs::write(&truncated, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
    assert_eq!(
        store
            .admit_audio_path(&truncated, CancellationToken::new())
            .unwrap_err()
            .class(),
        AttachmentStoreErrorClass::InvalidInput
    );

    let invalid_rate = root.path().join("zero-rate.wav");
    let mut invalid = pcm_wav(8_000, 1, 8_000, b"INVALID");
    invalid[24..28].copy_from_slice(&0_u32.to_le_bytes());
    invalid[28..32].copy_from_slice(&0_u32.to_le_bytes());
    std::fs::write(&invalid_rate, invalid).unwrap();
    assert_eq!(
        store
            .admit_audio_path(&invalid_rate, CancellationToken::new())
            .unwrap_err()
            .class(),
        AttachmentStoreErrorClass::InvalidInput
    );
}

#[test]
fn tampered_content_and_unsafe_roots_fail_closed() {
    let (root, context) = world(1024);
    let store = context.get::<AttachmentStore>(SERVICE_ATTACHMENTS).unwrap();
    let admission = store
        .admit(
            AttachmentInput::new(b"durable".to_vec(), Some("text/plain"), Some("note.txt"))
                .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();
    let digest = admission.metadata().content_id().digest_hex();
    let object = root
        .path()
        .join("objects-root/objects/sha256")
        .join(&digest[..2])
        .join(&digest[2..]);
    std::fs::write(&object, b"tampered").unwrap();
    assert_eq!(
        store
            .read(admission.metadata(), CancellationToken::new())
            .unwrap_err()
            .class(),
        AttachmentStoreErrorClass::Corrupt
    );

    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("linked-root");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let config = AttachmentStoreConfig::new(link, 1024).unwrap();
        let plugins = vec![
            heycode_session::session_plugin(root.path().join("other-sessions")),
            local_attachment_plugin(config),
        ];
        assert!(heycode_core::compose(&plugins).is_err());
    }
}
