//! OpenAI-compatible `/chat/completions` wire adapter.
//!
//! One protocol client serves every current spec-table provider whose request
//! schema matches OpenAI Chat Completions. Raw HTTP, cancellation and SSE
//! framing are delegated to `heycode-http`; this module owns JSON semantics.

use futures::StreamExt;

use crate::error::LlmError;
use crate::provider::ChunkStream;
use crate::vocab::{ChatMessage, ChatRequest, Role, ToolSpec};

/// Connection settings for one provider endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Base URL ending at the version root, e.g. `https://api.deepseek.com`.
    pub base_url: String,
    /// Environment variable holding the bearer token.
    pub api_key_env: &'static str,
    /// Extra headers appended verbatim (attribution, auth proxies).
    pub extra_headers: Vec<(String, String)>,
}

/// HTTP client speaking the OpenAI-compatible SSE streaming protocol.
#[derive(Clone)]
pub struct OpenAiCompatClient {
    http: heycode_http::HttpService,
    base_url: String,
    credential: crate::RouteCredential,
    extra_headers: Vec<(String, String)>,
}

impl std::fmt::Debug for OpenAiCompatClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompatClient")
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("extra_header_count", &self.extra_headers.len())
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatClient {
    /// Build a client, resolving the API key from `cfg.api_key_env`. An unset
    /// or blank variable fails before any network activity.
    ///
    /// # Errors
    /// [`LlmError::MissingApiKey`] when the variable is unset or empty;
    /// A classified [`LlmError::Provider`] when HTTP construction fails.
    pub fn new(cfg: OpenAiCompatConfig) -> Result<Self, LlmError> {
        let api_key = match std::env::var(cfg.api_key_env) {
            Ok(key) if !key.trim().is_empty() => key,
            _ => {
                return Err(LlmError::MissingApiKey {
                    env: cfg.api_key_env,
                });
            }
        };
        Self::with_key(cfg, api_key)
    }

    /// Build a client with an explicitly supplied key, bypassing the
    /// environment. Used by tests and embedders that own credential storage.
    ///
    /// # Errors
    /// A classified [`LlmError::Provider`] when HTTP construction fails.
    pub fn with_key(cfg: OpenAiCompatConfig, api_key: impl Into<String>) -> Result<Self, LlmError> {
        let transport =
            heycode_http::ReqwestHttpTransport::new().map_err(crate::classify_transport_error)?;
        Self::with_key_and_transport(
            cfg,
            api_key,
            heycode_http::HttpService::new(std::sync::Arc::new(transport)),
        )
    }

    /// Build with an explicit key and plugin-provided shared HTTP transport.
    ///
    /// # Errors
    /// Reserved for compatibility with constructors whose transport creation
    /// can fail; this injected path currently performs no fallible work.
    pub fn with_key_and_transport(
        cfg: OpenAiCompatConfig,
        api_key: impl Into<String>,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        Self::with_credential_and_transport(cfg, crate::RouteCredential::fixed(api_key), http)
    }

    /// Build with a credential resolved once per operation and the shared
    /// transport. A route built this way sends the key that is in the store
    /// when the request is made, not the one that was there at composition.
    ///
    /// # Errors
    /// Reserved for compatibility with constructors whose transport creation
    /// can fail; this injected path currently performs no fallible work.
    pub fn with_credential_and_transport(
        cfg: OpenAiCompatConfig,
        credential: crate::RouteCredential,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        Ok(Self {
            http,
            base_url: cfg.base_url,
            credential,
            extra_headers: cfg.extra_headers,
        })
    }

    /// Open a streaming completion against `{base_url}/chat/completions`.
    /// The stream always yields at least one item; failures arrive as
    /// Classified body-free [`LlmError::Provider`] items.
    #[must_use = "the stream runs only while polled"]
    pub fn stream(&self, model: &str, request: &ChatRequest) -> ChunkStream {
        // Resolved for this operation only; the next call resolves again.
        let credential = match self.credential.acquire() {
            Ok(credential) => credential,
            Err(error) => {
                return Box::pin(futures::stream::once(async move {
                    Err(LlmError::UnresolvedCredential(error))
                }));
            }
        };
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut wire = match heycode_http::HttpSseRequest::post(
            url,
            chat_request_body(model, request).to_string().into_bytes(),
        )
        .and_then(|request| {
            request.header("authorization", &format!("Bearer {}", credential.expose()))
        })
        .and_then(|request| request.header("content-type", "application/json"))
        {
            Ok(request) => request,
            Err(error) => {
                return Box::pin(futures::stream::once(async move {
                    Err(map_transport_error(error))
                }));
            }
        };
        for (name, value) in &self.extra_headers {
            wire = match wire.header(name, value) {
                Ok(request) => request,
                Err(error) => {
                    return Box::pin(futures::stream::once(async move {
                        Err(map_transport_error(error))
                    }));
                }
            };
        }
        let events = self
            .http
            .sse(wire, tokio_util::sync::CancellationToken::new());
        let inference = crate::chat::normalize_chat_events(
            events,
            "legacy-openai-compatible".to_owned(),
            model.to_owned(),
        );
        Box::pin(inference.filter_map(|item| async move {
            match item {
                Ok(crate::InferenceEvent::TextDelta(text)) => {
                    Some(Ok(crate::StreamChunk::TextDelta(text)))
                }
                Ok(crate::InferenceEvent::ReasoningDelta(text)) => {
                    Some(Ok(crate::StreamChunk::ReasoningDelta(text)))
                }
                Ok(crate::InferenceEvent::ToolCallDelta {
                    output_index,
                    id,
                    name,
                    arguments_delta,
                }) => Some(match u16::try_from(output_index) {
                    Ok(index) => Ok(crate::StreamChunk::ToolCallDelta {
                        index,
                        id: id.map(|id| id.as_str().to_owned()),
                        name,
                        arguments_delta,
                    }),
                    Err(_) => Err(LlmError::InvalidResponse(
                        "Chat tool index exceeds legacy u16 range".to_owned(),
                    )),
                }),
                Ok(crate::InferenceEvent::Usage(usage)) => {
                    Some(Ok(crate::StreamChunk::Usage(usage)))
                }
                Ok(crate::InferenceEvent::Finish(reason)) => {
                    Some(Ok(crate::StreamChunk::Finish(reason)))
                }
                Ok(
                    crate::InferenceEvent::ResponseStarted { .. }
                    | crate::InferenceEvent::ItemStarted { .. }
                    | crate::InferenceEvent::ItemFinished { .. }
                    | crate::InferenceEvent::ServerToolCall { .. }
                    | crate::InferenceEvent::ServerToolResult { .. }
                    | crate::InferenceEvent::ServerToolUsage(_)
                    | crate::InferenceEvent::Citation { .. }
                    | crate::InferenceEvent::ProviderState(_)
                    | crate::InferenceEvent::ResponseMetadata(_)
                    | crate::InferenceEvent::ResponseFinished { .. },
                ) => None,
                Err(error) => Some(Err(error)),
            }
        }))
    }
}

