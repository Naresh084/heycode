//! MCP12 rich tool results and the output-schema bridge.
//!
//! A `tools/call` result is an **ordered** list of typed blocks plus optional
//! structured data. MCP06 kept only the blocks carrying a `text` field and
//! joined them, so an image between two paragraphs vanished and the paragraphs
//! closed over the gap as though it had never been there — the model saw a
//! result the server did not send. Resource links, embedded resources and
//! `structuredContent` were dropped entirely.
//!
//! Everything here preserves order and kind. A block this build cannot carry is
//! refused rather than dropped, because a silently shortened result is a
//! different result. Bytes are decoded and bounded rather than trusted as
//! strings; annotations and unknown extension members remain attached to their
//! exact block; and `Debug` reports lengths, never bodies: a server controls all
//! of this and none of it may reach a log through a derived format.
//!
//! # Output schema
//!
//! When a tool declares an `outputSchema`, the specification says servers
//! **MUST** return conforming `structuredContent` and clients **SHOULD**
//! validate it. heycode has no JSON Schema validator and may not add one, so
//! [`McpStructuredCheck`] reports exactly what was checked: a schema using a
//! construct this build does not evaluate yields `NotChecked`, which is a
//! different answer from `Conforms`. A failed check is **reported**, not
//! enforced — a subset validator that refused a call would turn its own
//! incompleteness into a broken server.
//!
//! # Protocol revisions
//!
//! `content`, `structuredContent` and `isError` are unchanged between the legacy
//! `2025-11-25` revision heycode negotiates and the current `2026-07-28` one; the
//! latter only adds `resultType`, which is read as an unknown key and ignored.
//! The one substantive difference is the type of `structuredContent`:
//! `2025-11-25` declares it `{ [key: string]: unknown }` (an object), while
//! `2026-07-28` widens it to "any JSON value (object, array, string, number,
//! boolean, or null)". Any JSON value is therefore accepted here and the
//! declared schema, not a hardcoded type rule, decides whether it is right —
//! rejecting an array outright would break against a dual-era server.
//! <https://modelcontextprotocol.io/specification/2025-11-25/server/tools#structured-content>
//! <https://modelcontextprotocol.io/specification/2026-07-28/server/tools#structured-content>

use base64::Engine as _;
use sha2::Digest as _;

use crate::channel::McpChannelError;

/// Whole-result bound, checked before any block is normalized.
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
/// Block-count bound for one result.
const MAX_BLOCKS: usize = 256;
/// Bound for one text block.
const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Bound for all text of one result together.
const MAX_TOTAL_TEXT_BYTES: usize = 256 * 1024;
/// Bound for one decoded image or audio body.
const MAX_MEDIA_BYTES: usize = 8 * 1024 * 1024;
/// Bound for all decoded media of one result together.
const MAX_TOTAL_MEDIA_BYTES: usize = 16 * 1024 * 1024;
/// Bound for the serialized `structuredContent` value.
const MAX_STRUCTURED_BYTES: usize = 1024 * 1024;
/// Bound for a resource URI, matching MCP08's `resources/read` walk.
const MAX_URI_BYTES: usize = 2 * 1024;
/// Bound for one-line display metadata.
const MAX_DISPLAY_BYTES: usize = 1_024;
/// Bound for a block description.
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
/// Bound for an annotation timestamp before parsing it.
const MAX_LAST_MODIFIED_BYTES: usize = 128;
/// Bound for extension fields retained on one protocol object.
const MAX_EXTENSION_FIELDS: usize = 64;
/// Schema-recursion bound; deeper nesting reports `NotChecked`.
const MAX_SCHEMA_DEPTH: u32 = 8;

/// Which kind of block this is, independent of its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpResultBlockKind {
    /// `{"type":"text"}`.
    Text,
    /// `{"type":"image"}`.
    Image,
    /// `{"type":"audio"}`.
    Audio,
    /// `{"type":"resource_link"}`.
    ResourceLink,
    /// `{"type":"resource"}` — an embedded resource.
    EmbeddedResource,
}

impl McpResultBlockKind {
    /// Every kind this build carries.
    ///
    /// Closed on purpose: a sixth content type must fail to compile here rather
    /// than be silently dropped from a result.
    pub const ALL: [Self; 5] = [
        Self::Text,
        Self::Image,
        Self::Audio,
        Self::ResourceLink,
        Self::EmbeddedResource,
    ];

    /// Exact wire spelling of the block's `type` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::ResourceLink => "resource_link",
            Self::EmbeddedResource => "resource",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// Intended audience from MCP content annotations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpResultAudience {
    /// Content intended for the human user.
    User,
    /// Content intended for the model.
    Assistant,
}

impl McpResultAudience {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// Validated annotations attached to one MCP result content block.
///
/// Unknown annotation members are retained as bounded JSON extension data,
/// never flattened into a known field or silently discarded. `Debug` exposes
/// only presence/count facts because every value is server-controlled.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct McpResultAnnotations {
    audience: Vec<McpResultAudience>,
    priority: Option<serde_json::Number>,
    last_modified: Option<String>,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl McpResultAnnotations {
    /// Intended audiences in exact wire order.
    #[must_use]
    pub fn audience(&self) -> &[McpResultAudience] {
        &self.audience
    }

    /// Priority in the inclusive MCP range 0.0..=1.0.
    #[must_use]
    pub fn priority(&self) -> Option<f64> {
        self.priority.as_ref().and_then(serde_json::Number::as_f64)
    }

    /// Exact validated JSON number retained for a durable bridge.
    #[must_use]
    pub const fn priority_number(&self) -> Option<&serde_json::Number> {
        self.priority.as_ref()
    }

    /// Validated RFC 3339 form of the MCP ISO-8601 modification timestamp.
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Bounded annotation members this build does not interpret.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }

    /// Whether no annotation member was present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.audience.is_empty()
            && self.priority.is_none()
            && self.last_modified.is_none()
            && self.extensions.is_empty()
    }
}

