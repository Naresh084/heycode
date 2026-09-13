//! Durable, provider-neutral rich tool-result vocabulary.

use serde::{Deserialize, Serialize};

const MAX_BLOCKS: usize = 256;
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_STRUCTURED_BYTES: usize = 1024 * 1024;
const MAX_EXTENSION_BYTES: usize = 256 * 1024;
const MAX_EXTENSION_FIELDS: usize = 64;
const MAX_URI_BYTES: usize = 2 * 1024;
const MAX_DISPLAY_BYTES: usize = 4 * 1024;

/// Stable failure class for malformed rich-result state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RichToolResultError {
    /// A typed field violates its closed shape.
    #[error("rich tool result is invalid")]
    Invalid,
    /// A bounded collection or value exceeds its limit.
    #[error("rich tool result exceeds its limit")]
    TooLarge,
}

/// Intended audience of one external result block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultAudience {
    /// Human user.
    User,
    /// Model/assistant.
    Assistant,
}

/// Validated protocol annotations retained on their exact result block.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultAnnotations {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    audience: Vec<ToolResultAudience>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl ToolResultAnnotations {
    /// Construct one bounded annotation object.
    ///
    /// # Errors
    /// Duplicate audiences, invalid priority/timestamp or unbounded extension
    /// data fail without retaining server-controlled text in the error.
    pub fn new(
        audience: Vec<ToolResultAudience>,
        priority: Option<serde_json::Number>,
        last_modified: Option<String>,
        extensions: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, RichToolResultError> {
        let value = Self {
            audience,
            priority,
            last_modified,
            extensions,
        };
        value.validate()?;
        Ok(value)
    }

    /// Intended audiences in protocol order.
    #[must_use]
    pub fn audience(&self) -> &[ToolResultAudience] {
        &self.audience
    }

    /// Optional priority in the inclusive 0..=1 range.
    #[must_use]
    pub const fn priority(&self) -> Option<&serde_json::Number> {
        self.priority.as_ref()
    }

    /// Optional validated modification timestamp text.
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Bounded protocol extension members.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }

    /// Validate deserialized state.
    ///
    /// # Errors
    /// Invalid or unbounded fields.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        if self.audience.len() > 2
            || self
                .audience
                .iter()
                .enumerate()
                .any(|(index, item)| self.audience[..index].contains(item))
            || self.priority.as_ref().is_some_and(|priority| {
                priority
                    .as_f64()
                    .is_none_or(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            })
            || self.last_modified.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 128 || value.chars().any(forbidden_control)
            })
        {
            return Err(RichToolResultError::Invalid);
        }
        validate_extensions(&self.extensions)
    }
}

impl Default for ToolResultAnnotations {
    fn default() -> Self {
        Self {
            audience: Vec::new(),
            priority: None,
            last_modified: None,
            extensions: serde_json::Map::new(),
        }
    }
}

impl std::fmt::Debug for ToolResultAnnotations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResultAnnotations")
            .field("audience", &self.audience)
            .field("has_priority", &self.priority.is_some())
            .field("has_last_modified", &self.last_modified.is_some())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// Metadata attached to one exact result block.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultBlockMetadata {
    /// Protocol annotations.
    pub annotations: ToolResultAnnotations,
    /// Bounded block extension members.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

impl ToolResultBlockMetadata {
    /// Construct bounded block metadata.
    ///
    /// # Errors
    /// Invalid annotations or extension data.
    pub fn new(
        annotations: ToolResultAnnotations,
        extensions: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, RichToolResultError> {
        let value = Self {
            annotations,
            extensions,
        };
        value.validate()?;
        Ok(value)
    }

    /// Validate deserialized metadata.
    ///
    /// # Errors
    /// Invalid annotations or extension data.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        self.annotations.validate()?;
        validate_extensions(&self.extensions)
    }
}

impl Default for ToolResultBlockMetadata {
    fn default() -> Self {
        Self {
            annotations: ToolResultAnnotations::default(),
            extensions: serde_json::Map::new(),
        }
    }
}

impl std::fmt::Debug for ToolResultBlockMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResultBlockMetadata")
            .field("annotations", &self.annotations)
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// Presence-preserving structured JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum ToolStructuredContent {
    /// The field was absent.
    Absent,
    /// The field was present; its value may itself be JSON `null`.
    Present(serde_json::Value),
}