fn map_transport_error(error: heycode_http::TransportError) -> LlmError {
    crate::classify_transport_error(error)
}

/// Translate a request into the OpenAI-compatible JSON body: streaming on,
/// usage requested, absent optionals omitted.
fn chat_request_body(model: &str, request: &ChatRequest) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = request.messages.iter().map(wire_message).collect();
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(tools) = request.tools.as_deref().filter(|tools| !tools.is_empty()) {
        body["tools"] = serde_json::Value::Array(tools.iter().map(wire_tool).collect());
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }
    body
}

/// One transcript message on the wire: tool calls ride assistant messages,
/// `tool_call_id` rides tool-result messages.
fn wire_message(message: &ChatMessage) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let content = if message.role == Role::User
        && (!message.images.is_empty() || !message.documents.is_empty())
    {
        let mut parts = Vec::with_capacity(message.documents.len() + message.images.len() + 1);
        parts.extend(message.documents.iter().map(|document| {
            serde_json::json!({
                "type":"file",
                "file":{
                    "filename":document.filename(),
                    "file_data":crate::vocab::document_data_url(document),
                }
            })
        }));
        if !message.content.is_empty() {
            parts.push(serde_json::json!({"type":"text","text":message.content}));
        }
        parts.extend(message.images.iter().map(|image| {
            serde_json::json!({
                "type":"image_url",
                "image_url":{"url":crate::vocab::image_data_url(image),"detail":"auto"}
            })
        }));
        serde_json::Value::Array(parts)
    } else {
        serde_json::Value::String(message.content.clone())
    };
    let mut wire = serde_json::json!({ "role": role, "content": content });
    if let Some(tool_calls) = &message.tool_calls {
        wire["tool_calls"] = serde_json::Value::Array(
            tool_calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments },
                    })
                })
                .collect(),
        );
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        wire["tool_call_id"] = serde_json::json!(tool_call_id);
    }
    wire
}