impl std::fmt::Debug for McpResultAnnotations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResultAnnotations")
            .field("audience", &self.audience)
            .field("has_priority", &self.priority.is_some())
            .field("has_last_modified", &self.last_modified.is_some())
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// Decoded media from an `image` or `audio` block.
///
/// The base64 is decoded at the boundary rather than carried as a string: a
/// string cannot be bounded by its real size, and a Consumer that admits the
/// bytes through ATT01 needs them decoded anyway. `Debug` reports the MIME type
/// and a byte count, never the body.
#[derive(Clone, PartialEq, Eq)]
pub struct McpResultMedia {
    media_type: heycode_core::AttachmentMediaType,
    content_id: heycode_core::AttachmentContentId,
    bytes: Vec<u8>,
}

impl McpResultMedia {
    /// Canonical validated MIME type required by both media block kinds.
    #[must_use]
    pub fn mime_type(&self) -> &str {
        self.media_type.as_str()
    }

    /// ATT01-compatible validated media type for a later admission Consumer.
    #[must_use]
    pub const fn attachment_media_type(&self) -> &heycode_core::AttachmentMediaType {
        &self.media_type
    }

    /// Content address ATT01 will derive if it admits these exact bytes.
    ///
    /// This is candidate identity, not a claim that durable admission happened.
    #[must_use]
    pub const fn content_id(&self) -> &heycode_core::AttachmentContentId {
        &self.content_id
    }

    /// Decoded body.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Decoded body length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the decoded body is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl std::fmt::Debug for McpResultMedia {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResultMedia")
            .field("media_type", &self.media_type)
            .field("content_id", &self.content_id)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// A `resource_link` block: a URI the client may fetch or subscribe to.
///
/// The specification notes such links are not guaranteed to appear in
/// `resources/list`, so this is a reference, not a listing row.
#[derive(Clone, PartialEq, Eq)]
pub struct McpResultLink {
    uri: String,
    name: String,
    title: Option<String>,
    description: Option<String>,
    mime_type: Option<String>,
    size: Option<u64>,
    public_source: Option<heycode_core::ServerToolSource>,
}

impl McpResultLink {
    /// Resource URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Programmatic name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Optional display title.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Optional description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Optional MIME type.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// Server-declared byte size, when present. This remains a hint until a
    /// later read or attachment admission verifies the actual body.
    #[must_use]
    pub const fn declared_size(&self) -> Option<u64> {
        self.size
    }

    /// Public HTTP(S) projection when the resource URI is one.
    ///
    /// File, git and custom resource URIs remain valid resources but are not
    /// promoted onto the public-source plane.
    #[must_use]
    pub const fn public_source(&self) -> Option<&heycode_core::ServerToolSource> {
        self.public_source.as_ref()
    }
}

impl std::fmt::Debug for McpResultLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResultLink")
            .field("uri", &"[REDACTED]")
            .field("has_name", &!self.name.is_empty())
            .field("has_title", &self.title.is_some())
            .field("has_description", &self.description.is_some())
            .field("mime_type", &self.mime_type)
            .field("size", &self.size)
            .field("is_public", &self.public_source.is_some())
            .finish()
    }
}

/// Body of an embedded resource: exactly one of `text` or `blob`.
///
/// `Debug` reports a kind and a length, never bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum McpEmbeddedBody {
    /// `text` contents.
    Text(String),
    /// `blob` contents, base64-decoded.
    Blob(Vec<u8>),
}

impl McpEmbeddedBody {
    /// Text body, when this resource is textual.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Blob(_) => None,
        }
    }

    /// Decoded binary body, when this resource is binary.
    #[must_use]
    pub fn blob(&self) -> Option<&[u8]> {
        match self {
            Self::Text(_) => None,
            Self::Blob(bytes) => Some(bytes),
        }
    }

    /// Decoded body length.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Blob(bytes) => bytes.len(),
        }
    }

    /// Whether the decoded body is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for McpEmbeddedBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Text(_) => "text",
            Self::Blob(_) => "blob",
        };
        write!(formatter, "McpEmbeddedBody::{kind}({} bytes)", self.len())
    }
}

/// A `resource` block: resource contents inlined into the result.
#[derive(Clone, PartialEq, Eq)]
pub struct McpEmbeddedResource {
    uri: String,
    mime_type: Option<String>,
    body: McpEmbeddedBody,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl McpEmbeddedResource {
    /// Resource URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Optional MIME type.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// Inlined body.
    #[must_use]
    pub const fn body(&self) -> &McpEmbeddedBody {
        &self.body
    }

    /// Bounded resource-content members this build does not interpret.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }
}

impl std::fmt::Debug for McpEmbeddedResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpEmbeddedResource")
            .field("uri", &"[REDACTED]")
            .field("mime_type", &self.mime_type)
            .field("body", &self.body)
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// Typed payload of one result block.
#[derive(Clone, PartialEq, Eq)]
pub enum McpResultContent {
    /// Plain text.
    Text(String),
    /// Image bytes with their MIME type.
    Image(McpResultMedia),
    /// Audio bytes with their MIME type.
    Audio(McpResultMedia),
    /// A link to a resource.
    ResourceLink(McpResultLink),
    /// Resource contents inlined into the result.
    EmbeddedResource(McpEmbeddedResource),
}

impl std::fmt::Debug for McpResultContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => formatter
                .debug_tuple("Text")
                .field(&format_args!("{} bytes", text.len()))
                .finish(),
            Self::Image(media) => formatter.debug_tuple("Image").field(media).finish(),
            Self::Audio(media) => formatter.debug_tuple("Audio").field(media).finish(),
            Self::ResourceLink(link) => formatter.debug_tuple("ResourceLink").field(link).finish(),
            Self::EmbeddedResource(resource) => formatter
                .debug_tuple("EmbeddedResource")
                .field(resource)
                .finish(),
        }
    }
}

