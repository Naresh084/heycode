//! Provider-neutral chat vocabulary shared by every [`crate::Provider`].
//!
//! These types are wire-agnostic: adapters translate to and from provider
//! JSON at the edge. Providers must never fork the vocabulary (AGENTS §5).

pub use heycode_core::{TokenUsage, ToolSpec};

/// Author of a [`ChatMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Standing instructions rendered ahead of the conversation.
    System,
    /// End-user input.
    User,
    /// Model output; may carry [`ChatMessage::tool_calls`].
    Assistant,
    /// Result of one tool call, bound to it by [`ChatMessage::tool_call_id`].
    Tool,
}

/// One tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatToolCall {
    /// Provider-assigned id, echoed back in the matching tool-result message.
    pub id: String,
    /// Tool name as advertised in [`ChatRequest::tools`].
    pub name: String,
    /// Raw JSON text of the arguments object, concatenated from stream deltas.
    pub arguments: String,
}

/// One verified inline image input.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatImage {
    media_type: heycode_core::AttachmentMediaType,
    bytes: Vec<u8>,
}

impl ChatImage {
    /// Construct one supported bounded image.
    ///
    /// # Errors
    /// MIME must be PNG/JPEG/GIF/WebP and bytes must be 1..=32 MiB.
    pub fn new(
        media_type: heycode_core::AttachmentMediaType,
        bytes: Vec<u8>,
    ) -> Result<Self, crate::LlmError> {
        if !matches!(
            media_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) || bytes.is_empty()
            || bytes.len() > 32 * 1024 * 1024
        {
            return Err(crate::LlmError::InvalidResponse(
                "image input is invalid".to_owned(),
            ));
        }
        Ok(Self { media_type, bytes })
    }

    /// Canonical image MIME.
    #[must_use]
    pub fn media_type(&self) -> &heycode_core::AttachmentMediaType {
        &self.media_type
    }

    /// Exact verified bytes for protocol serialization.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ChatImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatImage")
            .field("media_type", &self.media_type)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

/// One verified inline document input.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatDocument {
    media_type: heycode_core::AttachmentMediaType,
    filename: String,
    bytes: Vec<u8>,
}

impl ChatDocument {
    /// Construct one bounded native PDF input.
    ///
    /// # Errors
    /// MIME must be PDF, filename must be a safe portable basename and bytes
    /// must be 1..=32 MiB.
    pub fn new(
        media_type: heycode_core::AttachmentMediaType,
        filename: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Self, crate::LlmError> {
        let filename = filename.into();
        if media_type.as_str() != "application/pdf"
            || filename.is_empty()
            || filename.len() > 255
            || filename.trim() != filename
            || matches!(filename.as_str(), "." | "..")
            || filename.contains(['/', '\\'])
            || filename.chars().any(char::is_control)
            || bytes.is_empty()
            || bytes.len() > 32 * 1024 * 1024
            || !bytes.starts_with(b"%PDF-")
        {
            return Err(crate::LlmError::InvalidResponse(
                "document input is invalid".to_owned(),
            ));
        }
        Ok(Self {
            media_type,
            filename,
            bytes,
        })
    }

    /// Canonical document MIME.
    #[must_use]
    pub fn media_type(&self) -> &heycode_core::AttachmentMediaType {
        &self.media_type
    }

    /// Safe basename sent as provider display metadata.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Exact verified bytes for protocol serialization.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ChatDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatDocument")
            .field("media_type", &self.media_type)
            .field("has_filename", &true)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

pub(crate) fn image_base64(image: &ChatImage) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(image.bytes())
}

pub(crate) fn image_data_url(image: &ChatImage) -> String {
    format!(
        "data:{};base64,{}",
        image.media_type().as_str(),
        image_base64(image)
    )
}

pub(crate) fn document_base64(document: &ChatDocument) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(document.bytes())
}

pub(crate) fn document_data_url(document: &ChatDocument) -> String {
    format!(
        "data:{};base64,{}",
        document.media_type().as_str(),
        document_base64(document)
    )
}

