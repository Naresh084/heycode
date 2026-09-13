//! Pre-commit rich tool output carried from a Tool to the Agent owner.

const MAX_BLOCKS: usize = 256;
const MAX_MEDIA_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_MEDIA_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;

/// Decoded media awaiting durable attachment admission.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingToolMedia {
    declared_media_type: Option<heycode_core::AttachmentMediaType>,
    bytes: Vec<u8>,
}

impl PendingToolMedia {
    /// Construct one bounded non-empty media body.
    ///
    /// # Errors
    /// Empty or over-8-MiB input.
    pub fn new(
        declared_media_type: Option<heycode_core::AttachmentMediaType>,
        bytes: Vec<u8>,
    ) -> Result<Self, PendingRichToolResultError> {
        if bytes.is_empty() || bytes.len() > MAX_MEDIA_BYTES {
            return Err(PendingRichToolResultError::TooLarge);
        }
        Ok(Self {
            declared_media_type,
            bytes,
        })
    }

    /// Protocol-declared media type.
    #[must_use]
    pub const fn declared_media_type(&self) -> Option<&heycode_core::AttachmentMediaType> {
        self.declared_media_type.as_ref()
    }

    /// Decoded bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume into declared type and bytes.
    #[must_use]
    pub fn into_parts(self) -> (Option<heycode_core::AttachmentMediaType>, Vec<u8>) {
        (self.declared_media_type, self.bytes)
    }
}

impl std::fmt::Debug for PendingToolMedia {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingToolMedia")
            .field("declared_media_type", &self.declared_media_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// One ordered rich block before media reaches ATT01.
#[derive(Clone, PartialEq, Eq)]
pub enum PendingToolResultBlock {
    /// Verbatim text.
    Text {
        /// Text body.
        text: String,
        /// Exact block metadata.
        metadata: heycode_core::ToolResultBlockMetadata,
    },
    /// Decoded image.
    Image {
        /// Pending media.
        media: PendingToolMedia,
        /// Exact block metadata.
        metadata: heycode_core::ToolResultBlockMetadata,
    },
    /// Decoded audio.
    Audio {
        /// Pending media.
        media: PendingToolMedia,
        /// Exact block metadata.
        metadata: heycode_core::ToolResultBlockMetadata,
    },
    /// Resource link.
    ResourceLink {
        /// Complete link and metadata.
        link: heycode_core::ToolResultResourceLink,
    },
    /// Embedded textual resource.
    EmbeddedText {
        /// Absolute resource URI.
        uri: String,
        /// Optional declared MIME type.
        mime_type: Option<heycode_core::AttachmentMediaType>,
        /// Verbatim resource text.
        text: String,
        /// Bounded members of the embedded resource object.
        resource_extensions: serde_json::Map<String, serde_json::Value>,
        /// Exact block metadata.
        metadata: heycode_core::ToolResultBlockMetadata,
    },
    /// Embedded binary resource.
    EmbeddedBlob {
        /// Absolute resource URI.
        uri: String,
        /// Pending bytes.
        media: PendingToolMedia,
        /// Bounded members of the embedded resource object.
        resource_extensions: serde_json::Map<String, serde_json::Value>,
        /// Exact block metadata.
        metadata: heycode_core::ToolResultBlockMetadata,
    },
}

impl std::fmt::Debug for PendingToolResultBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { text, metadata } => formatter
                .debug_struct("Text")
                .field("bytes", &text.len())
                .field("metadata", metadata)
                .finish(),
            Self::Image { media, metadata } => formatter
                .debug_struct("Image")
                .field("media", media)
                .field("metadata", metadata)
                .finish(),
            Self::Audio { media, metadata } => formatter
                .debug_struct("Audio")
                .field("media", media)
                .field("metadata", metadata)
                .finish(),
            Self::ResourceLink { link } => {
                formatter.debug_tuple("ResourceLink").field(link).finish()
            }
            Self::EmbeddedText { text, metadata, .. } => formatter
                .debug_struct("EmbeddedText")
                .field("uri", &"[REDACTED]")
                .field("bytes", &text.len())
                .field("metadata", metadata)
                .finish(),
            Self::EmbeddedBlob {
                media, metadata, ..
            } => formatter
                .debug_struct("EmbeddedBlob")
                .field("uri", &"[REDACTED]")
                .field("media", media)
                .field("metadata", metadata)
                .finish(),
        }
    }
}