/// One block of a tool result, in the order the server returned it.
///
/// Protocol annotations and unknown extension members remain attached to the
/// exact block they arrived on.
#[derive(Clone, PartialEq, Eq)]
pub struct McpResultBlock {
    content: McpResultContent,
    annotations: McpResultAnnotations,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl McpResultBlock {
    /// Which kind this block is.
    #[must_use]
    pub const fn kind(&self) -> McpResultBlockKind {
        match &self.content {
            McpResultContent::Text(_) => McpResultBlockKind::Text,
            McpResultContent::Image(_) => McpResultBlockKind::Image,
            McpResultContent::Audio(_) => McpResultBlockKind::Audio,
            McpResultContent::ResourceLink(_) => McpResultBlockKind::ResourceLink,
            McpResultContent::EmbeddedResource(_) => McpResultBlockKind::EmbeddedResource,
        }
    }

    /// Typed payload.
    #[must_use]
    pub const fn content(&self) -> &McpResultContent {
        &self.content
    }

    /// Metadata attached to this exact block.
    #[must_use]
    pub const fn annotations(&self) -> &McpResultAnnotations {
        &self.annotations
    }

    /// Bounded block members this build does not interpret.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }
}

impl std::fmt::Debug for McpResultBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResultBlock")
            .field("kind", &self.kind())
            .field("content", &self.content)
            .field("annotations", &self.annotations)
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

/// What was checked when comparing `structuredContent` to a declared
/// `outputSchema`.
///
/// `NotChecked` exists because heycode has no JSON Schema validator: a schema this
/// build cannot evaluate leaves the result **unvalidated**, which is a different
/// answer from valid. Only `Conforms` is conformance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStructuredCheck {
    /// The tool declared no `outputSchema`; there is nothing to check against.
    NoSchema,
    /// A schema was declared and the server returned no `structuredContent`.
    /// The specification makes that a server violation, not an empty result.
    Missing,
    /// The schema uses a construct this build does not evaluate.
    NotChecked {
        /// The construct that stopped the check.
        construct: &'static str,
    },
    /// Every check this build performs passed.
    Conforms,
    /// A check failed.
    Violates {
        /// Static requirement text; never a value from the result.
        requirement: &'static str,
    },
}

impl McpStructuredCheck {
    /// True only for `Conforms`. `NotChecked` never counts as conformance.
    #[must_use]
    pub const fn conforms(self) -> bool {
        matches!(self, Self::Conforms)
    }

    /// Whether the check found an actual problem, as opposed to declining to
    /// look. `NoSchema` and `NotChecked` are not problems.
    #[must_use]
    pub const fn is_violation(self) -> bool {
        matches!(self, Self::Missing | Self::Violates { .. })
    }

    /// Rank for folding sub-checks: a violation outranks a decline, which
    /// outranks conformance.
    const fn rank(self) -> u8 {
        match self {
            Self::Conforms | Self::NoSchema => 0,
            Self::NotChecked { .. } => 1,
            Self::Missing | Self::Violates { .. } => 2,
        }
    }

