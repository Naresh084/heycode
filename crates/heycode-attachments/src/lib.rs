//! Durable content-addressed attachment storage.

use std::sync::{Arc, Condvar, Mutex};

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

mod local;
mod model;

pub use model::{
    AttachmentAdmission, AttachmentInput, AttachmentStoreConfig, AttachmentStoreError,
    AttachmentStoreErrorClass,
};

/// Durable attachment store service.
pub const SERVICE_ATTACHMENTS: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("attachments");
/// Explicit product default resolved by composition callers.
pub const DEFAULT_MAX_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;

/// Replaceable immutable byte backend.
pub trait AttachmentBackend: Send + Sync {
    /// Publish bytes at their validated content address without replacement.
    ///
    /// # Errors
    /// Cancellation, storage corruption or stable I/O failure.
    fn put(
        &self,
        content_id: &heycode_core::AttachmentContentId,
        bytes: &[u8],
        caller_cancellation: &CancellationToken,
        lifecycle_cancellation: &CancellationToken,
    ) -> Result<(), AttachmentStoreError>;

    /// Read one complete immutable object.
    ///
    /// # Errors
    /// Missing/corrupt content, cancellation or stable I/O failure.
    fn read(
        &self,
        content_id: &heycode_core::AttachmentContentId,
        maximum: usize,
        caller_cancellation: &CancellationToken,
        lifecycle_cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, AttachmentStoreError>;
}

struct AttachmentStoreInner {
    backend: Arc<dyn AttachmentBackend>,
    session: Arc<Mutex<heycode_session::Session>>,
    maximum_bytes: usize,
    lifecycle: CancellationToken,
    operations: OperationGate,
}

/// Content-addressed store whose successful admission durably appends metadata.
#[derive(Clone)]
pub struct AttachmentStore {
    inner: Arc<AttachmentStoreInner>,
}

impl AttachmentStore {
    /// Bind one backend to the current durable session.
    ///
    /// # Errors
    /// The service ceiling must be within 1 byte..=64 MiB.
    pub fn new(
        backend: Arc<dyn AttachmentBackend>,
        session: Arc<Mutex<heycode_session::Session>>,
        maximum_bytes: usize,
    ) -> Result<Self, AttachmentStoreError> {
        if !(1..=64 * 1024 * 1024).contains(&maximum_bytes) {
            return Err(AttachmentStoreError::invalid_input());
        }
        Ok(Self {
            inner: Arc::new(AttachmentStoreInner {
                backend,
                session,
                maximum_bytes,
                lifecycle: CancellationToken::new(),
                operations: OperationGate::new(),
            }),
        })
    }

    /// Validate, immutably store and durably append one attachment record.
    ///
    /// Bytes commit first. Metadata publishes through the session bus only
    /// after the JSONL append succeeds; an append failure may leave an
    /// unreachable immutable object for later garbage collection.
    ///
    /// # Errors
    /// Invalid size/name/MIME/image, cancellation, stopped lifecycle, storage
    /// corruption/I/O or session commit failure.
    pub fn admit(
        &self,
        input: AttachmentInput,
        cancellation: CancellationToken,
    ) -> Result<AttachmentAdmission, AttachmentStoreError> {
        let _operation = self.inner.operations.enter()?;
        self.check(&cancellation)?;
        if input.bytes().len() > self.inner.maximum_bytes {
            return Err(AttachmentStoreError::size_limit());
        }
        let inspected = inspect(input.bytes())?;
        if let Some(claimed) = input.claimed_media_type()
            && claimed != &inspected.media_type
        {
            return Err(AttachmentStoreError::mime_mismatch());
        }
        let content_id = content_id_for(input.bytes(), &cancellation, &self.inner.lifecycle)?;
        let byte_len =
            u64::try_from(input.bytes().len()).map_err(|_| AttachmentStoreError::size_limit())?;
        let mut metadata = match inspected.audio {
            Some(audio) => heycode_core::AttachmentMetadata::new_audio(
                content_id.clone(),
                inspected.media_type,
                byte_len,
                input.display_name().map(str::to_owned),
                audio,
            ),
            None => heycode_core::AttachmentMetadata::new(
                content_id.clone(),
                inspected.media_type,
                byte_len,
                input.display_name().map(str::to_owned),
                inspected.dimensions,
            ),
        }
        .map_err(|_| AttachmentStoreError::invalid_input())?;
        if let Some(source) = input.source() {
            metadata = metadata
                .with_source(source.clone())
                .map_err(|_| AttachmentStoreError::invalid_input())?;
        }
        self.inner.backend.put(
            &content_id,
            input.bytes(),
            &cancellation,
            &self.inner.lifecycle,
        )?;
        self.check(&cancellation)?;
        let event = self
            .inner
            .session
            .lock()
            .map_err(|_| AttachmentStoreError::session())?
            .append(heycode_session::SessionEventKind::AttachmentAdded {
                attachment: Box::new(metadata.clone()),
            })
            .map_err(|_| AttachmentStoreError::session())?;
        Ok(AttachmentAdmission::new(metadata, event.seq))
    }

