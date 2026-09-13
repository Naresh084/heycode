//! Validated content-addressed attachment vocabulary shared across boundaries.

use serde::{Deserialize, Deserializer, Serialize};

const SHA256_PREFIX: &str = "sha256-";
const SHA256_HEX_BYTES: usize = 64;
const MAX_MEDIA_TYPE_BYTES: usize = 127;
const MAX_DISPLAY_NAME_BYTES: usize = 255;
const MAX_ATTACHMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 32_768;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_SOURCE_TITLE_BYTES: usize = 512;
const MAX_AUDIO_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;
const MIN_AUDIO_SAMPLE_RATE_HZ: u32 = 8_000;
const MAX_AUDIO_SAMPLE_RATE_HZ: u32 = 384_000;
const MAX_AUDIO_CHANNELS: u16 = 8;

/// Validated SHA-256 content address.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AttachmentContentId(String);

impl AttachmentContentId {
    /// Parse one canonical `sha256-<64 lowercase hex>` address.
    ///
    /// # Errors
    /// Wrong algorithm, length, case or non-hex bytes fail.
    pub fn new(value: impl Into<String>) -> Result<Self, AttachmentMetadataError> {
        let value = value.into();
        let Some(hex) = value.strip_prefix(SHA256_PREFIX) else {
            return Err(AttachmentMetadataError::InvalidContentId);
        };
        if hex.len() != SHA256_HEX_BYTES
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(AttachmentMetadataError::InvalidContentId);
        }
        Ok(Self(value))
    }

    /// Construct the canonical address for one SHA-256 digest.
    #[must_use]
    pub fn from_sha256(digest: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(SHA256_PREFIX.len() + SHA256_HEX_BYTES);
        value.push_str(SHA256_PREFIX);
        for byte in digest {
            value.push(char::from(HEX[usize::from(byte >> 4)]));
            value.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Self(value)
    }

    /// Full canonical address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lowercase digest component used by immutable storage providers.
    #[must_use]
    pub fn digest_hex(&self) -> &str {
        &self.0[SHA256_PREFIX.len()..]
    }
}

impl std::fmt::Debug for AttachmentContentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AttachmentContentId(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for AttachmentContentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Canonical lowercase MIME media type without parameters.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AttachmentMediaType(String);

impl AttachmentMediaType {
    /// Parse and lowercase one `type/subtype` token.
    ///
    /// # Errors
    /// Whitespace, parameters, controls, missing/extra slash or unsafe token
    /// bytes fail.
    pub fn new(value: impl Into<String>) -> Result<Self, AttachmentMetadataError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_MEDIA_TYPE_BYTES
            || value.trim() != value
            || !value.is_ascii()
        {
            return Err(AttachmentMetadataError::InvalidMediaType);
        }
        let canonical = value.to_ascii_lowercase();
        let Some((top, subtype)) = canonical.split_once('/') else {
            return Err(AttachmentMetadataError::InvalidMediaType);
        };
        if subtype.contains('/') || !valid_mime_token(top) || !valid_mime_token(subtype) {
            return Err(AttachmentMetadataError::InvalidMediaType);
        }
        Ok(Self(canonical))
    }

    /// Canonical lowercase media type.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this media type requires validated raster dimensions.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.0.starts_with("image/")
    }

    /// Whether this media type requires validated audio stream metadata.
    #[must_use]
    pub fn is_audio(&self) -> bool {
        self.0.starts_with("audio/")
    }
}

impl std::fmt::Debug for AttachmentMediaType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("AttachmentMediaType")
            .field(&self.0)
            .finish()
    }
}

impl<'de> Deserialize<'de> for AttachmentMediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Validated raster dimensions protected against decompression bombs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AttachmentDimensions {
    width: u32,
    height: u32,
}