    fn stronger(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// One complete, ordered `tools/call` result.
///
/// `Debug` reports counts, never bodies.
#[derive(Clone, PartialEq, Eq)]
pub struct McpToolResult {
    blocks: Vec<McpResultBlock>,
    structured: Option<serde_json::Value>,
    check: McpStructuredCheck,
    is_error: bool,
    extensions: serde_json::Map<String, serde_json::Value>,
}

impl McpToolResult {
    /// Parse one `tools/call` result against the tool's declared output schema.
    ///
    /// # Errors
    /// A result over the whole-result bound (refused before any block is read),
    /// a missing or non-array `content`, an unmodelled block `type`, a block
    /// violating its own contract or bound, and invalid base64. An unmodelled
    /// block is refused rather than skipped: dropping it would hand the model a
    /// result the server did not send.
    ///
    /// <https://modelcontextprotocol.io/specification/2025-11-25/server/tools#tool-result>
    pub fn parse(
        result: &serde_json::Value,
        output_schema: Option<&serde_json::Value>,
    ) -> Result<Self, McpChannelError> {
        if serde_json::to_vec(result).map_or(usize::MAX, |bytes| bytes.len()) > MAX_RESULT_BYTES {
            return Err(McpChannelError::protocol(
                "tools/call result exceeds the 4194304-byte bound",
            ));
        }
        let object = result.as_object().ok_or(McpChannelError::protocol(
            "tools/call result must be an object",
        ))?;
        let rows = object
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or(McpChannelError::protocol(
                "tools/call result must contain a content array",
            ))?;
        if rows.len() > MAX_BLOCKS {
            return Err(McpChannelError::protocol(
                "tools/call result exceeds the 256-block bound",
            ));
        }
        let mut blocks = Vec::with_capacity(rows.len());
        let mut text_bytes: usize = 0;
        let mut media_bytes: usize = 0;
        for row in rows {
            let block = parse_block(row)?;
            match block.content() {
                McpResultContent::Text(text) => {
                    text_bytes = text_bytes.saturating_add(text.len());
                }
                McpResultContent::Image(media) | McpResultContent::Audio(media) => {
                    media_bytes = media_bytes.saturating_add(media.len());
                }
                McpResultContent::EmbeddedResource(resource) => match resource.body() {
                    McpEmbeddedBody::Text(text) => {
                        text_bytes = text_bytes.saturating_add(text.len());
                    }
                    McpEmbeddedBody::Blob(bytes) => {
                        media_bytes = media_bytes.saturating_add(bytes.len());
                    }
                },
                McpResultContent::ResourceLink(_) => {}
            }
            blocks.push(block);
        }
        if text_bytes > MAX_TOTAL_TEXT_BYTES {
            return Err(McpChannelError::protocol(
                "tools/call text exceeds the 262144-byte total bound",
            ));
        }
        if media_bytes > MAX_TOTAL_MEDIA_BYTES {
            return Err(McpChannelError::protocol(
                "tools/call media exceeds the 16777216-byte total bound",
            ));
        }
        let structured = match object.get("structuredContent") {
            None => None,
            Some(value) => {
                if serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
                    > MAX_STRUCTURED_BYTES
                {
                    return Err(McpChannelError::protocol(
                        "structuredContent exceeds the 1048576-byte bound",
                    ));
                }
                Some(value.clone())
            }
        };
        let is_error = match object.get("isError") {
            None => false,
            Some(serde_json::Value::Bool(flag)) => *flag,
            Some(_) => {
                return Err(McpChannelError::protocol(
                    "isError must be absent or a boolean",
                ));
            }
        };
        let check = check_structured(output_schema, structured.as_ref());
        let extensions = collect_extensions(object, &["content", "structuredContent", "isError"])?;
        Ok(Self {
            blocks,
            structured,
            check,
            is_error,
            extensions,
        })
    }

    /// Blocks in the order the server returned them.
    #[must_use]
    pub fn blocks(&self) -> &[McpResultBlock] {
        &self.blocks
    }

    /// Server-produced structured data, when the result carried any.
    #[must_use]
    pub const fn structured(&self) -> Option<&serde_json::Value> {
        self.structured.as_ref()
    }

    /// What was checked against the tool's declared output schema.
    #[must_use]
    pub const fn check(&self) -> McpStructuredCheck {
        self.check
    }

    /// Whether the server reported a tool execution error.
    ///
    /// A tool execution error is model-visible feedback the model can act on,
    /// distinct from a JSON-RPC protocol error, which is not.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.is_error
    }

    /// Bounded result members this legacy-era bridge does not interpret, such
    /// as a newer protocol revision's `resultType` or `_meta`.
    #[must_use]
    pub const fn extensions(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extensions
    }

    /// Every image block, for a Consumer that can admit media through ATT01.
    pub fn images(&self) -> impl Iterator<Item = &McpResultMedia> {
        self.blocks.iter().filter_map(|block| match block {
            McpResultBlock {
                content: McpResultContent::Image(media),
                ..
            } => Some(media),
            _ => None,
        })
    }

    /// Convert the complete parsed result into the shared pre-commit Tool
    /// plane. Media bytes remain private until the Agent admits them through
    /// the attachment service in model order.
    ///
    /// # Errors
    /// The shared boundary rejects any state that cannot be represented under
    /// its independent resource limits.
    pub fn to_tool_output(&self) -> Result<heycode_tools::ToolOutput, McpChannelError> {
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for block in &self.blocks {
            let metadata = shared_metadata(block)?;
            let shared = match block.content() {
                McpResultContent::Text(text) => heycode_tools::PendingToolResultBlock::Text {
                    text: text.clone(),
                    metadata,
                },
                McpResultContent::Image(media) => heycode_tools::PendingToolResultBlock::Image {
                    media: heycode_tools::PendingToolMedia::new(
                        Some(media.attachment_media_type().clone()),
                        media.bytes().to_vec(),
                    )
                    .map_err(|_| {
                        McpChannelError::protocol("rich result media exceeds its bound")
                    })?,
                    metadata,
                },
                McpResultContent::Audio(media) => heycode_tools::PendingToolResultBlock::Audio {
                    media: heycode_tools::PendingToolMedia::new(
                        Some(media.attachment_media_type().clone()),
                        media.bytes().to_vec(),
                    )
                    .map_err(|_| {
                        McpChannelError::protocol("rich result media exceeds its bound")
                    })?,
                    metadata,
                },
                McpResultContent::ResourceLink(link) => {
                    let shared = heycode_core::ToolResultResourceLink {
                        uri: link.uri().to_owned(),
                        name: link.name().to_owned(),
                        title: link.title().map(str::to_owned),
                        description: link.description().map(str::to_owned),
                        mime_type: link
                            .mime_type()
                            .map(heycode_core::AttachmentMediaType::new)
                            .transpose()
                            .map_err(|_| {
                                McpChannelError::protocol("resource MIME type is invalid")
                            })?,
                        declared_size: link.declared_size(),
                        metadata,
                    };
                    shared
                        .validate()
                        .map_err(|_| McpChannelError::protocol("rich resource link is invalid"))?;
                    heycode_tools::PendingToolResultBlock::ResourceLink { link: shared }
                }
                McpResultContent::EmbeddedResource(resource) => {
                    let mime_type = resource
                        .mime_type()
                        .map(heycode_core::AttachmentMediaType::new)
                        .transpose()
                        .map_err(|_| McpChannelError::protocol("resource MIME type is invalid"))?;
                    match resource.body() {
                        McpEmbeddedBody::Text(text) => {
                            heycode_tools::PendingToolResultBlock::EmbeddedText {
                                uri: resource.uri().to_owned(),
                                mime_type,
                                text: text.clone(),
                                resource_extensions: resource.extensions().clone(),
                                metadata,
                            }
                        }
                        McpEmbeddedBody::Blob(bytes) => {
                            heycode_tools::PendingToolResultBlock::EmbeddedBlob {
                                uri: resource.uri().to_owned(),
                                media: heycode_tools::PendingToolMedia::new(
                                    mime_type,
                                    bytes.clone(),
                                )
                                .map_err(|_| {
                                    McpChannelError::protocol("rich result media exceeds its bound")
                                })?,
                                resource_extensions: resource.extensions().clone(),
                                metadata,
                            }
                        }
                    }
                }
            };
            blocks.push(shared);
        }
        let structured_content = self
            .structured
            .as_ref()
            .map_or(heycode_core::ToolStructuredContent::Absent, |value| {
                heycode_core::ToolStructuredContent::Present(value.clone())
            });
        let rich = heycode_tools::PendingRichToolResult::new(
            blocks,
            structured_content,
            shared_schema_check(self.check),
            self.extensions.clone(),
        )
        .map_err(|_| McpChannelError::protocol("rich tool result exceeds its shared bound"))?;
        Ok(if self.is_error {
            heycode_tools::ToolOutput::rich_error(self.ui_value(), rich)
        } else {
            heycode_tools::ToolOutput::rich(self.ui_value(), rich)
        })
    }

    /// Typed JSON projection for UI/SDK Consumers. Media bodies never enter
    /// this value; they cross only the pending rich plane above.
    #[must_use]
    pub fn ui_value(&self) -> serde_json::Value {
        let blocks = self
            .blocks
            .iter()
            .map(|block| {
                let mut value = match block.content() {
                    McpResultContent::Text(text) => {
                        serde_json::json!({"type":"text", "text":text})
                    }
                    McpResultContent::Image(media) => media_ui("image", media),
                    McpResultContent::Audio(media) => media_ui("audio", media),
                    McpResultContent::ResourceLink(link) => serde_json::json!({
                        "type":"resource_link",
                        "uri":link.uri(),
                        "name":link.name(),
                        "title":link.title(),
                        "description":link.description(),
                        "mimeType":link.mime_type(),
                        "size":link.declared_size(),
                    }),
                    McpResultContent::EmbeddedResource(resource) => match resource.body() {
                        McpEmbeddedBody::Text(text) => serde_json::json!({
                            "type":"resource",
                            "uri":resource.uri(),
                            "mimeType":resource.mime_type(),
                            "body":{"kind":"text", "text":text},
                            "resourceExtensions":resource.extensions(),
                        }),
                        McpEmbeddedBody::Blob(bytes) => serde_json::json!({
                            "type":"resource",
                            "uri":resource.uri(),
                            "mimeType":resource.mime_type(),
                            "body":{"kind":"blob", "byteLength":bytes.len()},
                            "resourceExtensions":resource.extensions(),
                        }),
                    },
                };
                if let Some(object) = value.as_object_mut() {
                    object.insert("annotations".to_owned(), annotation_ui(block.annotations()));
                    object.insert(
                        "extensions".to_owned(),
                        serde_json::Value::Object(block.extensions().clone()),
                    );
                }
                value
            })
            .collect::<Vec<_>>();
        let mut value = serde_json::json!({
            "schemaVersion": 1,
            "blocks": blocks,
            "schemaCheck": schema_check_ui(self.check),
            "isError": self.is_error,
            "extensions": self.extensions,
        });
        if let Some(structured) = &self.structured
            && let Some(object) = value.as_object_mut()
        {
            object.insert("structuredContent".to_owned(), structured.clone());
        }
        value
    }

    /// Deterministic model-visible rendering that preserves order and kind.
    ///
    /// Text renders verbatim and blocks are joined with a single newline, so a
    /// result made only of text blocks renders exactly as MCP06 rendered it.
    /// Everything MCP06 dropped now appears as a one-line marker in its original
    /// position: a model that sees three paragraphs must not be shown two
    /// because an image sat between them.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        let mut lines: Vec<String> = self
            .blocks
            .iter()
            .map(|block| match block.content() {
                McpResultContent::Text(text) => text.clone(),
                McpResultContent::Image(media) => {
                    format!(
                        "[image {} · {} bytes]",
                        media.mime_type(),
                        media.bytes.len()
                    )
                }
                McpResultContent::Audio(media) => {
                    format!(
                        "[audio {} · {} bytes]",
                        media.mime_type(),
                        media.bytes.len()
                    )
                }
                McpResultContent::ResourceLink(link) => render_link(link),
                McpResultContent::EmbeddedResource(resource) => render_embedded(resource),
            })
            .collect();
        if let Some(structured) = &self.structured {
            lines.push("[structured content]".to_owned());
            lines.push(
                serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string()),
            );
        }
        // A declared schema the result violates is reported to the model rather
        // than enforced: this build validates a subset, and refusing on a subset
        // validator's opinion would turn its incompleteness into a broken server.
        match self.check {
            McpStructuredCheck::Missing => lines.push(
                "[structured content missing: the tool declares an output schema]".to_owned(),
            ),
            McpStructuredCheck::Violates { requirement } => lines.push(format!(
                "[structured content does not match the tool's declared output schema: {requirement}]"
            )),
            McpStructuredCheck::NoSchema
            | McpStructuredCheck::NotChecked { .. }
            | McpStructuredCheck::Conforms => {}
        }
        lines.join("\n")
    }
}

