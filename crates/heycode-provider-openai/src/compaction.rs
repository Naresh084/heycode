//! Provider-owned native Responses compaction boundary.
//!
//! `POST /v1/responses/compact` is a buffered operation distinct from normal
//! streaming inference. The returned compaction item is encrypted provider
//! state: this module validates its envelope, retains every output item
//! unchanged and never interprets or renders the opaque checkpoint.
//!
//! Primary source:
//! <https://developers.openai.com/api/reference/java/resources/responses/methods/compact>.

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind};
use heycode_http::{HttpRequest, HttpService, TransportError};
use heycode_llm::{
    CallPurpose, CapabilitySupport, InferenceInput, NativeCompactionAdapter,
    NativeCompactionCheckpoint, NativeCompactionError, NativeCompactionFuture, NativeFeature,
    ResolvedCall, Role, RouteCredential,
};
use tokio_util::sync::CancellationToken;

use crate::OPENAI_GPT_5_6_SOL;
use crate::catalog::{DEFAULT_BASE_URL, safe_model_id};
use crate::prompt_cache::OpenAiCacheUsage;

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_INPUT_ITEMS: usize = 2_048;
const MAX_CHECKPOINT_ITEMS: usize = 256;

/// Exact capability evidence for native compaction.
///
/// The maintained default has a current official endpoint example. Other
/// account-scoped model ids remain Unknown because `/v1/models` exposes no
/// compaction field.
#[must_use]
pub fn openai_compaction_support(model: &str) -> CapabilitySupport {
    if model == OPENAI_GPT_5_6_SOL {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    }
}

/// Injected buffered client for `POST /v1/responses/compact`.
pub struct OpenAiCompactionClient {
    http: HttpService,
    url: String,
    credential: RouteCredential,
}

impl OpenAiCompactionClient {
    /// Build the official OpenAI compaction endpoint.
    ///
    /// # Errors
    /// Empty/invalid credentials or an unusable endpoint fail before use.
    pub fn new(
        http: HttpService,
        api_key: impl Into<String>,
    ) -> Result<Self, OpenAiCompactionFault> {
        Self::with_base_url(http, DEFAULT_BASE_URL, api_key)
    }

    /// Build against an explicit OpenAI-compatible API origin.
    ///
    /// # Errors
    /// Empty/invalid credentials or an unusable endpoint fail before use.
    pub fn with_base_url(
        http: HttpService,
        base_url: impl AsRef<str>,
        api_key: impl Into<String>,
    ) -> Result<Self, OpenAiCompactionFault> {
        let api_key = api_key.into();
        if api_key.is_empty() || api_key.chars().any(char::is_control) {
            return Err(OpenAiCompactionFault::InvalidConfiguration);
        }
        Self::with_route_credential(http, base_url, RouteCredential::fixed(api_key))
    }

    pub(crate) fn with_route_credential(
        http: HttpService,
        base_url: impl AsRef<str>,
        credential: RouteCredential,
    ) -> Result<Self, OpenAiCompactionFault> {
        let url = format!(
            "{}/v1/responses/compact",
            base_url.as_ref().trim_end_matches('/')
        );
        HttpRequest::post(&url, Vec::new())
            .map_err(|_| OpenAiCompactionFault::InvalidConfiguration)?;
        Ok(Self {
            http,
            url,
            credential,
        })
    }

    /// Compact exact Responses input items into a durable opaque checkpoint.
    ///
    /// The input values are already provider wire items. They are structurally
    /// admitted here and then sent unchanged; no message or output item is
    /// distilled by this layer.
    ///
    /// # Errors
    /// Unproven model support, malformed input, cancellation, transport/server
    /// failure or an invalid response returns a closed safe fault.
    pub async fn compact(
        &self,
        model: &str,
        input: Vec<serde_json::Value>,
        cancellation: CancellationToken,
    ) -> Result<OpenAiCompactionCheckpoint, OpenAiCompactionFault> {
        self.compact_request(model, input, None, cancellation).await
    }