/// One message in a chat transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Who authored the message.
    pub role: Role,
    /// Text content; empty when the message carries only tool calls.
    pub content: String,
    /// Ordered verified image inputs (`Role::User` only).
    pub images: Vec<ChatImage>,
    /// Ordered verified native document inputs (`Role::User` only).
    pub documents: Vec<ChatDocument>,
    /// Tool invocations requested by the model (`Role::Assistant` only).
    pub tool_calls: Option<Vec<ChatToolCall>>,
    /// Id of the tool call answered by this message (`Role::Tool` only).
    pub tool_call_id: Option<String>,
    /// Whether this tool result represents a failure or denial (`Role::Tool` only).
    pub tool_result_is_error: Option<bool>,
}

impl ChatMessage {
    /// A system message with `content`.
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            tool_result_is_error: None,
        }
    }

    /// A user message with `content`.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            tool_result_is_error: None,
        }
    }

    /// A user message with ordered image inputs.
    #[must_use]
    pub fn user_with_images(content: impl Into<String>, images: Vec<ChatImage>) -> Self {
        Self::user_with_media(content, images, Vec::new())
    }

    /// A user message with ordered image and document inputs.
    #[must_use]
    pub fn user_with_media(
        content: impl Into<String>,
        images: Vec<ChatImage>,
        documents: Vec<ChatDocument>,
    ) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            images,
            documents,
            tool_calls: None,
            tool_call_id: None,
            tool_result_is_error: None,
        }
    }

    /// An assistant message with `content`.
    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            tool_result_is_error: None,
        }
    }

    /// An assistant message carrying tool invocations instead of plain text.
    #[must_use]
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ChatToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            tool_result_is_error: None,
        }
    }

    /// A tool-result message answering the call `tool_call_id`.
    #[must_use]
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::tool_result(tool_call_id, content, false)
    }

    /// A tool-result message with its durable execution outcome.
    #[must_use]
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            images: Vec::new(),
            documents: Vec::new(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            tool_result_is_error: Some(is_error),
        }
    }
}

/// Normalized reason a provider adapter recognized at terminal settlement.
/// Protocol-specific unknown or failure reasons may instead terminate the
/// stream with an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// The model finished its turn without requesting tools.
    Stop,
    /// The model requested tool executions.
    ToolCalls,
    /// A provider-native loop paused and requires exact-state continuation.
    Pause,
    /// Generation hit the token limit.
    Length,
}

/// One incremental piece of a streamed completion.
///
/// Well-formed streams obey: exactly one [`StreamChunk::Usage`], emitted
/// immediately before [`StreamChunk::Finish`], and nothing after `Finish`.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    /// Text to append to the assistant message content.
    TextDelta(String),
    /// Text to append to the assistant reasoning trace.
    ReasoningDelta(String),
    /// Partial tool-call data. `id` and `name` arrive on the first fragment
    /// of a call; argument fragments sharing an `index` concatenate in
    /// stream order.
    ToolCallDelta {
        /// Position of this call within the request's tool-call list.
        index: u16,
        /// Call id, present on the first fragment of the call.
        id: Option<String>,
        /// Tool name, present on the first fragment of the call.
        name: Option<String>,
        /// Fragment of the raw JSON arguments text.
        arguments_delta: String,
    },
    /// Token usage for the whole request.
    Usage(TokenUsage),
    /// Terminal marker of the stream.
    Finish(FinishReason),
}

/// One chat completion request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Model id to run; authoritative — providers must not substitute defaults.
    pub model: String,
    /// Transcript so far, oldest first.
    pub messages: Vec<ChatMessage>,
    /// Tools offered to the model; `None` disables tool use entirely.
    pub tools: Option<Vec<ToolSpec>>,
    /// Sampling temperature override.
    pub temperature: Option<f32>,
    /// Generation token-budget override.
    pub max_tokens: Option<u32>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn helpers_set_role_and_optional_fields() {
        let system = ChatMessage::system("be terse");
        assert_eq!(system.role, Role::System);
        assert_eq!(system.content, "be terse");
        assert!(system.tool_calls.is_none() && system.tool_call_id.is_none());

        let assistant = ChatMessage::assistant_with_tool_calls(
            "",
            vec![ChatToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        );
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(assistant.tool_calls.unwrap().len(), 1);

        let tool = ChatMessage::tool("c1", "file body");
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("c1"));
    }
}