fn shared_metadata(
    block: &McpResultBlock,
) -> Result<heycode_core::ToolResultBlockMetadata, McpChannelError> {
    let audience = block
        .annotations()
        .audience()
        .iter()
        .map(|audience| match audience {
            McpResultAudience::User => heycode_core::ToolResultAudience::User,
            McpResultAudience::Assistant => heycode_core::ToolResultAudience::Assistant,
        })
        .collect();
    let annotations = heycode_core::ToolResultAnnotations::new(
        audience,
        block.annotations().priority_number().cloned(),
        block.annotations().last_modified().map(str::to_owned),
        block.annotations().extensions().clone(),
    )
    .map_err(|_| McpChannelError::protocol("rich result annotations are invalid"))?;
    heycode_core::ToolResultBlockMetadata::new(annotations, block.extensions().clone())
        .map_err(|_| McpChannelError::protocol("rich result metadata is invalid"))
}

fn shared_schema_check(check: McpStructuredCheck) -> heycode_core::ToolResultSchemaCheck {
    match check {
        McpStructuredCheck::NoSchema => heycode_core::ToolResultSchemaCheck::NoSchema,
        McpStructuredCheck::Missing => heycode_core::ToolResultSchemaCheck::Missing,
        McpStructuredCheck::NotChecked { construct } => {
            heycode_core::ToolResultSchemaCheck::NotChecked {
                construct: construct.to_owned(),
            }
        }
        McpStructuredCheck::Conforms => heycode_core::ToolResultSchemaCheck::Conforms,
        McpStructuredCheck::Violates { requirement } => {
            heycode_core::ToolResultSchemaCheck::Violates {
                requirement: requirement.to_owned(),
            }
        }
    }
}

fn media_ui(kind: &str, media: &McpResultMedia) -> serde_json::Value {
    serde_json::json!({
        "type":kind,
        "mimeType":media.mime_type(),
        "contentId":media.content_id().as_str(),
        "byteLength":media.len(),
    })
}

fn annotation_ui(annotations: &McpResultAnnotations) -> serde_json::Value {
    serde_json::json!({
        "audience": annotations.audience().iter().map(|value| value.as_str()).collect::<Vec<_>>(),
        "priority": annotations.priority_number(),
        "lastModified": annotations.last_modified(),
        "extensions": annotations.extensions(),
    })
}

fn schema_check_ui(check: McpStructuredCheck) -> serde_json::Value {
    match check {
        McpStructuredCheck::NoSchema => serde_json::json!({"status":"no_schema"}),
        McpStructuredCheck::Missing => serde_json::json!({"status":"missing"}),
        McpStructuredCheck::NotChecked { construct } => {
            serde_json::json!({"status":"not_checked", "construct":construct})
        }
        McpStructuredCheck::Conforms => serde_json::json!({"status":"conforms"}),
        McpStructuredCheck::Violates { requirement } => {
            serde_json::json!({"status":"violates", "requirement":requirement})
        }
    }
}

impl std::fmt::Debug for McpToolResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolResult")
            .field("blocks", &self.blocks.len())
            .field("structured", &self.structured.is_some())
            .field("check", &self.check)
            .field("is_error", &self.is_error)
            .field("extensions", &self.extensions.len())
            .finish()
    }
}