    async fn compact_request(
        &self,
        model: &str,
        input: Vec<serde_json::Value>,
        instructions: Option<&str>,
        cancellation: CancellationToken,
    ) -> Result<OpenAiCompactionCheckpoint, OpenAiCompactionFault> {
        if cancellation.is_cancelled() {
            return Err(OpenAiCompactionFault::Cancelled);
        }
        match openai_compaction_support(model) {
            CapabilitySupport::Supported => {}
            CapabilitySupport::Unsupported => {
                return Err(OpenAiCompactionFault::UnsupportedCapability);
            }
            CapabilitySupport::Unknown => {
                return Err(OpenAiCompactionFault::UnprovenCapability);
            }
        }
        if !safe_model_id(model) || !valid_wire_items(&input) {
            return Err(OpenAiCompactionFault::InvalidConfiguration);
        }
        let mut body = serde_json::json!({"model":model,"input":input});
        if let Some(instructions) = instructions {
            body["instructions"] = serde_json::json!(instructions);
        }
        let body =
            serde_json::to_vec(&body).map_err(|_| OpenAiCompactionFault::InvalidConfiguration)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(OpenAiCompactionFault::InvalidConfiguration);
        }
        let credential = self
            .credential
            .acquire()
            .map_err(|_| OpenAiCompactionFault::InvalidConfiguration)?;
        let request = HttpRequest::post(&self.url, body)
            .and_then(|request| {
                request.header("authorization", &format!("Bearer {}", credential.expose()))
            })
            .and_then(|request| request.header("content-type", "application/json"))
            .map(|request| request.with_max_response_bytes(MAX_RESPONSE_BYTES))
            .map_err(|_| OpenAiCompactionFault::InvalidConfiguration)?;
        let response = self
            .http
            .send(request, cancellation)
            .await
            .map_err(map_transport_fault)?;
        if response.status != 200 {
            return Err(OpenAiCompactionFault::Rejected);
        }
        if response.content_type.as_deref() != Some("application/json") {
            return Err(OpenAiCompactionFault::InvalidResponse);
        }
        let value = serde_json::from_slice(&response.body)
            .map_err(|_| OpenAiCompactionFault::InvalidResponse)?;
        parse_checkpoint(model, value)
    }
}

impl NativeCompactionAdapter for OpenAiCompactionClient {
    fn compact(
        &self,
        call: ResolvedCall,
        cancellation: CancellationToken,
    ) -> NativeCompactionFuture<'_> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(NativeCompactionError::Cancelled);
            }
            validate_resolved_call(&call)?;
            let model = call.model().to_owned();
            let input = compact_input(&call)?;
            let checkpoint = self
                .compact_request(&model, input, call.system(), cancellation)
                .await
                .map_err(map_native_fault)?;
            let usage = checkpoint.cache_usage();
            NativeCompactionCheckpoint::new(
                checkpoint.items().to_vec(),
                Some(heycode_core::TokenUsage {
                    prompt_tokens: usage.input_tokens(),
                    completion_tokens: usage.output_tokens(),
                }),
            )
        })
    }
}

/// Validated compaction response whose output items can be persisted directly.
#[derive(Clone, PartialEq)]
pub struct OpenAiCompactionCheckpoint {
    response_id: String,
    created_at: u64,
    items: Vec<ProviderStateItem>,
    usage: serde_json::Value,
    cache_usage: OpenAiCacheUsage,
}

impl OpenAiCompactionCheckpoint {
    /// Provider response id.
    #[must_use]
    pub fn response_id(&self) -> &str {
        &self.response_id
    }

    /// Provider creation timestamp in Unix seconds.
    #[must_use]
    pub const fn created_at(&self) -> u64 {
        self.created_at
    }

    /// Exact output list, suitable for durable provider-state storage.
    #[must_use]
    pub fn items(&self) -> &[ProviderStateItem] {
        &self.items
    }

    /// Final exact opaque compaction item.
    #[must_use]
    pub fn compaction(&self) -> &ProviderStateItem {
        // Construction proves a nonempty list ending in a compaction item.
        &self.items[self.items.len() - 1]
    }

    /// Exact usage object for the POA05 cache-usage boundary.
    #[must_use]
    pub const fn usage(&self) -> &serde_json::Value {
        &self.usage
    }

    /// Checked cache-read/write and reasoning usage components.
    #[must_use]
    pub const fn cache_usage(&self) -> OpenAiCacheUsage {
        self.cache_usage
    }