impl ToolStructuredContent {
    /// Present value, preserving explicit JSON `null`.
    #[must_use]
    pub const fn value(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Absent => None,
            Self::Present(value) => Some(value),
        }
    }

    /// Validate deserialized presence/value bounds.
    ///
    /// # Errors
    /// Structured JSON exceeds the durable bound.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        if self.value().is_some_and(|value| {
            serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len()) > MAX_STRUCTURED_BYTES
        }) {
            return Err(RichToolResultError::TooLarge);
        }
        Ok(())
    }
}

/// Honest outcome of comparing structured content with a declared schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolResultSchemaCheck {
    /// No schema was declared.
    NoSchema,
    /// A schema was declared but structured content was absent.
    Missing,
    /// The evaluator declined an unsupported assertion.
    NotChecked {
        /// Bounded assertion/construct name.
        construct: String,
    },
    /// Every implemented assertion passed.
    Conforms,
    /// One implemented assertion failed.
    Violates {
        /// Bounded static requirement identifier/text.
        requirement: String,
    },
}

impl ToolResultSchemaCheck {
    /// Stable status without construct/requirement text.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NoSchema => "no_schema",
            Self::Missing => "missing",
            Self::NotChecked { .. } => "not_checked",
            Self::Conforms => "conforms",
            Self::Violates { .. } => "violates",
        }
    }

    /// Validate deserialized schema-check evidence.
    ///
    /// # Errors
    /// An unbounded or control-bearing identifier/requirement.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        let text = match self {
            Self::NotChecked { construct } => Some(construct),
            Self::Violates { requirement } => Some(requirement),
            Self::NoSchema | Self::Missing | Self::Conforms => None,
        };
        if text.is_some_and(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(forbidden_control)
        }) {
            return Err(RichToolResultError::Invalid);
        }
        Ok(())
    }
}

/// One resource-link block with all protocol metadata retained.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultResourceLink {
    /// Absolute resource URI.
    pub uri: String,
    /// Programmatic name.
    pub name: String,
    /// Optional display title.
    pub title: Option<String>,
    /// Optional prose description.
    pub description: Option<String>,
    /// Optional declared MIME type.
    pub mime_type: Option<crate::AttachmentMediaType>,
    /// Optional server-declared byte size.
    pub declared_size: Option<u64>,
    /// Metadata attached to this exact block.
    pub metadata: ToolResultBlockMetadata,
}

impl ToolResultResourceLink {
    /// Validate deserialized link state.
    ///
    /// # Errors
    /// Malformed URI/text or metadata.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        validate_uri(&self.uri)?;
        validate_display(&self.name, true)?;
        validate_optional_display(self.title.as_deref())?;
        validate_optional_display(self.description.as_deref())?;
        self.metadata.validate()
    }
}

impl std::fmt::Debug for ToolResultResourceLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResultResourceLink")
            .field("uri", &"[REDACTED]")
            .field("has_name", &!self.name.is_empty())
            .field("has_title", &self.title.is_some())
            .field("has_description", &self.description.is_some())
            .field("mime_type", &self.mime_type)
            .field("declared_size", &self.declared_size)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// Durable reference for media bytes admitted through the attachment service.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMediaReference {
    /// Verified immutable attachment metadata.
    pub attachment: crate::AttachmentMetadata,
    /// Protocol-declared media type, retained independently of content sniffing.
    pub declared_media_type: Option<crate::AttachmentMediaType>,
}

impl ToolResultMediaReference {
    /// Validate the durable reference.
    ///
    /// # Errors
    /// Invalid attachment metadata.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        self.attachment
            .validate()
            .map_err(|_| RichToolResultError::Invalid)
    }
}

impl std::fmt::Debug for ToolResultMediaReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolResultMediaReference")
            .field("attachment", &self.attachment)
            .field("declared_media_type", &self.declared_media_type)
            .finish()
    }
}