fn render_link(link: &McpResultLink) -> String {
    let mut rendered = format!("[resource_link {} · {}", link.uri, link.name);
    if let Some(mime) = &link.mime_type {
        rendered.push_str(" · ");
        rendered.push_str(mime);
    }
    rendered.push(']');
    if let Some(description) = &link.description {
        rendered.push(' ');
        rendered.push_str(description);
    }
    rendered
}

fn render_embedded(resource: &McpEmbeddedResource) -> String {
    let mime = resource.mime_type.as_deref().unwrap_or("unknown");
    match &resource.body {
        McpEmbeddedBody::Text(text) => {
            format!("[resource {} · {mime}]\n{text}", resource.uri)
        }
        McpEmbeddedBody::Blob(bytes) => format!(
            "[resource {} · {mime} · {} bytes]",
            resource.uri,
            bytes.len()
        ),
    }
}

fn parse_block(row: &serde_json::Value) -> Result<McpResultBlock, McpChannelError> {
    let row = row.as_object().ok_or(McpChannelError::protocol(
        "tool result content block must be an object",
    ))?;
    let kind = row
        .get("type")
        .and_then(serde_json::Value::as_str)
        .and_then(McpResultBlockKind::parse)
        .ok_or(McpChannelError::protocol(
            "tool result content block type must be text, image, audio, resource_link or resource",
        ))?;
    let (content, annotations, known): (McpResultContent, McpResultAnnotations, &[&str]) =
        match kind {
            McpResultBlockKind::Text => {
                let text =
                    required_str(row, "text", "text content must carry a string text field")?;
                check_bytes(
                    text.len(),
                    MAX_TEXT_BYTES,
                    "a text content block must be at most 65536 bytes",
                )?;
                (
                    McpResultContent::Text(text.to_owned()),
                    parse_annotations(row.get("annotations"))?,
                    &["type", "text", "annotations"],
                )
            }
            McpResultBlockKind::Image => (
                McpResultContent::Image(parse_media(row)?),
                parse_annotations(row.get("annotations"))?,
                &["type", "data", "mimeType", "annotations"],
            ),
            McpResultBlockKind::Audio => (
                McpResultContent::Audio(parse_media(row)?),
                parse_annotations(row.get("annotations"))?,
                &["type", "data", "mimeType", "annotations"],
            ),
            McpResultBlockKind::ResourceLink => (
                McpResultContent::ResourceLink(parse_link(row)?),
                parse_annotations(row.get("annotations"))?,
                &[
                    "type",
                    "uri",
                    "name",
                    "title",
                    "description",
                    "mimeType",
                    "size",
                    "annotations",
                ],
            ),
            McpResultBlockKind::EmbeddedResource => {
                let (resource, annotations) = parse_embedded(row)?;
                (
                    McpResultContent::EmbeddedResource(resource),
                    annotations,
                    &["type", "resource"],
                )
            }
        };
    Ok(McpResultBlock {
        content,
        annotations,
        extensions: collect_extensions(row, known)?,
    })
}