    /// Build the next exact Responses input sequence from this compacted
    /// output and newly appended wire items.
    ///
    /// # Errors
    /// Empty, malformed or oversized appended input is refused.
    pub fn continuation_input(
        &self,
        appended: Vec<serde_json::Value>,
    ) -> Result<Vec<serde_json::Value>, OpenAiCompactionFault> {
        if !valid_wire_items(&appended)
            || self.items.len().saturating_add(appended.len()) > MAX_INPUT_ITEMS
        {
            return Err(OpenAiCompactionFault::InvalidConfiguration);
        }
        Ok(self
            .items
            .iter()
            .map(|item| item.data().clone())
            .chain(appended)
            .collect())
    }
}

impl std::fmt::Debug for OpenAiCompactionCheckpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompactionCheckpoint")
            .field("created_at", &self.created_at)
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

/// Closed compaction failure that carries no request/response/provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAiCompactionFault {
    /// Local endpoint, credential or input configuration is invalid.
    InvalidConfiguration,
    /// Exact evidence says the model cannot compact.
    UnsupportedCapability,
    /// No exact capability evidence exists for the selected model.
    UnprovenCapability,
    /// The caller cancelled before the checkpoint commit point.
    Cancelled,
    /// Transport failed without exposing its diagnostic text.
    Transport,
    /// Provider refused the compaction request.
    Rejected,
    /// Success response was malformed or unsafe.
    InvalidResponse,
}

impl std::fmt::Display for OpenAiCompactionFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "OpenAI compaction configuration is invalid",
            Self::UnsupportedCapability => "OpenAI compaction is unsupported by this model",
            Self::UnprovenCapability => "OpenAI compaction capability is unproven",
            Self::Cancelled => "OpenAI compaction was cancelled",
            Self::Transport => "OpenAI compaction transport failed",
            Self::Rejected => "OpenAI compaction request was rejected",
            Self::InvalidResponse => "OpenAI compaction response is invalid",
        })
    }
}

impl std::error::Error for OpenAiCompactionFault {}

fn parse_checkpoint(
    model: &str,
    value: serde_json::Value,
) -> Result<OpenAiCompactionCheckpoint, OpenAiCompactionFault> {
    let object = value
        .as_object()
        .ok_or(OpenAiCompactionFault::InvalidResponse)?;
    if object.get("object").and_then(serde_json::Value::as_str) != Some("response.compaction") {
        return Err(OpenAiCompactionFault::InvalidResponse);
    }
    let response_id = object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| safe_item_id(id))
        .ok_or(OpenAiCompactionFault::InvalidResponse)?
        .to_owned();
    let created_at = object
        .get("created_at")
        .and_then(serde_json::Value::as_u64)
        .ok_or(OpenAiCompactionFault::InvalidResponse)?;
    let output = object
        .get("output")
        .and_then(serde_json::Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= MAX_CHECKPOINT_ITEMS)
        .ok_or(OpenAiCompactionFault::InvalidResponse)?;
    let usage = object
        .get("usage")
        .filter(|usage| usage.is_object())
        .ok_or(OpenAiCompactionFault::InvalidResponse)?
        .clone();
    let cache_usage =
        OpenAiCacheUsage::from_usage(&usage).map_err(|_| OpenAiCompactionFault::InvalidResponse)?;

    let final_index = output.len() - 1;
    let mut items = Vec::with_capacity(output.len());
    for (index, item) in output.iter().enumerate() {
        let item_object = item
            .as_object()
            .ok_or(OpenAiCompactionFault::InvalidResponse)?;
        let kind = item_object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(OpenAiCompactionFault::InvalidResponse)?;
        let id = item_object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| safe_item_id(id))
            .ok_or(OpenAiCompactionFault::InvalidResponse)?;
        let valid = if index == final_index {
            kind == "compaction"
                && item_object
                    .get("encrypted_content")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|content| !content.is_empty())
        } else {
            kind == "message"
                && item_object.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && item_object
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    == Some("completed")
                && item_object
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|content| !content.is_empty())
        };
        if !valid || id.is_empty() {
            return Err(OpenAiCompactionFault::InvalidResponse);
        }
        items.push(
            ProviderStateItem::new(
                "openai",
                model,
                ProviderProtocol::OpenAiResponses,
                ProviderStateKind::ResponseOutputItem,
                item.clone(),
            )
            .map_err(|_| OpenAiCompactionFault::InvalidResponse)?,
        );
    }
    Ok(OpenAiCompactionCheckpoint {
        response_id,
        created_at,
        items,
        usage,
        cache_usage,
    })
}