/// One tool definition on the wire, wrapped in the function envelope.
fn wire_tool(spec: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "parameters": spec.parameters,
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::vocab::ChatToolCall;

    /// Environment variable name that no other process or test sets.
    const UNSET_KEY_ENV: &str = "HEYCODE_TEST_NO_KEY_42";

    fn sample_request() -> ChatRequest {
        ChatRequest {
            model: "deepseek-chat".into(),
            messages: vec![
                ChatMessage::system("be terse"),
                ChatMessage::user("hi"),
                ChatMessage::assistant_with_tool_calls(
                    "",
                    vec![ChatToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: "{\"f\":1}".into(),
                    }],
                ),
                ChatMessage::tool("c1", "contents"),
            ],
            tools: Some(vec![ToolSpec {
                name: "read".into(),
                description: "read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            temperature: Some(0.5),
            max_tokens: None,
        }
    }

    #[test]
    fn missing_key_fails_before_network_with_setup_hint() {
        let err = OpenAiCompatClient::new(OpenAiCompatConfig {
            base_url: "https://example.invalid".into(),
            api_key_env: UNSET_KEY_ENV,
            extra_headers: Vec::new(),
        })
        .unwrap_err();

        match err {
            LlmError::MissingApiKey { env } => {
                assert_eq!(env, UNSET_KEY_ENV);
                assert!(err.to_string().contains("export it or set [llm] config"));
            }
            other => panic!("expected MissingApiKey, got {other:?}"),
        }
    }

    #[test]
    fn body_carries_streaming_options_and_omits_absent_optionals() {
        let body = chat_request_body("deepseek-chat", &sample_request());
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["temperature"], 0.5);
        assert!(
            body.get("max_tokens").is_none(),
            "absent optionals are omitted"
        );
    }

    #[test]
    fn messages_translate_tool_calls_and_tool_results() {
        let body = chat_request_body("m", &sample_request());
        let messages = body["messages"].as_array().unwrap();

        let assistant = &messages[2];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["tool_calls"][0]["id"], "c1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            "{\"f\":1}"
        );

        let tool = &messages[3];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "c1");
    }

    #[test]
    fn tools_wrap_in_function_envelope_and_vanish_when_empty() {
        let body = chat_request_body("m", &sample_request());
        let tool = &body["tools"][0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "read");
        assert_eq!(tool["function"]["parameters"]["type"], "object");

        let mut no_tools = sample_request();
        no_tools.tools = Some(Vec::new());
        assert!(chat_request_body("m", &no_tools).get("tools").is_none());
        no_tools.tools = None;
        assert!(chat_request_body("m", &no_tools).get("tools").is_none());
    }

    #[test]
    fn transport_http_error_classifies_without_echoing_the_body() {
        let error = map_transport_error(heycode_http::TransportError::http(
            429,
            "secret body",
            heycode_http::HttpErrorMetadata::default(),
        ));
        assert_eq!(error.class(), crate::ProviderErrorClass::RateLimited);
        assert!(!format!("{error:?} {error}").contains("secret body"));
    }
}