fn parse_media(
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<McpResultMedia, McpChannelError> {
    let data = required_str(row, "data", "media content must carry a string data field")?;
    // Bound the encoded form first: decoding an unbounded string to measure it
    // is exactly the allocation the bound exists to prevent. Base64 expands by
    // 4/3, so this ceiling admits every payload the decoded bound admits.
    check_bytes(
        data.len(),
        MAX_MEDIA_BYTES / 3 * 4 + 4,
        "a media content block must be at most 8388608 decoded bytes",
    )?;
    let mime_type = required_str(
        row,
        "mimeType",
        "media content must carry a string mimeType field",
    )?;
    let media_type = heycode_core::AttachmentMediaType::new(mime_type).map_err(|_| {
        McpChannelError::protocol("media content mimeType must be a valid MIME type")
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| McpChannelError::protocol("media content data must be valid base64"))?;
    check_bytes(
        bytes.len(),
        MAX_MEDIA_BYTES,
        "a media content block must be at most 8388608 decoded bytes",
    )?;
    let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
    Ok(McpResultMedia {
        media_type,
        content_id: heycode_core::AttachmentContentId::from_sha256(digest),
        bytes,
    })
}

fn parse_link(
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<McpResultLink, McpChannelError> {
    let uri = required_str(row, "uri", "a resource_link must carry a string uri")?;
    let parsed_uri = parse_resource_uri(uri)?;
    let name = required_str(row, "name", "a resource_link must carry a string name")?;
    check_display(name, "a resource_link name must be bounded one-line text")?;
    let title = optional_display(
        row,
        "title",
        "a resource_link title must be bounded one-line text",
    )?;
    let public_source = if matches!(parsed_uri.scheme(), "http" | "https") {
        Some(
            heycode_core::ServerToolSource::new(uri, title.as_deref()).map_err(|_| {
                McpChannelError::protocol(
                    "a public resource_link must be HTTP(S) without userinfo or unsafe text",
                )
            })?,
        )
    } else {
        None
    };
    Ok(McpResultLink {
        uri: uri.to_owned(),
        name: name.to_owned(),
        title,
        description: optional_prose(
            row,
            "description",
            "a resource_link description must be at most 4096 bytes of control-free text",
        )?,
        mime_type: optional_mime(row)?,
        size: optional_u64(
            row,
            "size",
            "a resource_link size must be a non-negative integer",
        )?,
        public_source,
    })
}

fn parse_embedded(
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<(McpEmbeddedResource, McpResultAnnotations), McpChannelError> {
    let resource = row
        .get("resource")
        .and_then(serde_json::Value::as_object)
        .ok_or(McpChannelError::protocol(
            "an embedded resource must carry a resource object",
        ))?;
    let uri = required_str(
        resource,
        "uri",
        "embedded resource contents must carry a string uri",
    )?;
    let _parsed_uri = parse_resource_uri(uri)?;
    let mime_type = optional_mime(resource)?;
    // Exactly one of text or blob: both is ambiguous and neither is empty.
    let body = match (resource.get("text"), resource.get("blob")) {
        (Some(serde_json::Value::String(text)), None) => {
            check_bytes(
                text.len(),
                MAX_TEXT_BYTES,
                "embedded resource text must be at most 65536 bytes",
            )?;
            McpEmbeddedBody::Text(text.clone())
        }
        (None, Some(serde_json::Value::String(blob))) => {
            check_bytes(
                blob.len(),
                MAX_MEDIA_BYTES / 3 * 4 + 4,
                "an embedded resource blob must be at most 8388608 decoded bytes",
            )?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(blob)
                .map_err(|_| {
                    McpChannelError::protocol("an embedded resource blob must be valid base64")
                })?;
            check_bytes(
                bytes.len(),
                MAX_MEDIA_BYTES,
                "an embedded resource blob must be at most 8388608 decoded bytes",
            )?;
            McpEmbeddedBody::Blob(bytes)
        }
        _ => {
            return Err(McpChannelError::protocol(
                "embedded resource contents must carry exactly one of text or blob",
            ));
        }
    };
    let annotations = parse_annotations(resource.get("annotations"))?;
    Ok((
        McpEmbeddedResource {
            uri: uri.to_owned(),
            mime_type,
            body,
            extensions: collect_extensions(
                resource,
                &["uri", "mimeType", "text", "blob", "annotations"],
            )?,
        },
        annotations,
    ))
}

fn parse_annotations(
    value: Option<&serde_json::Value>,
) -> Result<McpResultAnnotations, McpChannelError> {
    let Some(value) = value else {
        return Ok(McpResultAnnotations::default());
    };
    let object = value.as_object().ok_or(McpChannelError::protocol(
        "content annotations must be a JSON object",
    ))?;
    let audience = match object.get("audience") {
        None => Vec::new(),
        Some(serde_json::Value::Array(values)) if values.len() <= 2 => {
            let mut parsed = Vec::with_capacity(values.len());
            for value in values {
                let audience = value.as_str().and_then(McpResultAudience::parse).ok_or(
                    McpChannelError::protocol(
                        "annotation audience entries must be `user` or `assistant`",
                    ),
                )?;
                if parsed.contains(&audience) {
                    return Err(McpChannelError::protocol(
                        "annotation audience entries must be unique",
                    ));
                }
                parsed.push(audience);
            }
            parsed
        }
        Some(_) => {
            return Err(McpChannelError::protocol(
                "annotation audience must be an array with at most two entries",
            ));
        }
    };
    let priority = match object.get("priority") {
        None => None,
        Some(serde_json::Value::Number(number))
            if number
                .as_f64()
                .is_some_and(|priority| (0.0..=1.0).contains(&priority)) =>
        {
            Some(number.clone())
        }
        Some(_) => {
            return Err(McpChannelError::protocol(
                "annotation priority must be a number from 0.0 through 1.0",
            ));
        }
    };
    let last_modified = match object.get("lastModified") {
        None => None,
        Some(serde_json::Value::String(value))
            if value.len() <= MAX_LAST_MODIFIED_BYTES
                && chrono::DateTime::parse_from_rfc3339(value).is_ok() =>
        {
            Some(value.clone())
        }
        Some(_) => {
            return Err(McpChannelError::protocol(
                "annotation lastModified must be a bounded RFC 3339 timestamp",
            ));
        }
    };
    Ok(McpResultAnnotations {
        audience,
        priority,
        last_modified,
        extensions: collect_extensions(object, &["audience", "priority", "lastModified"])?,
    })
}

fn collect_extensions(
    object: &serde_json::Map<String, serde_json::Value>,
    known: &[&str],
) -> Result<serde_json::Map<String, serde_json::Value>, McpChannelError> {
    let mut extensions = serde_json::Map::new();
    for (key, value) in object {
        if !known.contains(&key.as_str()) {
            if extensions.len() >= MAX_EXTENSION_FIELDS {
                return Err(McpChannelError::protocol(
                    "an MCP result object carries too many extension fields",
                ));
            }
            extensions.insert(key.clone(), value.clone());
        }
    }
    Ok(extensions)
}

/// Compare `structuredContent` to a declared `outputSchema`.
///
/// A deliberately partial evaluator. It understands `type`, `required` and
/// `properties`, and reports [`McpStructuredCheck::NotChecked`] for every schema
/// construct it does not: `$ref`, the boolean combinators, conditionals and
/// pattern-based property matching all change what conformance means, and
/// ignoring them would let a schema look satisfied that is not.
fn check_structured(
    schema: Option<&serde_json::Value>,
    value: Option<&serde_json::Value>,
) -> McpStructuredCheck {
    let Some(schema) = schema else {
        return McpStructuredCheck::NoSchema;
    };
    let Some(value) = value else {
        return McpStructuredCheck::Missing;
    };
    check_value(schema, value, 0)
}

fn check_value(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    depth: u32,
) -> McpStructuredCheck {
    if depth > MAX_SCHEMA_DEPTH {
        return McpStructuredCheck::NotChecked {
            construct: "schema nesting deeper than this build evaluates",
        };
    }
    let Some(schema) = schema.as_object() else {
        return McpStructuredCheck::NotChecked {
            construct: "a non-object schema",
        };
    };
    let mut verdict = McpStructuredCheck::Conforms;
    for keyword in schema.keys() {
        if let Some(construct) = unsupported_schema_keyword(keyword) {
            verdict = verdict.stronger(McpStructuredCheck::NotChecked { construct });
        }
    }
    if schema
        .get("$schema")
        .is_some_and(|dialect| !supported_schema_dialect(dialect))
    {
        verdict = verdict.stronger(McpStructuredCheck::NotChecked {
            construct: "a JSON Schema dialect this build does not evaluate",
        });
    }
    if schema.get("required").is_some_and(|required| {
        required.as_array().is_none_or(|names| {
            names.iter().any(|name| name.as_str().is_none())
                || names
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != names.len()
        })
    }) {
        verdict = verdict.stronger(McpStructuredCheck::NotChecked {
            construct: "an invalid `required` declaration",
        });
    }
    if schema
        .get("properties")
        .is_some_and(|properties| !properties.is_object())
    {
        verdict = verdict.stronger(McpStructuredCheck::NotChecked {
            construct: "a non-object `properties`",
        });
    }
    if let Some(declared) = schema.get("type") {
        match type_matches(declared, value) {
            Some(true) => {}
            Some(false) => {
                return McpStructuredCheck::Violates {
                    requirement: "structuredContent does not have the declared type",
                };
            }
            None => {
                verdict = verdict.stronger(McpStructuredCheck::NotChecked {
                    construct: "a schema type this build does not evaluate",
                });
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required") {
            let Some(required) = required.as_array() else {
                return McpStructuredCheck::NotChecked {
                    construct: "a non-array `required`",
                };
            };
            for name in required {
                let Some(name) = name.as_str() else {
                    return McpStructuredCheck::NotChecked {
                        construct: "a non-string entry in `required`",
                    };
                };
                if !object.contains_key(name) {
                    return McpStructuredCheck::Violates {
                        requirement: "structuredContent is missing a required property",
                    };
                }
            }
        }
        if let Some(properties) = schema.get("properties") {
            let Some(properties) = properties.as_object() else {
                return McpStructuredCheck::NotChecked {
                    construct: "a non-object `properties`",
                };
            };
            for (name, sub_schema) in properties {
                if let Some(present) = object.get(name) {
                    let sub = check_value(sub_schema, present, depth.saturating_add(1));
                    if sub.is_violation() {
                        return sub;
                    }
                    verdict = verdict.stronger(sub);
                }
            }
        }
    }
    verdict
}

/// Return the static diagnostic for a JSON Schema keyword this subset does not
/// evaluate. Annotation-only keywords do not affect instance validity and are
/// safe to retain without downgrading the check.
fn unsupported_schema_keyword(keyword: &str) -> Option<&'static str> {
    match keyword {
        "type" | "required" | "properties" | "$schema" | "$id" | "$anchor" | "$comment"
        | "title" | "description" | "default" | "examples" | "deprecated" | "readOnly"
        | "writeOnly" => None,
        "$ref" => Some("$ref"),
        "$dynamicRef" => Some("$dynamicRef"),
        "allOf" => Some("allOf"),
        "anyOf" => Some("anyOf"),
        "oneOf" => Some("oneOf"),
        "not" => Some("not"),
        "if" => Some("if"),
        "patternProperties" => Some("patternProperties"),
        "dependentSchemas" => Some("dependentSchemas"),
        "additionalProperties" => Some("additionalProperties"),
        _ => Some("a JSON Schema keyword this build does not evaluate"),
    }
}

fn supported_schema_dialect(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|dialect| {
        matches!(
            dialect,
            "https://json-schema.org/draft/2020-12/schema"
                | "https://json-schema.org/draft/2020-12/schema#"
                | "http://json-schema.org/draft-07/schema#"
                | "https://json-schema.org/draft-07/schema#"
        )
    })
}

/// Whether `value` has the JSON type `declared` names.
///
/// `None` means the declaration itself is not a form this build reads, which
/// must become `NotChecked` rather than a pass.
fn type_matches(declared: &serde_json::Value, value: &serde_json::Value) -> Option<bool> {
    match declared {
        serde_json::Value::String(name) => json_type_matches(name, value),
        serde_json::Value::Array(names) => {
            if names.is_empty() {
                return None;
            }
            let mut any = false;
            for name in names {
                let name = name.as_str()?;
                any |= json_type_matches(name, value)?;
            }
            Some(any)
        }
        _ => None,
    }
}

fn json_type_matches(name: &str, value: &serde_json::Value) -> Option<bool> {
    Some(match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value
                    .as_f64()
                    .is_some_and(|number| number.is_finite() && number.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => return None,
    })
}

fn required_str<'a>(
    row: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    requirement: &'static str,
) -> Result<&'a str, McpChannelError> {
    row.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(McpChannelError::Protocol { requirement })
}

fn optional_display(
    row: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    requirement: &'static str,
) -> Result<Option<String>, McpChannelError> {
    match row.get(field) {
        None => Ok(None),
        Some(serde_json::Value::String(text)) => {
            check_display(text, requirement)?;
            Ok(Some(text.clone()))
        }
        Some(_) => Err(McpChannelError::Protocol { requirement }),
    }
}

fn optional_prose(
    row: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    requirement: &'static str,
) -> Result<Option<String>, McpChannelError> {
    match row.get(field) {
        None => Ok(None),
        Some(serde_json::Value::String(text)) => {
            if text.len() > MAX_DESCRIPTION_BYTES || has_forbidden_control(text) {
                return Err(McpChannelError::Protocol { requirement });
            }
            Ok(Some(text.clone()))
        }
        Some(_) => Err(McpChannelError::Protocol { requirement }),
    }
}

fn optional_u64(
    row: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    requirement: &'static str,
) -> Result<Option<u64>, McpChannelError> {
    match row.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(McpChannelError::Protocol { requirement }),
    }
}