impl AttachmentDimensions {
    /// Construct bounded nonzero dimensions.
    ///
    /// # Errors
    /// Zero, either dimension above 32,768 or more than 100 million pixels
    /// fails.
    pub fn new(width: u32, height: u32) -> Result<Self, AttachmentMetadataError> {
        let pixels = u64::from(width).checked_mul(u64::from(height));
        if width == 0
            || height == 0
            || width > MAX_IMAGE_DIMENSION
            || height > MAX_IMAGE_DIMENSION
            || pixels.is_none_or(|pixels| pixels > MAX_IMAGE_PIXELS)
        {
            return Err(AttachmentMetadataError::InvalidDimensions);
        }
        Ok(Self { width, height })
    }

    /// Pixel width.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Pixel height.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }
}

impl<'de> Deserialize<'de> for AttachmentDimensions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            width: u32,
            height: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.width, wire.height).map_err(serde::de::Error::custom)
    }
}

/// Validated audio stream facts without encoded sample bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AttachmentAudioMetadata {
    duration_ms: u64,
    sample_rate_hz: u32,
    channels: u16,
    bits_per_sample: u16,
}

impl AttachmentAudioMetadata {
    /// Construct bounded nonzero audio metadata.
    ///
    /// # Errors
    /// Duration must be at most 24 hours, sample rate 8–384 kHz, channels
    /// 1–8, and PCM depth one of 8/16/24/32 bits.
    pub fn new(
        duration_ms: u64,
        sample_rate_hz: u32,
        channels: u16,
        bits_per_sample: u16,
    ) -> Result<Self, AttachmentMetadataError> {
        if !(1..=MAX_AUDIO_DURATION_MS).contains(&duration_ms)
            || !(MIN_AUDIO_SAMPLE_RATE_HZ..=MAX_AUDIO_SAMPLE_RATE_HZ).contains(&sample_rate_hz)
            || !(1..=MAX_AUDIO_CHANNELS).contains(&channels)
            || !matches!(bits_per_sample, 8 | 16 | 24 | 32)
        {
            return Err(AttachmentMetadataError::InvalidAudioMetadata);
        }
        Ok(Self {
            duration_ms,
            sample_rate_hz,
            channels,
            bits_per_sample,
        })
    }

    /// Rounded-down duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        self.duration_ms
    }

    /// Samples per second.
    #[must_use]
    pub const fn sample_rate_hz(self) -> u32 {
        self.sample_rate_hz
    }

    /// Interleaved channel count.
    #[must_use]
    pub const fn channels(self) -> u16 {
        self.channels
    }

    /// PCM bits per sample per channel.
    #[must_use]
    pub const fn bits_per_sample(self) -> u16 {
        self.bits_per_sample
    }
}

impl<'de> Deserialize<'de> for AttachmentAudioMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            duration_ms: u64,
            sample_rate_hz: u32,
            channels: u16,
            bits_per_sample: u16,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.duration_ms,
            wire.sample_rate_hz,
            wire.channels,
            wire.bits_per_sample,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Optional durable HTTP provenance for a stored attachment.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentSourceMetadata {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    retrieved_at_ms: i64,
    source_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_count: Option<u32>,
}

impl AttachmentSourceMetadata {
    /// Construct bounded public HTTP source provenance.
    ///
    /// # Errors
    /// Unsafe URL/title, negative timestamp or page count outside 1..=10,000
    /// fails.
    pub fn new(
        url: impl Into<String>,
        title: Option<String>,
        retrieved_at_ms: i64,
        source_truncated: bool,
        page_count: Option<u32>,
    ) -> Result<Self, AttachmentMetadataError> {
        let source = Self {
            url: url.into(),
            title,
            retrieved_at_ms,
            source_truncated,
            page_count,
        };
        source.validate()?;
        Ok(source)
    }