/// One ordered durable rich-result block.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DurableToolResultBlock {
    /// Verbatim text block.
    Text {
        /// Text bytes.
        text: String,
        /// Exact block metadata.
        metadata: ToolResultBlockMetadata,
    },
    /// Image admitted through ATT01.
    Image {
        /// Durable media reference.
        media: ToolResultMediaReference,
        /// Exact block metadata.
        metadata: ToolResultBlockMetadata,
    },
    /// Audio/opaque media admitted through ATT01 storage.
    Audio {
        /// Durable media reference.
        media: ToolResultMediaReference,
        /// Exact block metadata.
        metadata: ToolResultBlockMetadata,
    },
    /// External resource link.
    ResourceLink {
        /// Complete link metadata.
        link: ToolResultResourceLink,
    },
    /// Embedded textual resource.
    EmbeddedText {
        /// Absolute resource URI.
        uri: String,
        /// Optional declared MIME type.
        mime_type: Option<crate::AttachmentMediaType>,
        /// Verbatim text body.
        text: String,
        /// Bounded members of the embedded resource object.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        resource_extensions: serde_json::Map<String, serde_json::Value>,
        /// Exact block metadata.
        metadata: ToolResultBlockMetadata,
    },
    /// Embedded binary resource admitted through ATT01 storage.
    EmbeddedBlob {
        /// Absolute resource URI.
        uri: String,
        /// Durable media reference.
        media: ToolResultMediaReference,
        /// Bounded members of the embedded resource object.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        resource_extensions: serde_json::Map<String, serde_json::Value>,
        /// Exact block metadata.
        metadata: ToolResultBlockMetadata,
    },
}

impl DurableToolResultBlock {
    fn validate(&self) -> Result<usize, RichToolResultError> {
        match self {
            Self::Text { text, metadata } => {
                validate_text(text)?;
                metadata.validate()?;
                Ok(text.len())
            }
            Self::Image { media, metadata } => {
                media.validate()?;
                if !media.attachment.media_type().is_image()
                    || media
                        .declared_media_type
                        .as_ref()
                        .is_none_or(|value| !value.is_image())
                {
                    return Err(RichToolResultError::Invalid);
                }
                metadata.validate()?;
                Ok(0)
            }
            Self::Audio { media, metadata } => {
                media.validate()?;
                if media
                    .declared_media_type
                    .as_ref()
                    .is_none_or(|value| !value.as_str().starts_with("audio/"))
                {
                    return Err(RichToolResultError::Invalid);
                }
                metadata.validate()?;
                Ok(0)
            }
            Self::ResourceLink { link } => {
                link.validate()?;
                Ok(0)
            }
            Self::EmbeddedText {
                uri,
                text,
                resource_extensions,
                metadata,
                ..
            } => {
                validate_uri(uri)?;
                validate_text(text)?;
                validate_extensions(resource_extensions)?;
                metadata.validate()?;
                Ok(text.len())
            }
            Self::EmbeddedBlob {
                uri,
                media,
                resource_extensions,
                metadata,
            } => {
                validate_uri(uri)?;
                media.validate()?;
                validate_extensions(resource_extensions)?;
                metadata.validate()?;
                Ok(0)
            }
        }
    }
}