fn optional_mime(
    row: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>, McpChannelError> {
    match row.get("mimeType") {
        None => Ok(None),
        Some(serde_json::Value::String(value)) => heycode_core::AttachmentMediaType::new(value)
            .map(|media_type| Some(media_type.as_str().to_owned()))
            .map_err(|_| McpChannelError::protocol("mimeType must be a valid MIME type")),
        Some(_) => Err(McpChannelError::protocol(
            "mimeType must be a valid MIME type",
        )),
    }
}

fn check_display(value: &str, requirement: &'static str) -> Result<(), McpChannelError> {
    if value.is_empty()
        || value.len() > MAX_DISPLAY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(McpChannelError::Protocol { requirement });
    }
    Ok(())
}

fn parse_resource_uri(value: &str) -> Result<url::Url, McpChannelError> {
    if value.is_empty()
        || value.len() > MAX_URI_BYTES
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
    {
        return Err(McpChannelError::protocol(
            "a resource uri must be a bounded absolute RFC 3986 URI",
        ));
    }
    url::Url::parse(value).map_err(|_| {
        McpChannelError::protocol("a resource uri must be a bounded absolute RFC 3986 URI")
    })
}

const fn check_bytes(
    len: usize,
    max: usize,
    requirement: &'static str,
) -> Result<(), McpChannelError> {
    if len > max {
        return Err(McpChannelError::Protocol { requirement });
    }
    Ok(())
}

fn has_forbidden_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}