    /// Read one explicitly selected regular file and admit its exact bytes.
    /// Symlinks and files that change while read fail without path disclosure.
    ///
    /// # Errors
    /// Invalid/unsafe path, size, cancellation, storage or session failures.
    pub fn admit_path(
        &self,
        path: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<AttachmentAdmission, AttachmentStoreError> {
        let input = self.input_from_path(path, &cancellation)?;
        self.admit(input, cancellation)
    }

    /// Read one explicitly selected regular image and admit it.
    /// Non-image content fails before storage/session mutation.
    ///
    /// # Errors
    /// Same as [`Self::admit_path`], plus unsupported non-image content.
    pub fn admit_image_path(
        &self,
        path: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<AttachmentAdmission, AttachmentStoreError> {
        let input = self.input_from_path(path, &cancellation)?;
        let inspected = inspect(input.bytes())?;
        if !inspected.media_type.is_image() {
            return Err(AttachmentStoreError::unsupported_media());
        }
        self.admit(input, cancellation)
    }

    /// Read one explicitly selected PDF/HTML document and admit it.
    /// Unsupported/opaque/text-only content fails before storage/session
    /// mutation; plain text remains a normal prompt/file capability later.
    ///
    /// # Errors
    /// Same as [`Self::admit_path`], plus unsupported document content.
    pub fn admit_document_path(
        &self,
        path: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<AttachmentAdmission, AttachmentStoreError> {
        let input = self.input_from_path(path, &cancellation)?;
        let inspected = inspect(input.bytes())?;
        if !matches!(
            inspected.media_type.as_str(),
            "application/pdf" | "text/html"
        ) {
            return Err(AttachmentStoreError::unsupported_media());
        }
        self.admit(input, cancellation)
    }

    /// Read one explicitly selected PCM WAV file and admit it.
    /// Unsupported or malformed audio fails before storage/session mutation.
    ///
    /// # Errors
    /// Same as [`Self::admit_path`], plus unsupported or invalid audio.
    pub fn admit_audio_path(
        &self,
        path: &std::path::Path,
        cancellation: CancellationToken,
    ) -> Result<AttachmentAdmission, AttachmentStoreError> {
        let input = self.input_from_path(path, &cancellation)?;
        let inspected = inspect(input.bytes())?;
        if !inspected.media_type.is_audio() || inspected.audio.is_none() {
            return Err(AttachmentStoreError::unsupported_media());
        }
        self.admit(input, cancellation)
    }

    fn input_from_path(
        &self,
        path: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> Result<AttachmentInput, AttachmentStoreError> {
        let _operation = self.inner.operations.enter()?;
        self.check(cancellation)?;
        if !path.is_absolute() {
            return Err(AttachmentStoreError::invalid_input());
        }
        let before = std::fs::symlink_metadata(path).map_err(|_| AttachmentStoreError::io())?;
        if before.file_type().is_symlink() || !before.is_file() {
            return Err(AttachmentStoreError::invalid_input());
        }
        if before.len() == 0
            || before.len() > u64::try_from(self.inner.maximum_bytes).unwrap_or(u64::MAX)
        {
            return Err(AttachmentStoreError::size_limit());
        }
        let mut file = std::fs::File::open(path).map_err(|_| AttachmentStoreError::io())?;
        let opened = file.metadata().map_err(|_| AttachmentStoreError::io())?;
        if !same_file_identity(&before, &opened) {
            return Err(AttachmentStoreError::invalid_input());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(before.len()).unwrap_or(0));
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            self.check(cancellation)?;
            use std::io::Read as _;
            let read = file
                .read(&mut buffer)
                .map_err(|_| AttachmentStoreError::io())?;
            if read == 0 {
                break;
            }
            if bytes
                .len()
                .checked_add(read)
                .is_none_or(|length| length > self.inner.maximum_bytes)
            {
                return Err(AttachmentStoreError::size_limit());
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        let after = file.metadata().map_err(|_| AttachmentStoreError::io())?;
        let current = std::fs::symlink_metadata(path).map_err(|_| AttachmentStoreError::io())?;
        if !same_file_identity(&before, &after)
            || !same_file_identity(&before, &current)
            || u64::try_from(bytes.len()).ok() != Some(before.len())
        {
            return Err(AttachmentStoreError::invalid_input());
        }
        let display_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(AttachmentStoreError::invalid_input)?;
        AttachmentInput::new(bytes, None, Some(display_name))
    }

    /// Read and independently verify one recorded object.
    ///
    /// # Errors
    /// Metadata, size, MIME/dimensions, content hash, lifecycle, cancellation
    /// or backend failures.
    pub fn read(
        &self,
        metadata: &heycode_core::AttachmentMetadata,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, AttachmentStoreError> {
        let _operation = self.inner.operations.enter()?;
        self.check(&cancellation)?;
        metadata
            .validate()
            .map_err(|_| AttachmentStoreError::corrupt())?;
        let expected =
            usize::try_from(metadata.byte_len()).map_err(|_| AttachmentStoreError::corrupt())?;
        if expected > self.inner.maximum_bytes {
            return Err(AttachmentStoreError::corrupt());
        }
        let bytes = self.inner.backend.read(
            metadata.content_id(),
            expected,
            &cancellation,
            &self.inner.lifecycle,
        )?;
        self.check(&cancellation)?;
        if bytes.len() != expected {
            return Err(AttachmentStoreError::corrupt());
        }
        let actual = content_id_for(&bytes, &cancellation, &self.inner.lifecycle)?;
        if &actual != metadata.content_id() {
            return Err(AttachmentStoreError::corrupt());
        }
        let inspected = inspect(&bytes).map_err(|_| AttachmentStoreError::corrupt())?;
        if &inspected.media_type != metadata.media_type()
            || inspected.dimensions != metadata.dimensions()
            || inspected.audio != metadata.audio()
        {
            return Err(AttachmentStoreError::corrupt());
        }
        Ok(bytes)
    }

    fn check(&self, cancellation: &CancellationToken) -> Result<(), AttachmentStoreError> {
        if self.inner.lifecycle.is_cancelled() {
            Err(AttachmentStoreError::stopped())
        } else if cancellation.is_cancelled() {
            Err(AttachmentStoreError::cancelled())
        } else {
            Ok(())
        }
    }

    fn shutdown(&self) {
        self.inner.lifecycle.cancel();
        self.inner.operations.shutdown();
    }
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.is_file()
        && right.is_file()
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
}

struct OperationGate {
    state: Mutex<OperationState>,
    settled: Condvar,
}

struct OperationState {
    active: usize,
    stopped: bool,
}

impl OperationGate {
    const fn new() -> Self {
        Self {
            state: Mutex::new(OperationState {
                active: 0,
                stopped: false,
            }),
            settled: Condvar::new(),
        }
    }

    fn enter(&self) -> Result<OperationLease<'_>, AttachmentStoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AttachmentStoreError::unavailable())?;
        if state.stopped {
            return Err(AttachmentStoreError::stopped());
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or_else(AttachmentStoreError::unavailable)?;
        Ok(OperationLease { gate: self })
    }

    fn shutdown(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.stopped = true;
        while state.active != 0 {
            let Ok(next) = self.settled.wait(state) else {
                return;
            };
            state = next;
        }
    }
}

struct OperationLease<'a> {
    gate: &'a OperationGate,
}

impl Drop for OperationLease<'_> {
    fn drop(&mut self) {
        let Ok(mut state) = self.gate.state.lock() else {
            return;
        };
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            self.gate.settled.notify_all();
        }
    }
}

struct InspectedAttachment {
    media_type: heycode_core::AttachmentMediaType,
    dimensions: Option<heycode_core::AttachmentDimensions>,
    audio: Option<heycode_core::AttachmentAudioMetadata>,
}

fn inspect(bytes: &[u8]) -> Result<InspectedAttachment, AttachmentStoreError> {
    if bytes.is_empty() {
        return Err(AttachmentStoreError::invalid_input());
    }
    if let Ok(format) = image::guess_format(bytes) {
        let (media_type, format) = match format {
            image::ImageFormat::Png => ("image/png", image::ImageFormat::Png),
            image::ImageFormat::Jpeg => ("image/jpeg", image::ImageFormat::Jpeg),
            image::ImageFormat::Gif => ("image/gif", image::ImageFormat::Gif),
            image::ImageFormat::WebP => ("image/webp", image::ImageFormat::WebP),
            _ => return Err(AttachmentStoreError::unsupported_media()),
        };
        let reader = image::ImageReader::with_format(
            std::io::BufReader::new(std::io::Cursor::new(bytes)),
            format,
        );
        let (width, height) = reader
            .into_dimensions()
            .map_err(|_| AttachmentStoreError::invalid_image())?;
        let dimensions = heycode_core::AttachmentDimensions::new(width, height)
            .map_err(|_| AttachmentStoreError::size_limit())?;
        return Ok(InspectedAttachment {
            media_type: heycode_core::AttachmentMediaType::new(media_type)
                .map_err(|_| AttachmentStoreError::invalid_input())?,
            dimensions: Some(dimensions),
            audio: None,
        });
    }
    if bytes.starts_with(b"RIFF") || bytes.get(8..12) == Some(b"WAVE") {
        let audio = inspect_pcm_wav(bytes)?;
        return Ok(InspectedAttachment {
            media_type: heycode_core::AttachmentMediaType::new("audio/wav")
                .map_err(|_| AttachmentStoreError::invalid_audio())?,
            dimensions: None,
            audio: Some(audio),
        });
    }
    let media_type = if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if let Ok(text) = std::str::from_utf8(bytes)
        && !bytes.contains(&0)
    {
        if looks_like_html(text) {
            "text/html"
        } else {
            "text/plain"
        }
    } else {
        "application/octet-stream"
    };
    Ok(InspectedAttachment {
        media_type: heycode_core::AttachmentMediaType::new(media_type)
            .map_err(|_| AttachmentStoreError::invalid_input())?,
        dimensions: None,
        audio: None,
    })
}

/// Validate bounded PCM WAV bytes without admitting an attachment or recording a session event.
/// Local speech adapters use this same parser before supplying explicitly selected audio to STT.
///
/// # Errors
/// Invalid RIFF layout, non-PCM encoding, inconsistent format/data lengths or unsupported audio facts.
pub fn validate_pcm_wav(
    bytes: &[u8],
) -> Result<heycode_core::AttachmentAudioMetadata, AttachmentStoreError> {
    inspect_pcm_wav(bytes)
}

fn inspect_pcm_wav(
    bytes: &[u8],
) -> Result<heycode_core::AttachmentAudioMetadata, AttachmentStoreError> {
    if bytes.len() < 44 || bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(AttachmentStoreError::invalid_audio());
    }
    let riff_size = read_u32_le(bytes, 4)?;
    let expected = usize::try_from(riff_size)
        .ok()
        .and_then(|size| size.checked_add(8))
        .ok_or_else(AttachmentStoreError::invalid_audio)?;
    if expected != bytes.len() {
        return Err(AttachmentStoreError::invalid_audio());
    }

    let mut offset = 12_usize;
    let mut format = None;
    let mut data_len = None;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(AttachmentStoreError::invalid_audio)?;
        if header_end > bytes.len() {
            return Err(AttachmentStoreError::invalid_audio());
        }
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = usize::try_from(read_u32_le(bytes, offset + 4)?)
            .map_err(|_| AttachmentStoreError::invalid_audio())?;
        let data_start = header_end;
        let data_end = data_start
            .checked_add(chunk_len)
            .ok_or_else(AttachmentStoreError::invalid_audio)?;
        if data_end > bytes.len() {
            return Err(AttachmentStoreError::invalid_audio());
        }
        match chunk_id {
            b"fmt " if format.is_none() => {
                if chunk_len < 16 {
                    return Err(AttachmentStoreError::invalid_audio());
                }
                format = Some((
                    read_u16_le(bytes, data_start)?,
                    read_u16_le(bytes, data_start + 2)?,
                    read_u32_le(bytes, data_start + 4)?,
                    read_u32_le(bytes, data_start + 8)?,
                    read_u16_le(bytes, data_start + 12)?,
                    read_u16_le(bytes, data_start + 14)?,
                ));
            }
            b"fmt " => return Err(AttachmentStoreError::invalid_audio()),
            b"data" if data_len.is_none() => data_len = Some(chunk_len),
            b"data" => return Err(AttachmentStoreError::invalid_audio()),
            _ => {}
        }
        offset = data_end
            .checked_add(chunk_len & 1)
            .ok_or_else(AttachmentStoreError::invalid_audio)?;
    }
    if offset != bytes.len() {
        return Err(AttachmentStoreError::invalid_audio());
    }
    let Some((encoding, channels, sample_rate, byte_rate, block_align, bits_per_sample)) = format
    else {
        return Err(AttachmentStoreError::invalid_audio());
    };
    let Some(data_len) = data_len else {
        return Err(AttachmentStoreError::invalid_audio());
    };
    let bytes_per_sample = bits_per_sample / 8;
    let expected_align = channels.checked_mul(bytes_per_sample);
    let expected_rate = sample_rate.checked_mul(u32::from(block_align));
    if encoding != 1
        || channels == 0
        || sample_rate == 0
        || block_align == 0
        || data_len == 0
        || bytes_per_sample == 0
        || expected_align != Some(block_align)
        || expected_rate != Some(byte_rate)
        || data_len % usize::from(block_align) != 0
    {
        return Err(AttachmentStoreError::invalid_audio());
    }
    let frames = data_len / usize::from(block_align);
    let duration_ms = u64::try_from(frames)
        .ok()
        .and_then(|frames| frames.checked_mul(1_000))
        .map(|scaled| scaled / u64::from(sample_rate))
        .ok_or_else(AttachmentStoreError::invalid_audio)?;
    heycode_core::AttachmentAudioMetadata::new(duration_ms, sample_rate, channels, bits_per_sample)
        .map_err(|_| AttachmentStoreError::invalid_audio())
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, AttachmentStoreError> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .and_then(|value| <[u8; 2]>::try_from(value).ok())
        .ok_or_else(AttachmentStoreError::invalid_audio)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, AttachmentStoreError> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .and_then(|value| <[u8; 4]>::try_from(value).ok())
        .ok_or_else(AttachmentStoreError::invalid_audio)?;
    Ok(u32::from_le_bytes(value))
}

fn looks_like_html(text: &str) -> bool {
    let prefix = text
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .chars()
        .take(1_024)
        .collect::<String>()
        .to_ascii_lowercase();
    ["<!doctype html", "<html", "<head", "<body"]
        .iter()
        .any(|marker| prefix.starts_with(marker) || prefix.contains(&format!(">{marker}")))
}

fn content_id_for(
    bytes: &[u8],
    caller_cancellation: &CancellationToken,
    lifecycle_cancellation: &CancellationToken,
) -> Result<heycode_core::AttachmentContentId, AttachmentStoreError> {
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(1024 * 1024) {
        if caller_cancellation.is_cancelled() || lifecycle_cancellation.is_cancelled() {
            return Err(AttachmentStoreError::cancelled());
        }
        digest.update(chunk);
    }
    Ok(heycode_core::AttachmentContentId::from_sha256(
        digest.finalize().into(),
    ))
}

/// Mount the audited local owner-only attachment provider.
#[must_use]
pub fn local_attachment_plugin(config: AttachmentStoreConfig) -> Box<dyn Plugin> {
    struct LocalAttachmentPlugin(AttachmentStoreConfig);

    impl Plugin for LocalAttachmentPlugin {
        fn name(&self) -> &'static str {
            "attachments-local"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_session::SERVICE_SESSION]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_ATTACHMENTS]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let session = context
                .get::<Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| CoreError::other("session service type mismatch"))?;
            let backend = local::LocalAttachmentBackend::open(self.0.root())
                .map_err(|error| CoreError::other(error.to_string()))?;
            let store = AttachmentStore::new(Arc::new(backend), session, self.0.maximum_bytes())
                .map_err(|error| CoreError::other(error.to_string()))?;
            context.provide(SERVICE_ATTACHMENTS, self.name(), store.clone())?;
            context.effect(move || store.shutdown());
            Ok(())
        }
    }

    Box::new(LocalAttachmentPlugin(config))
}