/// Complete ordered result before durable media admission.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingRichToolResult {
    blocks: Vec<PendingToolResultBlock>,
    structured_content: heycode_core::ToolStructuredContent,
    schema_check: heycode_core::ToolResultSchemaCheck,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl PendingRichToolResult {
    /// Construct one bounded pending result.
    ///
    /// # Errors
    /// Empty/oversized block, text, media or extension collections.
    pub fn new(
        blocks: Vec<PendingToolResultBlock>,
        structured_content: heycode_core::ToolStructuredContent,
        schema_check: heycode_core::ToolResultSchemaCheck,
        extensions: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, PendingRichToolResultError> {
        if blocks.len() > MAX_BLOCKS || extensions.len() > 64 {
            return Err(PendingRichToolResultError::Invalid);
        }
        let mut text_bytes = 0_usize;
        let mut media_bytes = 0_usize;
        for block in &blocks {
            match block {
                PendingToolResultBlock::Text { text, metadata } => {
                    validate_text(text)?;
                    metadata
                        .validate()
                        .map_err(|_| PendingRichToolResultError::Invalid)?;
                    text_bytes = text_bytes
                        .checked_add(text.len())
                        .ok_or(PendingRichToolResultError::TooLarge)?;
                }
                PendingToolResultBlock::Image { media, metadata } => {
                    if media
                        .declared_media_type()
                        .is_none_or(|value| !value.is_image())
                    {
                        return Err(PendingRichToolResultError::Invalid);
                    }
                    metadata
                        .validate()
                        .map_err(|_| PendingRichToolResultError::Invalid)?;
                    media_bytes = media_bytes
                        .checked_add(media.bytes().len())
                        .ok_or(PendingRichToolResultError::TooLarge)?;
                }
                PendingToolResultBlock::Audio { media, metadata } => {
                    if media
                        .declared_media_type()
                        .is_none_or(|value| !value.as_str().starts_with("audio/"))
                    {
                        return Err(PendingRichToolResultError::Invalid);
                    }
                    metadata
                        .validate()
                        .map_err(|_| PendingRichToolResultError::Invalid)?;
                    media_bytes = media_bytes
                        .checked_add(media.bytes().len())
                        .ok_or(PendingRichToolResultError::TooLarge)?;
                }
                PendingToolResultBlock::ResourceLink { link } => link
                    .validate()
                    .map_err(|_| PendingRichToolResultError::Invalid)?,
                PendingToolResultBlock::EmbeddedText {
                    uri,
                    text,
                    resource_extensions,
                    metadata,
                    ..
                } => {
                    validate_uri(uri)?;
                    validate_text(text)?;
                    validate_extensions(resource_extensions)?;
                    metadata
                        .validate()
                        .map_err(|_| PendingRichToolResultError::Invalid)?;
                    text_bytes = text_bytes
                        .checked_add(text.len())
                        .ok_or(PendingRichToolResultError::TooLarge)?;
                }
                PendingToolResultBlock::EmbeddedBlob {
                    uri,
                    media,
                    resource_extensions,
                    metadata,
                } => {
                    validate_uri(uri)?;
                    validate_extensions(resource_extensions)?;
                    metadata
                        .validate()
                        .map_err(|_| PendingRichToolResultError::Invalid)?;
                    media_bytes = media_bytes
                        .checked_add(media.bytes().len())
                        .ok_or(PendingRichToolResultError::TooLarge)?;
                }
            }
        }
        if text_bytes > MAX_TEXT_BYTES
            || media_bytes > MAX_TOTAL_MEDIA_BYTES
            || serde_json::to_vec(&extensions).map_or(usize::MAX, |bytes| bytes.len())
                > MAX_TEXT_BYTES
        {
            return Err(PendingRichToolResultError::TooLarge);
        }
        structured_content
            .validate()
            .map_err(|_| PendingRichToolResultError::TooLarge)?;
        schema_check
            .validate()
            .map_err(|_| PendingRichToolResultError::Invalid)?;
        validate_extensions(&extensions)?;
        Ok(Self {
            blocks,
            structured_content,
            schema_check,
            extensions,
        })
    }

    /// Consume into the complete parts the Agent must commit.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<PendingToolResultBlock>,
        heycode_core::ToolStructuredContent,
        heycode_core::ToolResultSchemaCheck,
        serde_json::Map<String, serde_json::Value>,
    ) {
        (
            self.blocks,
            self.structured_content,
            self.schema_check,
            self.extensions,
        )
    }
}