impl std::fmt::Debug for DurableToolResultBlock {
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

/// Complete durable ordered rich tool result.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DurableToolResult {
    schema_version: u8,
    blocks: Vec<DurableToolResultBlock>,
    structured_content: ToolStructuredContent,
    schema_check: ToolResultSchemaCheck,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl DurableToolResult {
    /// Current durable rich-result schema.
    pub const SCHEMA_VERSION: u8 = 1;

    /// Construct one complete durable result.
    ///
    /// # Errors
    /// Oversized blocks, malformed references or unbounded structured and
    /// extension values.
    pub fn new(
        blocks: Vec<DurableToolResultBlock>,
        structured_content: ToolStructuredContent,
        schema_check: ToolResultSchemaCheck,
        extensions: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, RichToolResultError> {
        let value = Self {
            schema_version: Self::SCHEMA_VERSION,
            blocks,
            structured_content,
            schema_check,
            extensions,
        };
        value.validate()?;
        Ok(value)
    }

    /// Ordered typed blocks.
    #[must_use]
    pub fn blocks(&self) -> &[DurableToolResultBlock] {
        &self.blocks
    }

    /// Presence-preserving structured value.
    #[must_use]
    pub const fn structured_content(&self) -> &ToolStructuredContent {
        &self.structured_content
    }

    /// Output-schema check evidence.
    #[must_use]
    pub const fn schema_check(&self) -> &ToolResultSchemaCheck {
        &self.schema_check
    }

    /// Bounded result-level extension members.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }

    /// Validate deserialized state.
    ///
    /// # Errors
    /// Unsupported schema, malformed blocks or unbounded values.
    pub fn validate(&self) -> Result<(), RichToolResultError> {
        if self.schema_version != Self::SCHEMA_VERSION || self.blocks.len() > MAX_BLOCKS {
            return Err(RichToolResultError::Invalid);
        }
        let mut text_bytes = 0_usize;
        for block in &self.blocks {
            text_bytes = text_bytes
                .checked_add(block.validate()?)
                .ok_or(RichToolResultError::TooLarge)?;
        }
        if text_bytes > MAX_TEXT_BYTES {
            return Err(RichToolResultError::TooLarge);
        }
        self.structured_content.validate()?;
        self.schema_check.validate()?;
        validate_extensions(&self.extensions)
    }

    /// Deterministic text projection used by providers without typed tool
    /// result content blocks.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        let mut lines = Vec::new();
        for block in &self.blocks {
            match block {
                DurableToolResultBlock::Text { text, .. } => lines.push(text.clone()),
                DurableToolResultBlock::Image { media, .. } => lines.push(format!(
                    "[image {} · {} bytes]",
                    media
                        .declared_media_type
                        .as_ref()
                        .map_or(media.attachment.media_type().as_str(), |value| value
                            .as_str()),
                    media.attachment.byte_len()
                )),
                DurableToolResultBlock::Audio { media, .. } => lines.push(format!(
                    "[audio {} · {} bytes]",
                    media
                        .declared_media_type
                        .as_ref()
                        .map_or(media.attachment.media_type().as_str(), |value| value
                            .as_str()),
                    media.attachment.byte_len()
                )),
                DurableToolResultBlock::ResourceLink { link } => {
                    let mut rendered = format!("[resource_link {} · {}", link.uri, link.name);
                    if let Some(mime) = &link.mime_type {
                        rendered.push_str(" · ");
                        rendered.push_str(mime.as_str());
                    }
                    rendered.push(']');
                    if let Some(description) = &link.description {
                        rendered.push(' ');
                        rendered.push_str(description);
                    }
                    lines.push(rendered);
                }
                DurableToolResultBlock::EmbeddedText {
                    uri,
                    mime_type,
                    text,
                    ..
                } => lines.push(format!(
                    "[resource {} · {}]\n{}",
                    uri,
                    mime_type.as_ref().map_or("unknown", |value| value.as_str()),
                    text
                )),
                DurableToolResultBlock::EmbeddedBlob { uri, media, .. } => lines.push(format!(
                    "[resource {} · {} · {} bytes]",
                    uri,
                    media
                        .declared_media_type
                        .as_ref()
                        .map_or(media.attachment.media_type().as_str(), |value| value
                            .as_str()),
                    media.attachment.byte_len()
                )),
            }
        }
        if let ToolStructuredContent::Present(value) = &self.structured_content {
            lines.push("[structured content]".to_owned());
            lines.push(serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()));
        }
        match &self.schema_check {
            ToolResultSchemaCheck::Missing => lines.push(
                "[structured content missing: the tool declares an output schema]".to_owned(),
            ),
            ToolResultSchemaCheck::Violates { requirement } => lines.push(format!(
                "[structured content does not match the tool's declared output schema: {requirement}]"
            )),
            ToolResultSchemaCheck::NoSchema
            | ToolResultSchemaCheck::NotChecked { .. }
            | ToolResultSchemaCheck::Conforms => {}
        }
        lines.join("\n")
    }
}

impl std::fmt::Debug for DurableToolResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableToolResult")
            .field("schema_version", &self.schema_version)
            .field("blocks", &self.blocks)
            .field(
                "has_structured_content",
                &matches!(self.structured_content, ToolStructuredContent::Present(_)),
            )
            .field("schema_check", &self.schema_check.as_str())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