    /// Revalidate boundary-deserialized provenance.
    ///
    /// # Errors
    /// Any constructor invariant violation fails.
    pub fn validate(&self) -> Result<(), AttachmentMetadataError> {
        let url = url::Url::parse(&self.url).map_err(|_| AttachmentMetadataError::InvalidSource)?;
        if self.url.len() > 4_096
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || self.url.chars().any(char::is_control)
            || self.url.chars().any(char::is_whitespace)
            || self.retrieved_at_ms < 0
            || self
                .page_count
                .is_some_and(|pages| !(1..=10_000).contains(&pages))
        {
            return Err(AttachmentMetadataError::InvalidSource);
        }
        if let Some(title) = &self.title
            && (title.is_empty()
                || title.len() > MAX_SOURCE_TITLE_BYTES
                || title.trim() != title
                || title.chars().any(char::is_control))
        {
            return Err(AttachmentMetadataError::InvalidSource);
        }
        Ok(())
    }

    /// Final public source URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Extracted title, when present.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Retrieval instant in Unix milliseconds.
    #[must_use]
    pub const fn retrieved_at_ms(&self) -> i64 {
        self.retrieved_at_ms
    }

    /// Whether transport discarded raw source bytes before hashing.
    #[must_use]
    pub const fn source_truncated(&self) -> bool {
        self.source_truncated
    }

    /// PDF page count, when known.
    #[must_use]
    pub const fn page_count(&self) -> Option<u32> {
        self.page_count
    }
}

impl std::fmt::Debug for AttachmentSourceMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentSourceMetadata")
            .field("url", &"<redacted>")
            .field("has_title", &self.title.is_some())
            .field("retrieved_at_ms", &self.retrieved_at_ms)
            .field("source_truncated", &self.source_truncated)
            .field("page_count", &self.page_count)
            .finish()
    }
}

impl<'de> Deserialize<'de> for AttachmentSourceMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            url: String,
            title: Option<String>,
            retrieved_at_ms: i64,
            source_truncated: bool,
            page_count: Option<u32>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.url,
            wire.title,
            wire.retrieved_at_ms,
            wire.source_truncated,
            wire.page_count,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Durable attachment metadata stored in the session log.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentMetadata {
    content_id: AttachmentContentId,
    media_type: AttachmentMediaType,
    byte_len: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<AttachmentDimensions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<AttachmentAudioMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<AttachmentSourceMetadata>,
}