fn validate_resolved_call(call: &ResolvedCall) -> Result<(), NativeCompactionError> {
    if call.provider() != "openai"
        || call.protocol() != ProviderProtocol::OpenAiResponses
        || call.purpose() != CallPurpose::Compaction
        || call.native_features() != [NativeFeature::Compaction]
        || !call.tools().is_empty()
        || !call.native_tool_routes().is_empty()
        || !call.provider_options().is_empty()
        || call.reasoning_effort().is_some()
        || call.structured_output().is_some()
        || call.temperature().is_some()
        || call.max_output_tokens().is_some()
    {
        return Err(NativeCompactionError::InvalidCheckpoint);
    }
    Ok(())
}

fn compact_input(call: &ResolvedCall) -> Result<Vec<serde_json::Value>, NativeCompactionError> {
    let mut output = Vec::new();
    for input in call.inputs() {
        match input {
            InferenceInput::ProviderState(item) => output.push(item.data().clone()),
            InferenceInput::Message(message) => match message.role {
                Role::User => {
                    if message.content.is_empty()
                        || !message.images.is_empty()
                        || !message.documents.is_empty()
                    {
                        return Err(NativeCompactionError::InvalidCheckpoint);
                    }
                    output.push(serde_json::json!({
                        "type":"message",
                        "role":"user",
                        "content":[{"type":"input_text","text":message.content}],
                    }));
                }
                Role::System => {
                    if message.content.is_empty()
                        || !message.images.is_empty()
                        || !message.documents.is_empty()
                    {
                        return Err(NativeCompactionError::InvalidCheckpoint);
                    }
                    output.push(serde_json::json!({
                        "type":"message",
                        "role":"developer",
                        "content":[{"type":"input_text","text":message.content}],
                    }));
                }
                Role::Tool => {
                    if !message.images.is_empty() || !message.documents.is_empty() {
                        return Err(NativeCompactionError::InvalidCheckpoint);
                    }
                    let call_id = message
                        .tool_call_id
                        .as_deref()
                        .ok_or(NativeCompactionError::InvalidCheckpoint)?;
                    output.push(serde_json::json!({
                        "type":"function_call_output",
                        "call_id":call_id,
                        "output":message.content,
                    }));
                }
                Role::Assistant => {
                    return Err(NativeCompactionError::InvalidCheckpoint);
                }
            },
        }
    }
    if output.is_empty() {
        return Err(NativeCompactionError::InvalidCheckpoint);
    }
    Ok(output)
}

fn map_native_fault(fault: OpenAiCompactionFault) -> NativeCompactionError {
    match fault {
        OpenAiCompactionFault::Cancelled => NativeCompactionError::Cancelled,
        OpenAiCompactionFault::Transport => NativeCompactionError::Transport,
        OpenAiCompactionFault::Rejected => NativeCompactionError::Rejected,
        OpenAiCompactionFault::UnsupportedCapability
        | OpenAiCompactionFault::UnprovenCapability => NativeCompactionError::Unsupported,
        OpenAiCompactionFault::InvalidConfiguration | OpenAiCompactionFault::InvalidResponse => {
            NativeCompactionError::InvalidCheckpoint
        }
    }
}

fn safe_item_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-'))
}

fn valid_wire_items(items: &[serde_json::Value]) -> bool {
    !items.is_empty()
        && items.len() <= MAX_INPUT_ITEMS
        && items.iter().all(|item| {
            item.as_object()
                .and_then(|object| object.get("type"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| !kind.is_empty() && kind.trim() == kind)
        })
}

fn map_transport_fault(error: TransportError) -> OpenAiCompactionFault {
    if matches!(error, TransportError::Cancelled) {
        OpenAiCompactionFault::Cancelled
    } else {
        OpenAiCompactionFault::Transport
    }
}