impl std::fmt::Debug for PendingRichToolResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingRichToolResult")
            .field("blocks", &self.blocks)
            .field(
                "has_structured_content",
                &matches!(
                    self.structured_content,
                    heycode_core::ToolStructuredContent::Present(_)
                ),
            )
            .field("schema_check", &self.schema_check.as_str())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// Stable pending-result validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PendingRichToolResultError {
    /// Invalid closed shape.
    #[error("pending rich tool result is invalid")]
    Invalid,
    /// Resource bound exceeded.
    #[error("pending rich tool result exceeds its limit")]
    TooLarge,
}

/// Tool return value plus optional typed pre-commit rich content.
pub struct ToolOutput {
    value: serde_json::Value,
    rich_result: Option<PendingRichToolResult>,
    is_error: bool,
}

impl ToolOutput {
    /// Plain JSON output.
    #[must_use]
    pub const fn plain(value: serde_json::Value) -> Self {
        Self {
            value,
            rich_result: None,
            is_error: false,
        }
    }

    /// JSON UI projection plus typed rich content.
    #[must_use]
    pub const fn rich(value: serde_json::Value, rich_result: PendingRichToolResult) -> Self {
        Self {
            value,
            rich_result: Some(rich_result),
            is_error: false,
        }
    }

    /// Typed rich content the provider reported as a tool execution error.
    #[must_use]
    pub const fn rich_error(value: serde_json::Value, rich_result: PendingRichToolResult) -> Self {
        Self {
            value,
            rich_result: Some(rich_result),
            is_error: true,
        }
    }

    /// Consume into JSON and rich planes.
    #[must_use]
    pub fn into_parts(self) -> (serde_json::Value, Option<PendingRichToolResult>, bool) {
        (self.value, self.rich_result, self.is_error)
    }
}

impl std::fmt::Debug for ToolOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolOutput")
            .field("value_kind", &json_kind(&self.value))
            .field("rich_result", &self.rich_result)
            .field("is_error", &self.is_error)
            .finish()
    }
}

fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn validate_uri(value: &str) -> Result<(), PendingRichToolResultError> {
    if value.is_empty()
        || value.len() > 2 * 1024
        || value.chars().any(char::is_whitespace)
        || value.chars().any(forbidden_control)
    {
        return Err(PendingRichToolResultError::Invalid);
    }
    let parsed = url::Url::parse(value).map_err(|_| PendingRichToolResultError::Invalid)?;
    if parsed.scheme().is_empty()
        || (matches!(parsed.scheme(), "http" | "https")
            && (!parsed.username().is_empty() || parsed.password().is_some()))
    {
        return Err(PendingRichToolResultError::Invalid);
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), PendingRichToolResultError> {
    if value.len() > 64 * 1024 || value.chars().any(forbidden_control) {
        return Err(PendingRichToolResultError::Invalid);
    }
    Ok(())
}

fn validate_extensions(
    extensions: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), PendingRichToolResultError> {
    if extensions.len() > 64
        || extensions
            .keys()
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(forbidden_control))
        || serde_json::to_vec(extensions).map_or(usize::MAX, |bytes| bytes.len()) > MAX_TEXT_BYTES
    {
        return Err(PendingRichToolResultError::TooLarge);
    }
    Ok(())
}

fn forbidden_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
}