impl AttachmentMetadata {
    /// Construct one bounded coherent durable record.
    ///
    /// # Errors
    /// Empty/oversized bytes, unsafe display name or image/dimension mismatch
    /// fails.
    pub fn new(
        content_id: AttachmentContentId,
        media_type: AttachmentMediaType,
        byte_len: u64,
        display_name: Option<String>,
        dimensions: Option<AttachmentDimensions>,
    ) -> Result<Self, AttachmentMetadataError> {
        let metadata = Self {
            content_id,
            media_type,
            byte_len,
            display_name,
            dimensions,
            audio: None,
            source: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Construct one bounded coherent durable audio record.
    ///
    /// # Errors
    /// Media type must be audio, bytes/name and audio facts must validate,
    /// and raster dimensions are structurally absent.
    pub fn new_audio(
        content_id: AttachmentContentId,
        media_type: AttachmentMediaType,
        byte_len: u64,
        display_name: Option<String>,
        audio: AttachmentAudioMetadata,
    ) -> Result<Self, AttachmentMetadataError> {
        let metadata = Self {
            content_id,
            media_type,
            byte_len,
            display_name,
            dimensions: None,
            audio: Some(audio),
            source: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Revalidate boundary-deserialized metadata.
    ///
    /// # Errors
    /// Any constructor invariant violation fails.
    pub fn validate(&self) -> Result<(), AttachmentMetadataError> {
        if self.byte_len == 0 || self.byte_len > MAX_ATTACHMENT_BYTES {
            return Err(AttachmentMetadataError::InvalidByteLength);
        }
        if let Some(name) = &self.display_name
            && (name.is_empty()
                || name.len() > MAX_DISPLAY_NAME_BYTES
                || name.trim() != name
                || matches!(name.as_str(), "." | "..")
                || name.contains(['/', '\\'])
                || name.chars().any(char::is_control))
        {
            return Err(AttachmentMetadataError::InvalidDisplayName);
        }
        if self.media_type.is_image() != self.dimensions.is_some() {
            return Err(AttachmentMetadataError::IncoherentDimensions);
        }
        if self.media_type.is_audio() != self.audio.is_some() {
            return Err(AttachmentMetadataError::IncoherentAudioMetadata);
        }
        if let Some(source) = &self.source {
            source.validate()?;
        }
        Ok(())
    }

    /// Attach already-validated durable source provenance.
    ///
    /// # Errors
    /// Full metadata revalidation failure.
    pub fn with_source(
        mut self,
        source: AttachmentSourceMetadata,
    ) -> Result<Self, AttachmentMetadataError> {
        self.source = Some(source);
        self.validate()?;
        Ok(self)
    }

    /// Content address.
    #[must_use]
    pub fn content_id(&self) -> &AttachmentContentId {
        &self.content_id
    }

    /// Canonical MIME media type.
    #[must_use]
    pub fn media_type(&self) -> &AttachmentMediaType {
        &self.media_type
    }

    /// Exact admitted byte count.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// Safe basename-like display label, when supplied.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// Validated raster dimensions, for image media types.
    #[must_use]
    pub const fn dimensions(&self) -> Option<AttachmentDimensions> {
        self.dimensions
    }

    /// Validated audio stream facts for audio media types.
    #[must_use]
    pub const fn audio(&self) -> Option<AttachmentAudioMetadata> {
        self.audio
    }

    /// Durable HTTP provenance, when this attachment came from retrieval.
    #[must_use]
    pub fn source(&self) -> Option<&AttachmentSourceMetadata> {
        self.source.as_ref()
    }
}

impl std::fmt::Debug for AttachmentMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentMetadata")
            .field("content_id", &"<redacted>")
            .field("media_type", &self.media_type)
            .field("byte_len", &self.byte_len)
            .field("has_display_name", &self.display_name.is_some())
            .field("dimensions", &self.dimensions)
            .field("audio", &self.audio)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl<'de> Deserialize<'de> for AttachmentMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            content_id: AttachmentContentId,
            media_type: AttachmentMediaType,
            byte_len: u64,
            display_name: Option<String>,
            dimensions: Option<AttachmentDimensions>,
            audio: Option<AttachmentAudioMetadata>,
            source: Option<AttachmentSourceMetadata>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let metadata = match wire.audio {
            Some(audio) if wire.dimensions.is_none() => Self::new_audio(
                wire.content_id,
                wire.media_type,
                wire.byte_len,
                wire.display_name,
                audio,
            ),
            Some(_) => Err(AttachmentMetadataError::IncoherentAudioMetadata),
            None => Self::new(
                wire.content_id,
                wire.media_type,
                wire.byte_len,
                wire.display_name,
                wire.dimensions,
            ),
        }
        .map_err(serde::de::Error::custom)?;
        match wire.source {
            Some(source) => metadata
                .with_source(source)
                .map_err(serde::de::Error::custom),
            None => Ok(metadata),
        }
    }
}

/// Durable provider-input route chosen for one document attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentInputRouteKind {
    /// Send the exact admitted PDF bytes through a protocol-native file block.
    Native,
    /// Send bounded UTF-8 text durably derived from the admitted source.
    Extracted,
}

/// Exact source and selected-object facts for one document route.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DocumentInputRoute {
    kind: DocumentInputRouteKind,
    source: AttachmentMetadata,
    selected: AttachmentMetadata,
}

impl DocumentInputRoute {
    /// Select exact PDF bytes for a protocol-native document block.
    ///
    /// # Errors
    /// Non-PDF metadata is not a native document route.
    pub fn native(source: AttachmentMetadata) -> Result<Self, AttachmentMetadataError> {
        Self::new(DocumentInputRouteKind::Native, source.clone(), source)
    }