fn validate_extensions(
    extensions: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), RichToolResultError> {
    if extensions.len() > MAX_EXTENSION_FIELDS
        || extensions
            .keys()
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(forbidden_control))
        || serde_json::to_vec(extensions).map_or(usize::MAX, |bytes| bytes.len())
            > MAX_EXTENSION_BYTES
    {
        return Err(RichToolResultError::TooLarge);
    }
    Ok(())
}

fn validate_uri(value: &str) -> Result<(), RichToolResultError> {
    if value.is_empty()
        || value.len() > MAX_URI_BYTES
        || value.chars().any(char::is_whitespace)
        || value.chars().any(forbidden_control)
    {
        return Err(RichToolResultError::Invalid);
    }
    let url = url::Url::parse(value).map_err(|_| RichToolResultError::Invalid)?;
    if url.scheme().is_empty()
        || (matches!(url.scheme(), "http" | "https")
            && (!url.username().is_empty() || url.password().is_some()))
    {
        return Err(RichToolResultError::Invalid);
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), RichToolResultError> {
    if value.len() > 64 * 1024 || value.chars().any(forbidden_control) {
        return Err(RichToolResultError::TooLarge);
    }
    Ok(())
}

fn validate_display(value: &str, required: bool) -> Result<(), RichToolResultError> {
    if (required && value.is_empty())
        || value.len() > MAX_DISPLAY_BYTES
        || value.chars().any(forbidden_control)
    {
        return Err(RichToolResultError::Invalid);
    }
    Ok(())
}

fn validate_optional_display(value: Option<&str>) -> Result<(), RichToolResultError> {
    match value {
        Some(value) => validate_display(value, false),
        None => Ok(()),
    }
}

fn forbidden_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn explicit_null_and_block_order_survive_the_durable_wire() {
        let result = DurableToolResult::new(
            vec![
                DurableToolResultBlock::Text {
                    text: "before".to_owned(),
                    metadata: ToolResultBlockMetadata::default(),
                },
                DurableToolResultBlock::ResourceLink {
                    link: ToolResultResourceLink {
                        uri: "https://example.test/report".to_owned(),
                        name: "report".to_owned(),
                        title: None,
                        description: None,
                        mime_type: None,
                        declared_size: None,
                        metadata: ToolResultBlockMetadata::default(),
                    },
                },
                DurableToolResultBlock::Text {
                    text: "after".to_owned(),
                    metadata: ToolResultBlockMetadata::default(),
                },
            ],
            ToolStructuredContent::Present(serde_json::Value::Null),
            ToolResultSchemaCheck::Conforms,
            serde_json::Map::new(),
        )
        .unwrap();
        let wire = serde_json::to_value(&result).unwrap();
        let decoded: DurableToolResult = serde_json::from_value(wire).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, result);
        assert_eq!(
            decoded.structured_content().value(),
            Some(&serde_json::Value::Null)
        );
        assert!(matches!(
            decoded.blocks(),
            [
                DurableToolResultBlock::Text { .. },
                DurableToolResultBlock::ResourceLink { .. },
                DurableToolResultBlock::Text { .. }
            ]
        ));
    }

    #[test]
    fn debug_reports_shapes_without_server_text_or_uris() {
        let result = DurableToolResult::new(
            vec![
                DurableToolResultBlock::Text {
                    text: "SERVER-TEXT-CANARY".to_owned(),
                    metadata: ToolResultBlockMetadata::default(),
                },
                DurableToolResultBlock::ResourceLink {
                    link: ToolResultResourceLink {
                        uri: "https://example.test/SECRET-URI-CANARY".to_owned(),
                        name: "SECRET-NAME-CANARY".to_owned(),
                        title: None,
                        description: None,
                        mime_type: None,
                        declared_size: None,
                        metadata: ToolResultBlockMetadata::default(),
                    },
                },
            ],
            ToolStructuredContent::Absent,
            ToolResultSchemaCheck::NoSchema,
            serde_json::Map::new(),
        )
        .unwrap();
        let rendered = format!("{result:?}");
        for canary in [
            "SERVER-TEXT-CANARY",
            "SECRET-URI-CANARY",
            "SECRET-NAME-CANARY",
        ] {
            assert!(!rendered.contains(canary), "{rendered}");
        }
    }
}