    /// Select durable extracted text while retaining its exact source record.
    ///
    /// # Errors
    /// Source must be PDF/HTML, selected content must be distinct plain text,
    /// and both metadata records must remain valid.
    pub fn extracted(
        source: AttachmentMetadata,
        selected: AttachmentMetadata,
    ) -> Result<Self, AttachmentMetadataError> {
        Self::new(DocumentInputRouteKind::Extracted, source, selected)
    }

    fn new(
        kind: DocumentInputRouteKind,
        source: AttachmentMetadata,
        selected: AttachmentMetadata,
    ) -> Result<Self, AttachmentMetadataError> {
        source.validate()?;
        selected.validate()?;
        let valid = match kind {
            DocumentInputRouteKind::Native => {
                source.media_type().as_str() == "application/pdf" && source == selected
            }
            DocumentInputRouteKind::Extracted => {
                matches!(
                    source.media_type().as_str(),
                    "application/pdf" | "text/html" | "application/xhtml+xml"
                ) && selected.media_type().as_str() == "text/plain"
                    && source.content_id() != selected.content_id()
            }
        };
        if !valid {
            return Err(AttachmentMetadataError::InvalidDocumentRoute);
        }
        Ok(Self {
            kind,
            source,
            selected,
        })
    }

    /// Resolved route kind.
    #[must_use]
    pub const fn kind(&self) -> DocumentInputRouteKind {
        self.kind
    }

    /// Original admitted document metadata.
    #[must_use]
    pub const fn source(&self) -> &AttachmentMetadata {
        &self.source
    }

    /// Exact metadata selected into the provider-visible user input.
    #[must_use]
    pub const fn selected(&self) -> &AttachmentMetadata {
        &self.selected
    }

    /// Revalidate a deserialized route.
    ///
    /// # Errors
    /// Any constructor invariant violation fails.
    pub fn validate(&self) -> Result<(), AttachmentMetadataError> {
        Self::new(self.kind, self.source.clone(), self.selected.clone()).map(|_| ())
    }
}

impl std::fmt::Debug for DocumentInputRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DocumentInputRoute")
            .field("kind", &self.kind)
            .field("source_media_type", &self.source.media_type())
            .field("selected_media_type", &self.selected.media_type())
            .finish()
    }
}

impl<'de> Deserialize<'de> for DocumentInputRoute {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: DocumentInputRouteKind,
            source: AttachmentMetadata,
            selected: AttachmentMetadata,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.kind, wire.source, wire.selected).map_err(serde::de::Error::custom)
    }
}

/// Stable attachment metadata validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttachmentMetadataError {
    /// Content address is not canonical SHA-256.
    #[error("attachment content id is invalid")]
    InvalidContentId,
    /// MIME media type is unsafe or malformed.
    #[error("attachment media type is invalid")]
    InvalidMediaType,
    /// Raster dimensions exceed the admission bounds.
    #[error("attachment dimensions are invalid")]
    InvalidDimensions,
    /// Byte count is empty or exceeds 64 MiB.
    #[error("attachment byte length is invalid")]
    InvalidByteLength,
    /// Display label is not a bounded portable basename.
    #[error("attachment display name is invalid")]
    InvalidDisplayName,
    /// Image metadata lacks dimensions or non-image metadata includes them.
    #[error("attachment dimensions do not match the media type")]
    IncoherentDimensions,
    /// Audio metadata is missing for audio or present for another media type.
    #[error("attachment audio metadata does not match the media type")]
    IncoherentAudioMetadata,
    /// Audio duration/rate/channel/sample metadata is outside supported bounds.
    #[error("attachment audio metadata is invalid")]
    InvalidAudioMetadata,
    /// HTTP source provenance is unsafe or malformed.
    #[error("attachment source metadata is invalid")]
    InvalidSource,
    /// Document route source/selection/mode is incoherent.
    #[error("attachment document route is invalid")]
    InvalidDocumentRoute,
}

fn valid_mime_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}
