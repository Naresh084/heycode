//! Amazon Bedrock `ConverseStream` protocol adapter (P07), including the
//! `application/vnd.amazon.eventstream` binary framing the response arrives in.
//!
//! Two layers live here and stay separate, exactly as SSE framing and OpenAI
//! JSON stay separate in [`crate::sse`] (GOTCHAS #47):
//!
//! * [`EventStreamDecoder`] owns bytes. It is fed arbitrary network fragments,
//!   validates both documented checksums, bounds every length against what
//!   actually arrived, and yields whole messages. A framing failure is a typed
//!   error, never a panic and never a silent truncation.
//! * [`BedrockStreamParser`] owns meaning. It reads the `:message-type` /
//!   `:event-type` headers and the JSON payload of each message and emits
//!   normalized [`InferenceEvent`]s.
//!
//! Every wire fact encoded below cites the AWS/Smithy documentation it came
//! from next to the constant or type it describes.
//!
//! One exact provider option, `runtime-metadata`, crosses cache checkpoints,
//! guardrails and source/target route evidence into the request. Other scope
//! boundaries fail before transport: image/document input, structured output,
//! provider-native features, reasoning effort (Bedrock puts thinking in
//! model-specific `additionalModelRequestFields`, not in a Converse-level
//! field) and provider-state replay (`heycode-core` has no `ProviderStateKind` for
//! Bedrock). Opaque reasoning state therefore fails loud rather than being
//! dropped. SigV4 signing belongs to PAWS01; this adapter takes an
//! already-authorized bearer credential.

use std::collections::{BTreeMap, BTreeSet};

use futures::StreamExt as _;

use crate::{
    AuthenticationBinding, ChatMessage, FinishReason, InferenceAdapter, InferenceEvent,
    InferenceInput, InferenceStream, InferenceTarget, LlmError, ModelDescriptor,
    ProviderDescriptor, ProviderProtocol, RequestDraft, ResolveError, ResolveSpec, ResolvedCall,
    Role, StreamItemKind, TokenUsage, ToolSpec, resolve_request,
};

// ---------------------------------------------------------------------------
// `application/vnd.amazon.eventstream` framing
// ---------------------------------------------------------------------------

/// Bytes of the fixed prelude: `uint32 total_length`, `uint32 headers_length`,
/// `uint32 prelude_crc`.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
const PRELUDE_BYTES: usize = 12;

/// Bytes of the prelude covered by `prelude_crc`: the two length fields only.
/// The prelude carries its own checksum so corrupted length information is
/// detected before it is used to size a read.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
const PRELUDE_CHECKED_BYTES: usize = 8;

/// Smallest legal message: the prelude plus the trailing `uint32 message_crc`,
/// i.e. no headers and no payload.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
const MESSAGE_OVERHEAD_BYTES: usize = PRELUDE_BYTES + 4;

/// "The encoded headers of a message MUST NOT exceed 131,072 bytes (128 kB)."
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
const MAX_HEADER_BYTES: usize = 131_072;

/// "The payload of a message MUST NOT exceed 25,165,824 bytes (24 MB)."
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
const MAX_PAYLOAD_BYTES: usize = 25_165_824;

/// Hard cap on one framed message.
///
/// The specification says "Services MUST validate these additional size
/// restrictions. Clients MUST NOT validate them"
/// (<https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>). This
/// decoder deliberately deviates: `total_length` is attacker-influenced and is
/// used to decide how many bytes to accumulate, so trusting a 4 GiB value is an
/// unbounded allocation. The cap is set to exactly the largest message the
/// specification permits, so no conformant message can be rejected by it.
const MAX_MESSAGE_BYTES: usize = MESSAGE_OVERHEAD_BYTES + MAX_HEADER_BYTES + MAX_PAYLOAD_BYTES;

/// Reflected CRC-32 (IEEE 802.3 / "GZIP CRC32") lookup table. Both event-stream
/// checksums use this algorithm.
///
/// <https://docs.aws.amazon.com/transcribe/latest/dg/event-stream.html>
const CRC32_TABLE: [u32; 256] = build_crc32_table();

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0_usize;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0_u8;
        while bit < 8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

/// "the checksum of all the data from the start of the message to the start of
/// the checksum."
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-format>
fn crc32(bytes: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFF_u32;
    for byte in bytes {
        let index = usize::from((state as u8) ^ *byte);
        state = CRC32_TABLE[index] ^ (state >> 8);
    }
    state ^ 0xFFFF_FFFF
}

/// Event-stream framing failure. Every variant carries lengths or indicators
/// only — never a byte of provider payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum EventStreamError {
    /// `prelude_crc` disagreed with the two length fields.
    #[error("event stream prelude checksum is invalid")]
    PreludeChecksum,
    /// `message_crc` disagreed with the framed message.
    #[error("event stream message checksum is invalid")]
    MessageChecksum,
    /// The declared lengths cannot describe a message.
    #[error("event stream length fields {total_length}/{headers_length} are inconsistent")]
    InvalidLength {
        /// Declared `total_length`.
        total_length: u32,
        /// Declared `headers_length`.
        headers_length: u32,
    },
    /// The declared message exceeds the documented maximum.
    #[error(
        "event stream message of {total_length} bytes exceeds the {MAX_MESSAGE_BYTES}-byte cap"
    )]
    MessageTooLarge {
        /// Declared `total_length`.
        total_length: u32,
    },
    /// The declared header block exceeds the documented maximum.
    #[error(
        "event stream headers of {headers_length} bytes exceed the {MAX_HEADER_BYTES}-byte cap"
    )]
    HeadersTooLarge {
        /// Declared `headers_length`.
        headers_length: u32,
    },
    /// A header could not be decoded within the declared header block.
    #[error("event stream header `{field}` is malformed")]
    InvalidHeader {
        /// Stable structural field name.
        field: &'static str,
    },
    /// The header type indicator is outside the fixed, non-extensible set.
    #[error("event stream header type indicator {indicator} is unknown")]
    UnknownHeaderType {
        /// Rejected indicator byte.
        indicator: u8,
    },
    /// The response ended part-way through a message.
    #[error("event stream ended with {pending_bytes} bytes of an incomplete message")]
    Truncated {
        /// Bytes of the partial message still buffered.
        pending_bytes: usize,
    },
}

/// One decoded event-stream message.
///
/// Only string-typed headers are retained: every header this protocol reads
/// (`:message-type`, `:event-type`, `:content-type`, `:exception-type`,
/// `:error-code`) is a string, so retaining values nothing reads would be dead
/// weight. Non-string values are still bounds-validated by their exact encoded
/// width, because a mis-sized skip would desynchronize the header block.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#amazon-event-semantics>
#[derive(Clone, PartialEq, Eq)]
struct EventStreamMessage {
    headers: BTreeMap<String, String>,
    payload: Vec<u8>,
}

impl EventStreamMessage {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

impl std::fmt::Debug for EventStreamMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EventStreamMessage")
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Incremental decoder for one `application/vnd.amazon.eventstream` body. Feed
/// network fragments as they arrive; call [`EventStreamDecoder::finish`] once
/// the body ends.
#[derive(Debug, Default)]
struct EventStreamDecoder {
    buffer: Vec<u8>,
}

impl EventStreamDecoder {
    fn new() -> Self {
        Self::default()
    }

    /// Ingest `bytes` and return every message they completed.
    ///
    /// # Errors
    /// Any checksum, length, cap or header-encoding violation. The decoder is
    /// not usable after an error; the caller terminates the stream.
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<EventStreamMessage>, EventStreamError> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while self.buffer.len() >= PRELUDE_BYTES {
            let prelude: [u8; PRELUDE_BYTES] = self
                .buffer
                .get(..PRELUDE_BYTES)
                .and_then(|slice| <[u8; PRELUDE_BYTES]>::try_from(slice).ok())
                .ok_or(EventStreamError::InvalidHeader { field: "prelude" })?;
            let total_length = be_u32(&prelude, 0, "total_length")?;
            let headers_length = be_u32(&prelude, 4, "headers_length")?;
            let prelude_crc = be_u32(&prelude, 8, "prelude_crc")?;
            let checked = prelude
                .get(..PRELUDE_CHECKED_BYTES)
                .ok_or(EventStreamError::InvalidHeader { field: "prelude" })?;
            if crc32(checked) != prelude_crc {
                return Err(EventStreamError::PreludeChecksum);
            }

            // Every length is validated before it is used to slice or to
            // decide how many more bytes to accumulate.
            let total = usize::try_from(total_length)
                .map_err(|_| EventStreamError::MessageTooLarge { total_length })?;
            let headers = usize::try_from(headers_length)
                .map_err(|_| EventStreamError::HeadersTooLarge { headers_length })?;
            if total < MESSAGE_OVERHEAD_BYTES || headers > total - MESSAGE_OVERHEAD_BYTES {
                return Err(EventStreamError::InvalidLength {
                    total_length,
                    headers_length,
                });
            }
            if total > MAX_MESSAGE_BYTES {
                return Err(EventStreamError::MessageTooLarge { total_length });
            }
            if headers > MAX_HEADER_BYTES {
                return Err(EventStreamError::HeadersTooLarge { headers_length });
            }
            if self.buffer.len() < total {
                break;
            }

            let message_crc = be_u32(&self.buffer, total - 4, "message_crc")?;
            let covered = self
                .buffer
                .get(..total - 4)
                .ok_or(EventStreamError::InvalidHeader { field: "message" })?;
            if crc32(covered) != message_crc {
                return Err(EventStreamError::MessageChecksum);
            }
            let header_end = PRELUDE_BYTES + headers;
            let header_bytes = self
                .buffer
                .get(PRELUDE_BYTES..header_end)
                .ok_or(EventStreamError::InvalidHeader { field: "headers" })?
                .to_vec();
            let payload = self
                .buffer
                .get(header_end..total - 4)
                .ok_or(EventStreamError::InvalidHeader { field: "payload" })?
                .to_vec();
            messages.push(EventStreamMessage {
                headers: parse_headers(&header_bytes)?,
                payload,
            });
            self.buffer.drain(..total);
        }
        Ok(messages)
    }

    /// Assert the body ended on a message boundary.
    ///
    /// # Errors
    /// Buffered bytes that never completed a message.
    fn finish(self) -> Result<(), EventStreamError> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(EventStreamError::Truncated {
                pending_bytes: self.buffer.len(),
            })
        }
    }
}

fn be_u32(bytes: &[u8], offset: usize, field: &'static str) -> Result<u32, EventStreamError> {
    let end = offset
        .checked_add(4)
        .ok_or(EventStreamError::InvalidHeader { field })?;
    let value: [u8; 4] = bytes
        .get(offset..end)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .ok_or(EventStreamError::InvalidHeader { field })?;
    Ok(u32::from_be_bytes(value))
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
    field: &'static str,
) -> Result<&'a [u8], EventStreamError> {
    let end = cursor
        .checked_add(length)
        .ok_or(EventStreamError::InvalidHeader { field })?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or(EventStreamError::InvalidHeader { field })?;
    *cursor = end;
    Ok(slice)
}

/// Decode the header block: a one-byte name length, the UTF-8 name, a one-byte
/// type indicator and the type's value.
///
/// Indicators: `0` boolean true, `1` boolean false, `2` byte, `3` short,
/// `4` integer, `5` long, `6` byte_array, `7` string, `8` timestamp, `9` uuid.
/// `byte_array` and `string` are prefixed by a two-byte unsigned length.
/// "The set of types is fixed and not open to extension."
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#headers-format>
fn parse_headers(bytes: &[u8]) -> Result<BTreeMap<String, String>, EventStreamError> {
    let mut retained = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let name_length = usize::from(take_u8(bytes, &mut cursor, "name length")?);
        // "Header names MUST be at least one byte long."
        if name_length == 0 {
            return Err(EventStreamError::InvalidHeader {
                field: "name length",
            });
        }
        let name = std::str::from_utf8(take(bytes, &mut cursor, name_length, "name")?)
            .map_err(|_| EventStreamError::InvalidHeader { field: "name" })?
            .to_owned();
        let indicator = take_u8(bytes, &mut cursor, "type")?;
        let value = match indicator {
            0 | 1 => None,
            2 => {
                take(bytes, &mut cursor, 1, "byte value")?;
                None
            }
            3 => {
                take(bytes, &mut cursor, 2, "short value")?;
                None
            }
            4 => {
                take(bytes, &mut cursor, 4, "integer value")?;
                None
            }
            5 | 8 => {
                take(bytes, &mut cursor, 8, "long value")?;
                None
            }
            6 => {
                let length = be_u16(bytes, &mut cursor, "byte array length")?;
                take(bytes, &mut cursor, length, "byte array value")?;
                None
            }
            7 => {
                let length = be_u16(bytes, &mut cursor, "string length")?;
                let text = std::str::from_utf8(take(bytes, &mut cursor, length, "string value")?)
                    .map_err(|_| EventStreamError::InvalidHeader {
                    field: "string value",
                })?;
                Some(text.to_owned())
            }
            9 => {
                take(bytes, &mut cursor, 16, "uuid value")?;
                None
            }
            indicator => return Err(EventStreamError::UnknownHeaderType { indicator }),
        };
        // "Any given header name MUST only appear once in a message."
        if !seen.insert(name.clone()) {
            return Err(EventStreamError::InvalidHeader {
                field: "duplicate name",
            });
        }
        if let Some(value) = value {
            retained.insert(name, value);
        }
    }
    Ok(retained)
}

fn take_u8(bytes: &[u8], cursor: &mut usize, field: &'static str) -> Result<u8, EventStreamError> {
    take(bytes, cursor, 1, field)?
        .first()
        .copied()
        .ok_or(EventStreamError::InvalidHeader { field })
}

fn be_u16(
    bytes: &[u8],
    cursor: &mut usize,
    field: &'static str,
) -> Result<usize, EventStreamError> {
    let value: [u8; 2] = take(bytes, cursor, 2, field)?
        .try_into()
        .map_err(|_| EventStreamError::InvalidHeader { field })?;
    Ok(usize::from(u16::from_be_bytes(value)))
}

// ---------------------------------------------------------------------------
// Route configuration
// ---------------------------------------------------------------------------

/// Documented response media type of a Bedrock `ConverseStream` body.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#amazon-event-stream-specification>
const EVENT_STREAM_MEDIA_TYPE: &str = "application/vnd.amazon.eventstream";

/// Documented `:content-type` of the JSON event payloads.
///
/// <https://smithy.io/2.0/aws/amazon-eventstream.html#message-events>
const EVENT_PAYLOAD_MEDIA_TYPE: &str = "application/json";

/// Default cap on one buffered `ConverseStream` response body. See
/// [`BedrockConverseConfig::with_max_response_bytes`].
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Reusable Bedrock `ConverseStream` route configuration. Debug output is
/// redacted.
#[derive(Clone)]
pub struct BedrockConverseConfig {
    provider: ProviderDescriptor,
    base_url: String,
    credential: crate::RouteCredential,
    extra_headers: Vec<(String, String)>,
    default_max_output_tokens: Option<u64>,
    max_response_bytes: usize,
    retry_spec: crate::RetrySpec,
}

impl BedrockConverseConfig {
    /// Build one already-authorized Bedrock Runtime route.
    ///
    /// `base_url` is the regional Bedrock Runtime endpoint, for example
    /// `https://bedrock-runtime.us-east-1.amazonaws.com`
    /// (<https://docs.aws.amazon.com/general/latest/gr/bedrock.html>).
    /// `api_key` is an Amazon Bedrock API key, sent as
    /// `Authorization: Bearer <key>`
    /// (<https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys-use.html>).
    ///
    /// The AWS credential-chain (SigV4) route is PAWS01's territory: a SigV4
    /// signature covers the method, URI, headers and body hash of each
    /// individual request, so it cannot be expressed as a fixed header here.
    #[must_use]
    pub fn with_api_key(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_credential(provider, base_url, crate::RouteCredential::fixed(api_key))
    }

    /// Build one already-authorized Bedrock Runtime route whose credential is
    /// resolved once per operation.
    #[must_use]
    pub fn with_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential,
            extra_headers: Vec::new(),
            default_max_output_tokens: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            retry_spec: crate::RetrySpec::standard(),
        }
    }

    /// Attach validated-at-dispatch attribution or gateway headers.
    #[must_use]
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Attach an adapter-owned output default. Converse leaves `maxTokens`
    /// optional and defaults to the model maximum, so `None` sends no cap.
    #[must_use]
    pub fn with_default_max_output_tokens(mut self, value: Option<u64>) -> Self {
        self.default_max_output_tokens = value;
        self
    }

    /// Cap one buffered response body. The shared transport exposes no
    /// raw-byte streaming seam, so the whole event-stream body is read before
    /// framing; this bounds that read.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Attach an explicit validated retry policy.
    #[must_use]
    pub fn with_retry_spec(mut self, retry_spec: crate::RetrySpec) -> Self {
        self.retry_spec = retry_spec;
        self
    }

    /// Secret-free exact-route resolution proposal.
    ///
    /// Bedrock exposes no Converse-level reasoning-effort field, so the exact
    /// effort list is empty and every requested effort fails resolution.
    #[must_use]
    pub fn resolve_spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::BedrockConverse,
            target: InferenceTarget::Http {
                base_url: self.base_url.clone(),
            },
            authentication: self.credential.binding(),
            default_max_output_tokens: self.default_max_output_tokens,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
        }
    }
}

impl std::fmt::Debug for BedrockConverseConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BedrockConverseConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("extra_header_count", &self.extra_headers.len())
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("retry_spec", &self.retry_spec)
            .finish()
    }
}

/// Reusable Amazon Bedrock `ConverseStream` protocol adapter.
#[derive(Clone)]
pub struct BedrockConverseAdapter {
    config: BedrockConverseConfig,
    http: heycode_http::HttpService,
}

impl BedrockConverseAdapter {
    /// Validate configuration and bind the shared HTTP service.
    ///
    /// # Errors
    /// Missing protocol declaration, blank credential, unusable endpoint or
    /// header, non-positive output default, or a zero response cap.
    pub fn new(
        config: BedrockConverseConfig,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        validate_config(&config)?;
        Ok(Self { config, http })
    }
}

fn validate_config(config: &BedrockConverseConfig) -> Result<(), LlmError> {
    if !config
        .provider
        .protocols
        .contains(&ProviderProtocol::BedrockConverse)
    {
        return Err(invalid(
            "Converse adapter provider does not declare Bedrock Converse",
        ));
    }
    if config.credential.fixed_is_blank() {
        return Err(crate::retry::local_failure(
            crate::ProviderErrorClass::Authentication,
        ));
    }
    if config.default_max_output_tokens == Some(0) {
        return Err(invalid("Converse adapter output default must be positive"));
    }
    if config.max_response_bytes == 0 {
        return Err(invalid("Converse response cap must be positive"));
    }
    let probe = converse_stream_url(&config.base_url, "probe")?;
    let probe_secret = config.credential.probe_value();
    let mut request = heycode_http::HttpRequest::post(probe, Vec::new())
        .and_then(|request| request.header("content-type", EVENT_PAYLOAD_MEDIA_TYPE))
        .and_then(|request| request.header("accept", EVENT_STREAM_MEDIA_TYPE))
        .and_then(|request| request.header("authorization", &format!("Bearer {probe_secret}")))
        .map_err(crate::classify_transport_error)?;
    let mut names = BTreeSet::new();
    for (name, value) in &config.extra_headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "content-type" | "accept" | "authorization"
        ) || !names.insert(normalized)
        {
            return Err(invalid(
                "Converse extra headers must be unique and cannot replace protocol/auth headers",
            ));
        }
        request = request
            .header(name, value)
            .map_err(crate::classify_transport_error)?;
    }
    drop(request);
    Ok(())
}

/// `POST /model/{modelId}/converse-stream`
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>
///
/// `modelId` is a non-greedy Smithy URI label, so every character outside the
/// RFC 3986 unreserved set is percent-encoded — model ARNs contain `/` and `:`.
///
/// <https://smithy.io/2.0/spec/http-bindings.html#httplabel-trait>
fn converse_stream_url(base_url: &str, model: &str) -> Result<String, LlmError> {
    if base_url.trim() != base_url || base_url.is_empty() {
        return Err(invalid(
            "Converse endpoint must be non-blank with no surrounding whitespace",
        ));
    }
    Ok(format!(
        "{}/model/{}/converse-stream",
        base_url.trim_end_matches('/'),
        encode_uri_label(model)
    ))
}

const HEX_DIGITS: [u8; 16] = *b"0123456789ABCDEF";

fn encode_uri_label(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

impl InferenceAdapter for BedrockConverseAdapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.config.provider.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.config.resolve_spec().authentication
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        validate_draft(&self.config, &draft)?;
        let call = resolve_request(
            &self.config.provider,
            draft,
            model,
            &self.config.resolve_spec(),
        )?
        .with_retry_spec(self.config.retry_spec.clone());
        Ok(call)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.stream_cancellable(call, tokio_util::sync::CancellationToken::new())
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        let body = match converse_stream_body(&self.config, &call) {
            Ok(body) => body.to_string().into_bytes(),
            Err(error) => return one_error(error),
        };
        let url = match converse_stream_url(&self.config.base_url, call.model()) {
            Ok(url) => url,
            Err(error) => return one_error(error),
        };
        let client_tools = call
            .tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<BTreeSet<_>>();
        let retry_spec = call.retry_spec().clone();
        let provider = call.provider().to_owned();
        let model = call.model().to_owned();
        let config = self.config.clone();
        let http = self.http.clone();
        // Resolved once, before the first attempt: every retry of this one
        // operation reuses it, and the next operation resolves again.
        let credential = match self.config.credential.acquire() {
            Ok(credential) => credential,
            Err(error) => return one_error(LlmError::UnresolvedCredential(error)),
        };
        crate::retry::retrying_stream(retry_spec, cancellation, move |attempt_cancellation| {
            let request =
                match converse_stream_request(&config, credential.expose(), &url, body.clone()) {
                    Ok(request) => request,
                    Err(error) => return one_error(error),
                };
            let response = http.send(request, attempt_cancellation);
            let client_tools = client_tools.clone();
            let provider = provider.clone();
            let model = model.clone();
            Box::pin(
                futures::stream::once(async move {
                    match response.await {
                        Ok(response) => {
                            decode_converse_stream(&response, &client_tools, &provider, &model)
                        }
                        Err(error) => vec![Err(crate::classify_transport_error(error))],
                    }
                })
                .flat_map(futures::stream::iter),
            )
        })
    }
}

fn converse_stream_request(
    config: &BedrockConverseConfig,
    secret: &str,
    url: &str,
    body: Vec<u8>,
) -> Result<heycode_http::HttpRequest, LlmError> {
    let mut request = heycode_http::HttpRequest::post(url, body)
        .and_then(|request| request.header("content-type", EVENT_PAYLOAD_MEDIA_TYPE))
        .and_then(|request| request.header("accept", EVENT_STREAM_MEDIA_TYPE))
        .and_then(|request| request.header("authorization", &format!("Bearer {secret}")))
        .map_err(crate::classify_transport_error)?;
    for (name, value) in &config.extra_headers {
        request = request
            .header(name, value)
            .map_err(crate::classify_transport_error)?;
    }
    Ok(request.with_max_response_bytes(config.max_response_bytes))
}

/// Decode one complete `ConverseStream` HTTP response into normalized events.
fn decode_converse_stream(
    response: &heycode_http::HttpResponse,
    client_tools: &BTreeSet<String>,
    provider: &str,
    model: &str,
) -> Vec<Result<InferenceEvent, LlmError>> {
    if !(200..300).contains(&response.status) {
        return vec![Err(crate::classify_transport_error(
            heycode_http::TransportError::http(
                response.status,
                String::from_utf8_lossy(&response.body).into_owned(),
                error_metadata(response),
            ),
        ))];
    }
    // Absence is unknown, not a mismatch: only a content type that is present
    // and wrong is a protocol failure.
    if response
        .content_type
        .as_deref()
        .is_some_and(|media_type| media_type != EVENT_STREAM_MEDIA_TYPE)
    {
        return vec![Err(invalid(
            "Converse response is not an Amazon event stream",
        ))];
    }
    let mut parser = BedrockStreamParser::new(
        response.header("x-amzn-requestid").map(str::to_owned),
        client_tools.clone(),
        provider.to_owned(),
        model.to_owned(),
    );
    let mut decoder = EventStreamDecoder::new();
    let mut output = Vec::new();
    match decoder.feed(&response.body) {
        Ok(messages) => {
            for message in messages {
                let events = parser.message(&message);
                let failed = events.iter().any(Result::is_err);
                output.extend(events);
                if failed {
                    return output;
                }
            }
        }
        Err(error) => {
            output.push(Err(framing_error(&error)));
            return output;
        }
    }
    // Nothing may follow `Finish` (AGENTS §5), so once the terminal metadata
    // event has settled the stream, trailing bytes are past its end.
    if parser.is_terminal() {
        return output;
    }
    if let Err(error) = decoder.finish() {
        output.push(Err(framing_error(&error)));
        return output;
    }
    output.extend(parser.finish());
    output
}

/// Parse the numeric `Retry-After` form. An HTTP-date needs a date parser this
/// crate does not carry, so it stays unknown rather than becoming a guess.
fn error_metadata(response: &heycode_http::HttpResponse) -> heycode_http::HttpErrorMetadata {
    let retry_after = response
        .header("retry-after")
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| {
            heycode_http::HttpRetryAfter::Delay(std::time::Duration::from_secs(seconds))
        });
    heycode_http::HttpErrorMetadata::new(retry_after, None)
}

fn framing_error(error: &EventStreamError) -> LlmError {
    LlmError::InvalidResponse(format!("Converse event stream framing failed: {error}"))
}

// ---------------------------------------------------------------------------
// Request draft validation and body construction
// ---------------------------------------------------------------------------

const BEDROCK_RUNTIME_METADATA_OPTION_KIND: &str = "runtime-metadata";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BedrockCachePlacementWire {
    Tools,
    System,
    LatestUserMessage,
}

#[derive(Debug, Clone)]
struct BedrockRuntimeMetadataWire {
    cache_points: Vec<(BedrockCachePlacementWire, serde_json::Value)>,
    guardrail: Option<serde_json::Value>,
}

fn decode_runtime_metadata(
    base_url: &str,
    model: &str,
    options: &[heycode_core::ProviderRequestOption],
) -> Result<Option<BedrockRuntimeMetadataWire>, String> {
    if options.is_empty() {
        return Ok(None);
    }
    if options.len() != 1 {
        return Err("Converse accepts exactly one runtime-metadata provider option".to_owned());
    }
    let option = &options[0];
    option
        .validate()
        .map_err(|_| "Converse runtime metadata option is invalid".to_owned())?;
    if option.kind() != BEDROCK_RUNTIME_METADATA_OPTION_KIND {
        return Err("Converse provider option kind is not supported".to_owned());
    }
    let data = option
        .data()
        .as_object()
        .ok_or_else(|| "Converse runtime metadata must be an object".to_owned())?;
    if data
        .keys()
        .any(|key| !matches!(key.as_str(), "route" | "cache_points" | "guardrailConfig"))
    {
        return Err("Converse runtime metadata contains an unknown field".to_owned());
    }
    validate_runtime_route(base_url, model, data.get("route"))?;

    let mut cache_points = Vec::new();
    if let Some(points) = data.get("cache_points") {
        let points = points
            .as_array()
            .filter(|points| !points.is_empty() && points.len() <= 3)
            .ok_or_else(|| "Converse cache_points must contain one to three rows".to_owned())?;
        let mut seen = BTreeSet::new();
        let mut saw_default_five_minutes = false;
        for point in points {
            let point = point
                .as_object()
                .filter(|point| point.len() == 2)
                .ok_or_else(|| "Converse cache point row is invalid".to_owned())?;
            let placement = match point.get("placement").and_then(serde_json::Value::as_str) {
                Some("tools") => BedrockCachePlacementWire::Tools,
                Some("system") => BedrockCachePlacementWire::System,
                Some("latest_user_message") => BedrockCachePlacementWire::LatestUserMessage,
                _ => return Err("Converse cache point placement is invalid".to_owned()),
            };
            if !seen.insert(placement) {
                return Err("Converse cache point placements must be unique".to_owned());
            }
            let cache_point = point
                .get("cachePoint")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| "Converse cachePoint must be an object".to_owned())?;
            if cache_point
                .keys()
                .any(|key| !matches!(key.as_str(), "type" | "ttl"))
                || cache_point.get("type").and_then(serde_json::Value::as_str) != Some("default")
                || cache_point
                    .get("ttl")
                    .is_some_and(|ttl| ttl.as_str() != Some("1h"))
            {
                return Err("Converse cachePoint is invalid".to_owned());
            }
            match cache_point.get("ttl") {
                None => saw_default_five_minutes = true,
                Some(_) if saw_default_five_minutes => {
                    return Err(
                        "Converse one-hour cache points must precede five-minute points".to_owned(),
                    );
                }
                Some(_) => {}
            }
            cache_points.push((placement, serde_json::Value::Object(cache_point.clone())));
        }
        if !cache_points
            .windows(2)
            .all(|window| window[0].0 < window[1].0)
        {
            return Err("Converse cache points must follow tools-system-messages order".to_owned());
        }
    }

    let guardrail = data
        .get("guardrailConfig")
        .map(validate_guardrail_config)
        .transpose()?;
    Ok(Some(BedrockRuntimeMetadataWire {
        cache_points,
        guardrail,
    }))
}

fn validate_runtime_route(
    base_url: &str,
    model: &str,
    route: Option<&serde_json::Value>,
) -> Result<(), String> {
    let route = route
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Converse runtime metadata requires route evidence".to_owned())?;
    if route.keys().any(|key| {
        !matches!(
            key.as_str(),
            "source_region" | "target_kind" | "cross_region_scope"
        )
    }) {
        return Err("Converse route evidence contains an unknown field".to_owned());
    }
    let source_region = route
        .get("source_region")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Converse source region is invalid".to_owned())?;
    let endpoint_region = base_url
        .trim_end_matches('/')
        .strip_prefix("https://bedrock-runtime.")
        .and_then(|value| value.strip_suffix(".amazonaws.com"))
        .ok_or_else(|| {
            "Converse runtime metadata requires an official regional endpoint".to_owned()
        })?;
    if source_region != endpoint_region {
        return Err("Converse source region differs from the resolved endpoint".to_owned());
    }
    let (target_kind, scope) = classify_bedrock_target(model);
    if route.get("target_kind").and_then(serde_json::Value::as_str) != Some(target_kind)
        || route
            .get("cross_region_scope")
            .and_then(serde_json::Value::as_str)
            != scope
    {
        return Err("Converse route evidence differs from the selected model target".to_owned());
    }
    Ok(())
}

fn classify_bedrock_target(model: &str) -> (&'static str, Option<&'static str>) {
    let resource = model
        .split_once("inference-profile/")
        .map_or(model, |(_, resource)| resource);
    if resource.starts_with("global.") {
        return ("cross_region_inference_profile", Some("global"));
    }
    if ["us.", "eu.", "apac.", "us-gov."]
        .iter()
        .any(|prefix| resource.starts_with(prefix))
    {
        return ("cross_region_inference_profile", Some("geographic"));
    }
    if model.contains("application-inference-profile/") {
        return ("application_inference_profile", None);
    }
    if model.contains(":foundation-model/") || !model.starts_with("arn:") {
        return ("foundation_model", None);
    }
    ("other", None)
}

fn validate_guardrail_config(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    let guardrail = value
        .as_object()
        .ok_or_else(|| "Converse guardrailConfig must be an object".to_owned())?;
    let identifier = guardrail
        .get("guardrailIdentifier")
        .and_then(serde_json::Value::as_str);
    let version = guardrail
        .get("guardrailVersion")
        .and_then(serde_json::Value::as_str);
    if guardrail.keys().any(|key| {
        !matches!(
            key.as_str(),
            "guardrailIdentifier" | "guardrailVersion" | "trace" | "streamProcessingMode"
        )
    }) || !identifier.is_some_and(valid_bedrock_guardrail_identifier)
        || !version.is_some_and(valid_bedrock_guardrail_version)
        || guardrail.get("trace").is_some_and(|value| {
            !matches!(
                value.as_str(),
                Some("enabled" | "disabled" | "enabled_full")
            )
        })
        || guardrail
            .get("streamProcessingMode")
            .is_some_and(|value| !matches!(value.as_str(), Some("sync" | "async")))
    {
        return Err("Converse guardrailConfig is invalid".to_owned());
    }
    Ok(serde_json::Value::Object(guardrail.clone()))
}

fn valid_bedrock_guardrail_identifier(value: &str) -> bool {
    if value.is_empty() || value.len() > 2_048 || value.chars().any(char::is_control) {
        return false;
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return true;
    }
    let mut fields = value.splitn(6, ':');
    fields.next() == Some("arn")
        && fields.next().is_some_and(|partition| {
            partition == "aws"
                || partition.strip_prefix("aws-").is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        })
                })
        })
        && fields.next() == Some("bedrock")
        && fields.next().is_some_and(|region| {
            (1..=20).contains(&region.len())
                && region
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        && fields.next().is_some_and(|account| {
            account.len() == 12 && account.bytes().all(|byte| byte.is_ascii_digit())
        })
        && fields.next().is_some_and(|resource| {
            resource.strip_prefix("guardrail/").is_some_and(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
        })
}

fn valid_bedrock_guardrail_version(value: &str) -> bool {
    value == "DRAFT"
        || ((1..=8).contains(&value.len())
            && value.as_bytes().first().is_some_and(|byte| *byte != b'0')
            && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn apply_runtime_metadata(
    body: &mut serde_json::Value,
    metadata: BedrockRuntimeMetadataWire,
) -> Result<(), LlmError> {
    for (placement, cache_point) in metadata.cache_points {
        let target = match placement {
            BedrockCachePlacementWire::Tools => body
                .get_mut("toolConfig")
                .and_then(|config| config.get_mut("tools"))
                .and_then(serde_json::Value::as_array_mut),
            BedrockCachePlacementWire::System => body
                .get_mut("system")
                .and_then(serde_json::Value::as_array_mut),
            BedrockCachePlacementWire::LatestUserMessage => body
                .get_mut("messages")
                .and_then(serde_json::Value::as_array_mut)
                .and_then(|messages| {
                    messages.iter_mut().rev().find_map(|message| {
                        (message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
                            .then(|| message.get_mut("content"))
                            .flatten()
                            .and_then(serde_json::Value::as_array_mut)
                    })
                }),
        }
        .ok_or_else(|| invalid("Converse cache point placement has no matching request plane"))?;
        target.push(serde_json::json!({"cachePoint":cache_point}));
    }
    if let Some(guardrail) = metadata.guardrail {
        body["guardrailConfig"] = guardrail;
    }
    Ok(())
}

fn validate_draft(
    config: &BedrockConverseConfig,
    draft: &RequestDraft,
) -> Result<(), ResolveError> {
    let runtime_metadata =
        decode_runtime_metadata(&config.base_url, &draft.model, &draft.provider_options).map_err(
            |message| ResolveError::InvalidRequest {
                field: "provider_options",
                message,
            },
        )?;
    if let Some(metadata) = &runtime_metadata {
        validate_runtime_metadata_planes(metadata, draft).map_err(|message| {
            ResolveError::InvalidRequest {
                field: "provider_options",
                message,
            }
        })?;
    }
    if draft
        .native_tool_routes
        .iter()
        .any(|route| route.kind() == heycode_core::NativeToolImplementationKind::Provider)
    {
        return request_error(
            "native_tool_routes",
            "Converse has no provider-hosted native-tool route dialect; the selection would be silently dropped",
        );
    }
    if !draft.native_features.is_empty() {
        return request_error(
            "native_features",
            "Converse provider-native feature dialects are not configured yet",
        );
    }
    if draft.structured_output.is_some() {
        return request_error(
            "structured_output",
            "Converse structured-output dialect is not configured yet",
        );
    }
    if draft
        .input_modalities
        .iter()
        .any(|modality| *modality != crate::InputModality::Text)
    {
        return request_error(
            "input_modalities",
            "Converse image/document input dialects are not configured yet",
        );
    }
    // `InferenceConfiguration.temperature` is documented as 0..=1; a value
    // outside it is rejected by the service, so it fails before transport.
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_InferenceConfiguration.html
    if draft
        .temperature
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return request_error(
            "temperature",
            "Converse temperature must be between 0 and 1 inclusive",
        );
    }
    if draft
        .system
        .as_deref()
        .is_some_and(|system| system.is_empty())
    {
        return request_error("system", "Converse system prompt text must be non-empty");
    }
    if draft.inputs.is_empty() {
        return request_error("inputs", "Converse requires at least one message");
    }
    // ToolSpecification.name pattern: `[a-zA-Z0-9_-]+`, 1..=64 bytes.
    // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolSpecification.html
    for tool in &draft.tools {
        if tool.name.len() > 64
            || !tool
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return request_error(
                "tools",
                "Converse tool names must be 1..=64 bytes of `[a-zA-Z0-9_-]`",
            );
        }
        if tool.description.is_empty() {
            return request_error("tools", "Converse tool descriptions must be non-empty");
        }
    }
    let advertised = draft
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    for input in &draft.inputs {
        match input {
            InferenceInput::ProviderState(state) => {
                validate_bedrock_state(state, &draft.provider, &draft.model, &advertised)?;
            }
            InferenceInput::Message(message) => validate_message(message, &advertised)?,
        }
    }
    validate_tool_result_chronology(&draft.inputs)
}

fn validate_runtime_metadata_planes(
    metadata: &BedrockRuntimeMetadataWire,
    draft: &RequestDraft,
) -> Result<(), String> {
    for (placement, _) in &metadata.cache_points {
        let available = match placement {
            BedrockCachePlacementWire::Tools => !draft.tools.is_empty(),
            BedrockCachePlacementWire::System => draft.system.is_some(),
            BedrockCachePlacementWire::LatestUserMessage => draft.inputs.iter().any(|input| {
                matches!(
                    input,
                    InferenceInput::Message(message)
                        if matches!(message.role, Role::User | Role::Tool)
                )
            }),
        };
        if !available {
            return Err(
                "Converse cache point placement has no matching request content plane".to_owned(),
            );
        }
    }
    Ok(())
}

fn validate_message(
    message: &ChatMessage,
    advertised: &BTreeSet<&str>,
) -> Result<(), ResolveError> {
    match message.role {
        Role::System => request_error(
            "inputs",
            "Converse system instructions belong in the top-level system slot",
        ),
        Role::User => {
            if message.tool_calls.is_some()
                || message.tool_call_id.is_some()
                || message.tool_result_is_error.is_some()
            {
                return request_error("inputs", "Converse user text has invalid tool metadata");
            }
            if message.content.is_empty() {
                return request_error("inputs", "Converse user text must be non-empty");
            }
            Ok(())
        }
        Role::Assistant => {
            if message.tool_call_id.is_some() || message.tool_result_is_error.is_some() {
                return request_error("inputs", "Converse assistant input cannot be a tool result");
            }
            let calls = message.tool_calls.as_deref().unwrap_or_default();
            if message.content.is_empty() && calls.is_empty() {
                return request_error("inputs", "Converse assistant content must be non-empty");
            }
            let mut ids = BTreeSet::new();
            for call in calls {
                // ToolUseBlock.toolUseId pattern `[a-zA-Z0-9_.:-]+`, 1..=64.
                // https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolUseBlock.html
                if !is_tool_use_id(&call.id)
                    || !ids.insert(call.id.as_str())
                    || !advertised.contains(call.name.as_str())
                    || serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .ok()
                        .is_none_or(|value| !value.is_object())
                {
                    return request_error(
                        "inputs",
                        "Converse assistant tool calls require unique valid ids, advertised names and object JSON input",
                    );
                }
            }
            Ok(())
        }
        Role::Tool => {
            if message.tool_calls.is_some() {
                return request_error("inputs", "Converse tool results carry no tool calls");
            }
            if !message.tool_call_id.as_deref().is_some_and(is_tool_use_id) {
                return request_error(
                    "inputs",
                    "Converse tool results require one valid toolUseId",
                );
            }
            Ok(())
        }
    }
}

fn validate_bedrock_state(
    state: &heycode_core::ProviderStateItem,
    provider: &str,
    model: &str,
    advertised: &BTreeSet<&str>,
) -> Result<(), ResolveError> {
    if state.validate().is_err()
        || state.provider() != provider
        || state.model() != model
        || state.protocol() != ProviderProtocol::BedrockConverse
        || state.kind() != heycode_core::ProviderStateKind::BedrockConverseMessage
    {
        return request_error(
            "provider_state",
            "Converse provider state differs from the selected route",
        );
    }
    let content =
        state.data()["content"]
            .as_array()
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_state",
                message: "Converse provider state content is invalid".to_owned(),
            })?;
    let mut ids = BTreeSet::new();
    for block in content {
        let Some(tool) = block.get("toolUse").and_then(serde_json::Value::as_object) else {
            continue;
        };
        let id = tool
            .get("toolUseId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let name = tool
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !is_tool_use_id(id)
            || !ids.insert(id)
            || !advertised.contains(name)
            || tool.get("input").is_none_or(|input| !input.is_object())
        {
            return request_error(
                "provider_state",
                "Converse provider-state tools require unique valid ids and advertised names",
            );
        }
    }
    Ok(())
}

fn is_tool_use_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
}

/// Converse requires every assistant `toolUse` block to be answered by
/// `toolResult` blocks in the immediately following user turn.
fn validate_tool_result_chronology(inputs: &[InferenceInput]) -> Result<(), ResolveError> {
    let mut pending = BTreeSet::<String>::new();
    for input in inputs {
        if !pending.is_empty() {
            let InferenceInput::Message(message) = input else {
                return request_error(
                    "inputs",
                    "Converse tool calls must be followed immediately by their tool results",
                );
            };
            if message.role != Role::Tool {
                return request_error(
                    "inputs",
                    "Converse tool calls must be followed immediately by their tool results",
                );
            }
            let id = message.tool_call_id.as_deref().unwrap_or_default();
            if !pending.remove(id) {
                return request_error(
                    "inputs",
                    "Converse tool result does not match a pending tool call",
                );
            }
            continue;
        }
        match input {
            InferenceInput::ProviderState(state) => {
                if let Some(content) = state.data()["content"].as_array() {
                    pending.extend(content.iter().filter_map(|block| {
                        block["toolUse"]["toolUseId"].as_str().map(str::to_owned)
                    }));
                }
            }
            InferenceInput::Message(message) => match message.role {
                Role::Tool => {
                    return request_error(
                        "inputs",
                        "Converse tool result has no immediately preceding tool call",
                    );
                }
                Role::Assistant => {
                    if let Some(calls) = &message.tool_calls {
                        pending.extend(calls.iter().map(|call| call.id.as_str().to_owned()));
                    }
                }
                Role::System | Role::User => {}
            },
        }
    }
    if pending.is_empty() {
        Ok(())
    } else {
        request_error(
            "inputs",
            "Converse tool calls are missing their immediate tool results",
        )
    }
}

fn request_error<T>(field: &'static str, message: impl Into<String>) -> Result<T, ResolveError> {
    Err(ResolveError::InvalidRequest {
        field,
        message: message.into(),
    })
}

/// Build the `ConverseStream` request body.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html>
fn converse_stream_body(
    config: &BedrockConverseConfig,
    call: &ResolvedCall,
) -> Result<serde_json::Value, LlmError> {
    if call.protocol() != ProviderProtocol::BedrockConverse {
        return Err(invalid("resolved call protocol is not Bedrock Converse"));
    }
    let mut messages = Vec::new();
    let mut pending_results = Vec::new();
    for input in call.inputs() {
        match input {
            InferenceInput::ProviderState(state) => {
                flush_tool_results(&mut messages, &mut pending_results);
                messages.push(state.data().clone());
            }
            InferenceInput::Message(message) if message.role == Role::Tool => {
                pending_results.push(tool_result_block(message)?);
            }
            InferenceInput::Message(message) => {
                flush_tool_results(&mut messages, &mut pending_results);
                messages.push(converse_message(message)?);
            }
        }
    }
    flush_tool_results(&mut messages, &mut pending_results);

    let mut body = serde_json::json!({ "messages": messages });
    if let Some(system) = call.system() {
        body["system"] = serde_json::json!([{ "text": system }]);
    }
    let mut inference_config = serde_json::Map::new();
    if let Some(max_tokens) = call.max_output_tokens() {
        inference_config.insert("maxTokens".to_owned(), serde_json::json!(max_tokens));
    }
    if let Some(temperature) = call.temperature() {
        inference_config.insert("temperature".to_owned(), serde_json::json!(temperature));
    }
    if !inference_config.is_empty() {
        body["inferenceConfig"] = serde_json::Value::Object(inference_config);
    }
    if !call.tools().is_empty() {
        body["toolConfig"] = serde_json::json!({
            "tools": call.tools().iter().map(converse_tool).collect::<Vec<_>>(),
            "toolChoice": {"auto": {}},
        });
    }
    if let Some(metadata) =
        decode_runtime_metadata(&config.base_url, call.model(), call.provider_options())
            .map_err(invalid)?
    {
        apply_runtime_metadata(&mut body, metadata)?;
    }
    Ok(body)
}

/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Tool.html>
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolSpecification.html>
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolInputSchema.html>
fn converse_tool(tool: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "toolSpec": {
            "name": tool.name,
            "description": tool.description,
            "inputSchema": {"json": tool.parameters},
        }
    })
}

/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Message.html>
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ContentBlock.html>
fn converse_message(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    match message.role {
        Role::System => Err(invalid(
            "Converse system input appeared outside the top-level system slot",
        )),
        Role::Tool => Ok(serde_json::json!({
            "role": "user",
            "content": [tool_result_block(message)?],
        })),
        Role::User => Ok(serde_json::json!({
            "role": "user",
            "content": [{"text": message.content}],
        })),
        Role::Assistant => {
            let mut content = Vec::new();
            if !message.content.is_empty() {
                content.push(serde_json::json!({"text": message.content}));
            }
            for call in message.tool_calls.as_deref().unwrap_or_default() {
                let input: serde_json::Value = serde_json::from_str(&call.arguments)
                    .map_err(|_| invalid("Converse assistant tool input is not JSON"))?;
                if !input.is_object() {
                    return Err(invalid(
                        "Converse assistant tool input must be a JSON object",
                    ));
                }
                content.push(serde_json::json!({
                    "toolUse": {
                        "toolUseId": call.id,
                        "name": call.name,
                        "input": input,
                    }
                }));
            }
            Ok(serde_json::json!({"role": "assistant", "content": content}))
        }
    }
}

/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolResultBlock.html>
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolResultContentBlock.html>
fn tool_result_block(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    let tool_use_id = message
        .tool_call_id
        .as_ref()
        .ok_or_else(|| invalid("Converse tool result has no toolUseId"))?;
    let mut block = serde_json::json!({
        "toolResult": {
            "toolUseId": tool_use_id,
            "content": [{"text": message.content}],
        }
    });
    // `status` is documented as supported only by some model families, so the
    // default `success` is left implicit and only a real failure is stated.
    if message.tool_result_is_error == Some(true) {
        block["toolResult"]["status"] = serde_json::json!("error");
    }
    Ok(block)
}

fn flush_tool_results(messages: &mut Vec<serde_json::Value>, pending: &mut Vec<serde_json::Value>) {
    if !pending.is_empty() {
        messages.push(serde_json::json!({
            "role": "user",
            "content": std::mem::take(pending),
        }));
    }
}

// ---------------------------------------------------------------------------
// Event semantics
// ---------------------------------------------------------------------------

struct ActiveToolCall {
    id: String,
    name: String,
    arguments: String,
    announced: bool,
}

struct ActiveBlock {
    index: u32,
    item_id: String,
    kind: StreamItemKind,
    tool: Option<ActiveToolCall>,
    state: serde_json::Map<String, serde_json::Value>,
}

/// Exact `metadata.usage` counters.
///
/// `inputTokens`, `outputTokens` and `totalTokens` are `Required: Yes`; the two
/// cache counters are `Required: No`, so their absence is UNKNOWN and is never
/// reported as a zero.
///
/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_TokenUsage.html>
struct BedrockUsage {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    cache_details: Option<Vec<BedrockCacheDetail>>,
}

struct BedrockCacheDetail {
    input_tokens: u64,
    ttl: BedrockCacheTtlWire,
}

enum BedrockCacheTtlWire {
    FiveMinutes,
    OneHour,
}

impl BedrockUsage {
    /// "When prompt caching is enabled, the `inputTokens` field represents only
    /// the non-cached input tokens ... total input tokens = inputTokens +
    /// cacheReadInputTokens + cacheWriteInputTokens".
    ///
    /// <https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html>
    ///
    /// A counter the service did not report contributes nothing, because the
    /// service reports these only when caching took part in the request. It is
    /// never turned into a claim that zero tokens were read or written.
    fn normalized(&self) -> Result<TokenUsage, LlmError> {
        let prompt_tokens = [
            Some(self.input_tokens),
            self.cache_read_input_tokens,
            self.cache_write_input_tokens,
        ]
        .into_iter()
        .flatten()
        .try_fold(0_u64, |total, component| total.checked_add(component))
        .ok_or_else(|| invalid("Converse prompt usage overflowed u64"))?;
        Ok(TokenUsage {
            prompt_tokens,
            completion_tokens: self.output_tokens,
        })
    }

    fn response_metadata(
        &self,
    ) -> Result<Option<heycode_core::ProviderResponseMetadata>, LlmError> {
        if self.cache_read_input_tokens.is_none()
            && self.cache_write_input_tokens.is_none()
            && self.cache_details.is_none()
        {
            return Ok(None);
        }
        let (Some(cache_read), Some(cache_write)) =
            (self.cache_read_input_tokens, self.cache_write_input_tokens)
        else {
            // The neutral cache vocabulary requires a complete read/write
            // pair. Preserve partial provider evidence in ordinary aggregate
            // usage without turning an absent component into zero.
            return Ok(None);
        };
        let total_input = self
            .input_tokens
            .checked_add(cache_read)
            .and_then(|total| total.checked_add(cache_write))
            .ok_or_else(|| invalid("Converse detailed cache usage overflowed u64"))?;
        let mut cache = heycode_core::ProviderCacheUsage::new(
            total_input,
            self.output_tokens,
            cache_read,
            cache_write,
        )
        .and_then(|cache| cache.with_uncached_input_tokens(self.input_tokens))
        .map_err(|_| invalid("Converse detailed cache usage is inconsistent"))?;
        if let Some(details) = &self.cache_details {
            let mut five_minutes = 0_u64;
            let mut one_hour = 0_u64;
            for detail in details {
                let target = match detail.ttl {
                    BedrockCacheTtlWire::FiveMinutes => &mut five_minutes,
                    BedrockCacheTtlWire::OneHour => &mut one_hour,
                };
                *target = target
                    .checked_add(detail.input_tokens)
                    .ok_or_else(|| invalid("Converse cacheDetails overflowed u64"))?;
            }
            cache = cache
                .with_cache_write_ttl_tokens(five_minutes, one_hour)
                .map_err(|_| invalid("Converse cacheDetails disagree with cache writes"))?;
        }
        heycode_core::ProviderResponseMetadata::new(Some(cache), Vec::new(), None)
            .map(Some)
            .map_err(|_| invalid("Converse response metadata is invalid"))
    }
}

/// Interprets decoded event-stream messages as `ConverseStream` events.
struct BedrockStreamParser {
    response_id: Option<String>,
    provider: String,
    model: String,
    client_tools: BTreeSet<String>,
    tool_ids: BTreeSet<String>,
    started: bool,
    active: Option<ActiveBlock>,
    last_closed_index: Option<u32>,
    tool_call_count: usize,
    stop_reason: Option<String>,
    state_content: Vec<serde_json::Value>,
    terminal: bool,
}

impl BedrockStreamParser {
    fn new(
        response_id: Option<String>,
        client_tools: BTreeSet<String>,
        provider: String,
        model: String,
    ) -> Self {
        Self {
            response_id,
            provider,
            model,
            client_tools,
            tool_ids: BTreeSet::new(),
            started: false,
            active: None,
            last_closed_index: None,
            tool_call_count: 0,
            stop_reason: None,
            state_content: Vec::new(),
            terminal: false,
        }
    }

    fn message(&mut self, message: &EventStreamMessage) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        match self.parse_message(message) {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => {
                self.terminal = true;
                vec![Err(error)]
            }
        }
    }

    /// Dispatch on the documented semantic headers.
    ///
    /// <https://smithy.io/2.0/aws/amazon-eventstream.html#amazon-event-semantics>
    fn parse_message(
        &mut self,
        message: &EventStreamMessage,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let message_type = message
            .header(":message-type")
            .ok_or_else(|| invalid("Converse event stream message has no `:message-type`"))?;
        match message_type {
            "event" => {
                let event_type = message
                    .header(":event-type")
                    .ok_or_else(|| invalid("Converse event has no `:event-type`"))?;
                if message
                    .header(":content-type")
                    .is_some_and(|media_type| media_type != EVENT_PAYLOAD_MEDIA_TYPE)
                {
                    return Err(invalid("Converse event payload is not JSON"));
                }
                let payload = event_payload(&message.payload)?;
                self.event(event_type, &payload)
            }
            // Modeled errors name the union member in `:exception-type`. The
            // payload is provider text and never reaches the error.
            "exception" => {
                self.terminal = true;
                let exception = message
                    .header(":exception-type")
                    .ok_or_else(|| invalid("Converse exception has no `:exception-type`"))?;
                Err(crate::retry::provider_event_error(
                    exception_class(exception),
                    Some(exception),
                ))
            }
            // Unmodeled errors carry `:error-code` and a human-readable
            // `:error-message`; only the code is safe to retain.
            "error" => {
                self.terminal = true;
                let code = message
                    .header(":error-code")
                    .ok_or_else(|| invalid("Converse error has no `:error-code`"))?;
                Err(crate::retry::provider_event_error(
                    crate::ProviderErrorClass::Server,
                    Some(code),
                ))
            }
            _ => Err(invalid("Converse event stream `:message-type` is unknown")),
        }
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStreamOutput.html>
    fn event(
        &mut self,
        event_type: &str,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        match event_type {
            "messageStart" => self.message_start(payload),
            "contentBlockStart" => self.block_start(payload),
            "contentBlockDelta" => self.block_delta(payload),
            "contentBlockStop" => self.block_stop(payload),
            "messageStop" => self.message_stop(payload),
            "metadata" => self.metadata(payload),
            _ => Err(invalid("Converse stream used an unknown `:event-type`")),
        }
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_MessageStartEvent.html>
    fn message_start(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if self.started {
            return Err(invalid("Converse stream started more than one message"));
        }
        if required_str(payload, "role", "Converse messageStart")? != "assistant" {
            return Err(invalid("Converse messageStart role is not assistant"));
        }
        self.started = true;
        Ok(self
            .response_id
            .clone()
            .map(|response_id| InferenceEvent::ResponseStarted { response_id })
            .into_iter()
            .collect())
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ContentBlockStartEvent.html>
    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ToolUseBlockStart.html>
    fn block_start(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let index = self.next_index(payload, "Converse contentBlockStart")?;
        let start = required_object(payload, "start", "Converse contentBlockStart")?;
        let Some(tool_use) = start.get("toolUse") else {
            return Err(invalid(
                "Converse content block start dialect is limited to client tool use",
            ));
        };
        let tool_use = as_object(tool_use, "Converse toolUse start")?;
        // `type` is documented with the single valid value `server_tool_use`;
        // this route never advertises server tools.
        if tool_use.get("type").is_some_and(|value| !value.is_null()) {
            return Err(invalid(
                "Converse response started an unadvertised server tool",
            ));
        }
        let id = required_str(tool_use, "toolUseId", "Converse toolUse start")?.to_owned();
        let name = required_str(tool_use, "name", "Converse toolUse start")?.to_owned();
        if !is_tool_use_id(&id) {
            return Err(invalid("Converse toolUseId is malformed"));
        }
        if !self.client_tools.contains(&name) {
            return Err(invalid(
                "Converse response requested an unadvertised client tool",
            ));
        }
        if !self.tool_ids.insert(id.clone()) {
            return Err(invalid("Converse toolUseId was used twice"));
        }
        self.tool_call_count = self.tool_call_count.saturating_add(1);
        self.active = Some(ActiveBlock {
            index,
            item_id: id.clone(),
            kind: StreamItemKind::FunctionCall,
            tool: Some(ActiveToolCall {
                id,
                name,
                arguments: String::new(),
                announced: false,
            }),
            state: serde_json::Map::new(),
        });
        Ok(self.started_event())
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ContentBlockDeltaEvent.html>
    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ContentBlockDelta.html>
    fn block_delta(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let delta = required_object(payload, "delta", "Converse contentBlockDelta")?;
        let mut output = Vec::new();
        if self.active.is_none() {
            // Text and reasoning blocks arrive without a `contentBlockStart`,
            // so the first delta opens them.
            let index = self.next_index(payload, "Converse contentBlockDelta")?;
            let kind = if delta.contains_key("text") {
                StreamItemKind::Message
            } else if delta.contains_key("reasoningContent") {
                StreamItemKind::Reasoning
            } else {
                return Err(invalid(
                    "Converse content block delta has no supported member",
                ));
            };
            self.active = Some(ActiveBlock {
                index,
                item_id: format!("content/{index}"),
                kind,
                tool: None,
                state: serde_json::Map::new(),
            });
            output.extend(self.started_event());
        } else {
            let index = required_u32(payload, "contentBlockIndex", "Converse contentBlockDelta")?;
            if self.active.as_ref().map(|block| block.index) != Some(index) {
                return Err(invalid(
                    "Converse content delta index differs from its active block",
                ));
            }
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| invalid("Converse content delta has no active block"))?;
        let index = active.index;
        if let Some(text) = delta.get("text") {
            if active.kind != StreamItemKind::Message {
                return Err(invalid("Converse text delta arrived on a non-text block"));
            }
            let text = text
                .as_str()
                .ok_or_else(|| invalid("Converse text delta must be a string"))?;
            append_state_text(&mut active.state, "text", text)?;
            output.push(InferenceEvent::TextDelta(text.to_owned()));
        } else if let Some(reasoning) = delta.get("reasoningContent") {
            if active.kind != StreamItemKind::Reasoning {
                return Err(invalid(
                    "Converse reasoning delta arrived on a non-reasoning block",
                ));
            }
            let reasoning = as_object(reasoning, "Converse reasoningContent delta")?;
            if reasoning.len() != 1
                || reasoning
                    .keys()
                    .any(|key| !matches!(key.as_str(), "text" | "signature" | "redactedContent"))
            {
                return Err(invalid(
                    "Converse reasoningContent delta has an unknown member",
                ));
            }
            let (kind, value) = reasoning
                .iter()
                .next()
                .ok_or_else(|| invalid("Converse reasoningContent delta is empty"))?;
            let value = value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("Converse reasoning state must be a nonempty string"))?;
            append_state_text(&mut active.state, kind, value)?;
            if kind == "text" {
                let text = value;
                output.push(InferenceEvent::ReasoningDelta(text.to_owned()));
            }
        } else if let Some(tool_use) = delta.get("toolUse") {
            let tool = active
                .tool
                .as_mut()
                .ok_or_else(|| invalid("Converse toolUse delta arrived on a non-tool block"))?;
            let tool_use = as_object(tool_use, "Converse toolUse delta")?;
            let input = required_str(tool_use, "input", "Converse toolUse delta")?;
            tool.arguments.push_str(input);
            let first = !tool.announced;
            tool.announced = true;
            output.push(InferenceEvent::ToolCallDelta {
                output_index: index,
                id: first.then(|| heycode_core::CallId::from_raw(&tool.id)),
                name: first.then(|| tool.name.clone()),
                arguments_delta: input.to_owned(),
            });
        } else {
            return Err(invalid(
                "Converse content block delta dialect is limited to text, reasoning and tool use",
            ));
        }
        Ok(output)
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ContentBlockStopEvent.html>
    fn block_stop(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let index = required_u32(payload, "contentBlockIndex", "Converse contentBlockStop")?;
        let block = self
            .active
            .take()
            .ok_or_else(|| invalid("Converse content block stopped without a start"))?;
        if block.index != index {
            return Err(invalid(
                "Converse content stop index differs from its active block",
            ));
        }
        let mut output = Vec::new();
        let state_block = if let Some(tool) = block.tool {
            let input = if tool.arguments.trim().is_empty() {
                // Delta concatenation must equal the final arguments, so a tool
                // call with no input deltas still receives its empty object.
                output.push(InferenceEvent::ToolCallDelta {
                    output_index: index,
                    id: (!tool.announced).then(|| heycode_core::CallId::from_raw(&tool.id)),
                    name: (!tool.announced).then(|| tool.name.clone()),
                    arguments_delta: "{}".to_owned(),
                });
                serde_json::json!({})
            } else {
                let input: serde_json::Value = serde_json::from_str(&tool.arguments)
                    .map_err(|_| invalid("Converse tool input deltas did not form JSON"))?;
                if !input.is_object() {
                    return Err(invalid(
                        "Converse tool input deltas must form a JSON object",
                    ));
                }
                input
            };
            serde_json::json!({
                "toolUse":{
                    "toolUseId":tool.id,
                    "name":tool.name,
                    "input":input
                }
            })
        } else {
            match block.kind {
                StreamItemKind::Message => {
                    let text = block
                        .state
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .filter(|text| !text.is_empty())
                        .ok_or_else(|| invalid("Converse text block is empty"))?;
                    serde_json::json!({"text":text})
                }
                StreamItemKind::Reasoning if !block.state.is_empty() => {
                    serde_json::json!({"reasoningContent":block.state})
                }
                StreamItemKind::Reasoning => {
                    return Err(invalid("Converse reasoning block is empty"));
                }
                _ => return Err(invalid("Converse content block state is unsupported")),
            }
        };
        self.state_content.push(state_block);
        self.last_closed_index = Some(index);
        output.push(InferenceEvent::ItemFinished {
            output_index: index,
            item_id: block.item_id,
            kind: block.kind,
        });
        Ok(output)
    }

    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_MessageStopEvent.html>
    fn message_stop(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if !self.started || self.active.is_some() {
            return Err(invalid(
                "Converse messageStop requires a started message with no active block",
            ));
        }
        let reason = required_str(payload, "stopReason", "Converse messageStop")?.to_owned();
        if self.stop_reason.replace(reason).is_some() {
            return Err(invalid("Converse stream supplied stopReason twice"));
        }
        Ok(Vec::new())
    }

    /// Usage lands in the terminal `metadata` event, so `Usage` and `Finish`
    /// are emitted here — never from `messageStop`, which carries no counters.
    ///
    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStreamMetadataEvent.html>
    fn metadata(
        &mut self,
        payload: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let stop_reason = self
            .stop_reason
            .clone()
            .ok_or_else(|| invalid("Converse metadata arrived before messageStop"))?;
        let reported = required_object(payload, "usage", "Converse metadata")?;
        let usage = BedrockUsage {
            input_tokens: required_u64(reported, "inputTokens", "Converse usage")?,
            output_tokens: required_u64(reported, "outputTokens", "Converse usage")?,
            cache_read_input_tokens: optional_u64(
                reported,
                "cacheReadInputTokens",
                "Converse usage",
            )?,
            cache_write_input_tokens: optional_u64(
                reported,
                "cacheWriteInputTokens",
                "Converse usage",
            )?,
            cache_details: parse_bedrock_cache_details(reported)?,
        };
        // `totalTokens` is declared `Required: Yes`, so its absence means a
        // malformed usage object; its documented relation to the cache
        // components is not stated precisely enough to cross-check against, so
        // it is required and typed but not otherwise used.
        required_u64(reported, "totalTokens", "Converse usage")?;
        let finish = self.finish_reason(&stop_reason)?;
        let state = if self.state_content.is_empty() {
            None
        } else {
            Some(
                heycode_core::ProviderStateItem::new(
                    &self.provider,
                    &self.model,
                    ProviderProtocol::BedrockConverse,
                    heycode_core::ProviderStateKind::BedrockConverseMessage,
                    serde_json::json!({
                        "role":"assistant",
                        "content":std::mem::take(&mut self.state_content)
                    }),
                )
                .map_err(|_| invalid("Converse provider state is invalid"))?,
            )
        };
        self.terminal = true;
        let mut output = Vec::new();
        if let Some(response_id) = self.response_id.clone() {
            output.push(InferenceEvent::ResponseFinished {
                response_id,
                status: stop_reason,
            });
        }
        if let Some(state) = state {
            output.push(InferenceEvent::ProviderState(state));
        }
        if let Some(metadata) = usage.response_metadata()? {
            output.push(InferenceEvent::ResponseMetadata(metadata));
        }
        output.push(InferenceEvent::Usage(usage.normalized()?));
        output.push(InferenceEvent::Finish(finish));
        Ok(output)
    }

    /// `stopReason` is a closed enum; an unknown member fails loud instead of
    /// degrading into a synthetic `Stop`.
    ///
    /// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_MessageStopEvent.html>
    fn finish_reason(&self, stop_reason: &str) -> Result<FinishReason, LlmError> {
        match stop_reason {
            "end_turn" | "stop_sequence" => Ok(FinishReason::Stop),
            "tool_use" if self.tool_call_count > 0 => Ok(FinishReason::ToolCalls),
            "tool_use" => Err(invalid("Converse tool_use stop has no toolUse block")),
            "max_tokens" | "model_context_window_exceeded" => Ok(FinishReason::Length),
            "guardrail_intervened" | "content_filtered" => Err(crate::retry::provider_event_error(
                crate::ProviderErrorClass::InvalidRequest,
                Some(stop_reason),
            )),
            "malformed_model_output" | "malformed_tool_use" => {
                Err(crate::retry::provider_event_error(
                    crate::ProviderErrorClass::Server,
                    Some(stop_reason),
                ))
            }
            _ => Err(invalid("Converse response used an unknown stopReason")),
        }
    }

    fn started_event(&self) -> Vec<InferenceEvent> {
        self.active
            .as_ref()
            .map(|block| InferenceEvent::ItemStarted {
                output_index: block.index,
                item_id: block.item_id.clone(),
                kind: block.kind.clone(),
            })
            .into_iter()
            .collect()
    }

    /// Content block indexes open one at a time and never revisit a closed
    /// index. Contiguity is not documented, so it is not enforced.
    fn next_index(
        &self,
        payload: &serde_json::Map<String, serde_json::Value>,
        context: &str,
    ) -> Result<u32, LlmError> {
        let index = required_u32(payload, "contentBlockIndex", context)?;
        if self.active.is_some() {
            return Err(invalid("Converse content blocks must not overlap"));
        }
        if self.last_closed_index.is_some_and(|closed| index <= closed) {
            return Err(invalid(
                "Converse content block indexes must strictly increase",
            ));
        }
        if !self.started {
            return Err(invalid("Converse content block began before messageStart"));
        }
        Ok(index)
    }

    const fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn finish(self) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            Vec::new()
        } else {
            vec![Err(invalid(
                "Converse event stream ended before its terminal metadata usage",
            ))]
        }
    }
}

fn append_state_text(
    state: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    fragment: &str,
) -> Result<(), LlmError> {
    match state.entry(key.to_owned()) {
        serde_json::map::Entry::Vacant(entry) => {
            entry.insert(serde_json::Value::String(fragment.to_owned()));
        }
        serde_json::map::Entry::Occupied(mut entry) => {
            let serde_json::Value::String(value) = entry.get_mut() else {
                return Err(invalid("Converse provider state is not textual"));
            };
            value.push_str(fragment);
        }
    }
    Ok(())
}

/// <https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStreamOutput.html>
fn exception_class(exception: &str) -> crate::ProviderErrorClass {
    match exception {
        "throttlingException" => crate::ProviderErrorClass::RateLimited,
        "serviceUnavailableException" => crate::ProviderErrorClass::Overloaded,
        "validationException" => crate::ProviderErrorClass::InvalidRequest,
        _ => crate::ProviderErrorClass::Server,
    }
}

fn event_payload(payload: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, LlmError> {
    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("Converse event payload is not JSON: {error}")))?;
    match value {
        serde_json::Value::Object(object) => Ok(object),
        _ => Err(invalid("Converse event payload must be a JSON object")),
    }
}

// ---------------------------------------------------------------------------
// Shared JSON accessors
// ---------------------------------------------------------------------------

fn as_object<'a>(
    value: &'a serde_json::Value,
    context: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn required_object<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    object
        .get(field)
        .ok_or_else(|| invalid(format!("{context} is missing `{field}`")))
        .and_then(|value| as_object(value, context))
}

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, LlmError> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a string")))?;
    if value.is_empty() {
        Err(invalid(format!("{context} `{field}` must be non-empty")))
    } else {
        Ok(value)
    }
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u64, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a u64")))
}

fn parse_bedrock_cache_details(
    usage: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<Vec<BedrockCacheDetail>>, LlmError> {
    let Some(details) = usage.get("cacheDetails").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let details = details
        .as_array()
        .filter(|details| details.len() <= 16)
        .ok_or_else(|| invalid("Converse cacheDetails must be a bounded array"))?;
    let mut parsed = Vec::with_capacity(details.len());
    for detail in details {
        let detail = as_object(detail, "Converse cacheDetails row")?;
        if detail
            .keys()
            .any(|key| !matches!(key.as_str(), "inputTokens" | "ttl"))
        {
            return Err(invalid("Converse cacheDetails row has an unknown field"));
        }
        let ttl = match required_str(detail, "ttl", "Converse cacheDetails row")? {
            "5m" => BedrockCacheTtlWire::FiveMinutes,
            "1h" => BedrockCacheTtlWire::OneHour,
            _ => return Err(invalid("Converse cacheDetails row has an unknown ttl")),
        };
        parsed.push(BedrockCacheDetail {
            input_tokens: required_u64(detail, "inputTokens", "Converse cacheDetails row")?,
            ttl,
        });
    }
    Ok(Some(parsed))
}

fn optional_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<u64>, LlmError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{context} `{field}` must be a u64"))),
    }
}

fn required_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u32, LlmError> {
    u32::try_from(required_u64(object, field, context)?)
        .map_err(|_| invalid(format!("{context} `{field}` must be a u32")))
}

fn invalid(message: impl Into<String>) -> LlmError {
    LlmError::InvalidResponse(message.into())
}

fn one_error(error: LlmError) -> InferenceStream {
    Box::pin(futures::stream::once(async move { Err(error) }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod framing_tests {
    use super::*;

    /// Encode one message exactly as the specification describes, so the
    /// decoder is tested against an independent encoder rather than itself.
    fn encode(headers: &[(&str, HeaderValue<'_>)], payload: &[u8]) -> Vec<u8> {
        let mut header_bytes = Vec::new();
        for (name, value) in headers {
            header_bytes.push(u8::try_from(name.len()).unwrap());
            header_bytes.extend_from_slice(name.as_bytes());
            value.encode(&mut header_bytes);
        }
        let total =
            u32::try_from(MESSAGE_OVERHEAD_BYTES + header_bytes.len() + payload.len()).unwrap();
        let headers_length = u32::try_from(header_bytes.len()).unwrap();
        let mut message = Vec::new();
        message.extend_from_slice(&total.to_be_bytes());
        message.extend_from_slice(&headers_length.to_be_bytes());
        message.extend_from_slice(&crc32(&message).to_be_bytes());
        message.extend_from_slice(&header_bytes);
        message.extend_from_slice(payload);
        let message_crc = crc32(&message);
        message.extend_from_slice(&message_crc.to_be_bytes());
        message
    }

    enum HeaderValue<'a> {
        Text(&'a str),
        Long(i64),
        Bytes(&'a [u8]),
        Raw(u8, Vec<u8>),
    }

    impl HeaderValue<'_> {
        fn encode(&self, out: &mut Vec<u8>) {
            match self {
                Self::Text(value) => {
                    out.push(7);
                    out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
                    out.extend_from_slice(value.as_bytes());
                }
                Self::Long(value) => {
                    out.push(5);
                    out.extend_from_slice(&value.to_be_bytes());
                }
                Self::Bytes(value) => {
                    out.push(6);
                    out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
                    out.extend_from_slice(value);
                }
                Self::Raw(indicator, value) => {
                    out.push(*indicator);
                    out.extend_from_slice(value);
                }
            }
        }
    }

    fn text_message(event_type: &str, payload: &str) -> Vec<u8> {
        encode(
            &[
                (":message-type", HeaderValue::Text("event")),
                (":event-type", HeaderValue::Text(event_type)),
                (":content-type", HeaderValue::Text("application/json")),
            ],
            payload.as_bytes(),
        )
    }

    fn decode_all(chunks: &[&[u8]]) -> Result<Vec<EventStreamMessage>, EventStreamError> {
        let mut decoder = EventStreamDecoder::new();
        let mut messages = Vec::new();
        for chunk in chunks {
            messages.extend(decoder.feed(chunk)?);
        }
        decoder.finish()?;
        Ok(messages)
    }

    #[test]
    fn crc32_matches_the_published_check_value() {
        // The IEEE 802.3 / gzip CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn decodes_headers_and_payload_of_one_whole_message() {
        let wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        let messages = decode_all(&[&wire]).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].header(":message-type"), Some("event"));
        assert_eq!(messages[0].header(":event-type"), Some("messageStart"));
        assert_eq!(messages[0].payload, br#"{"role":"assistant"}"#.to_vec());
    }

    #[test]
    fn frame_split_across_reads_decodes_identically_at_every_byte_boundary() {
        let mut wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        wire.extend_from_slice(&text_message(
            "contentBlockStop",
            r#"{"contentBlockIndex":0}"#,
        ));
        let expected = decode_all(&[&wire]).unwrap();
        for split in 0..=wire.len() {
            let (head, tail) = wire.split_at(split);
            assert_eq!(
                decode_all(&[head, tail]).unwrap(),
                expected,
                "split at byte {split}"
            );
        }
    }

    #[test]
    fn frame_split_bytewise_decodes_identically() {
        let wire = text_message("messageStop", r#"{"stopReason":"end_turn"}"#);
        let chunks = wire.iter().map(std::slice::from_ref).collect::<Vec<_>>();
        assert_eq!(decode_all(&chunks).unwrap(), decode_all(&[&wire]).unwrap());
    }

    #[test]
    fn truncated_frame_at_end_of_stream_fails() {
        let wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        let partial = &wire[..wire.len() - 1];
        assert_eq!(
            decode_all(&[partial]).unwrap_err(),
            EventStreamError::Truncated {
                pending_bytes: partial.len()
            }
        );
    }

    #[test]
    fn partial_prelude_at_end_of_stream_fails() {
        let wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        assert_eq!(
            decode_all(&[&wire[..5]]).unwrap_err(),
            EventStreamError::Truncated { pending_bytes: 5 }
        );
    }

    #[test]
    fn corrupted_prelude_length_fails_the_prelude_checksum() {
        let mut wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        wire[3] = wire[3].wrapping_add(1);
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::PreludeChecksum
        );
    }

    #[test]
    fn corrupted_payload_fails_the_message_checksum() {
        let mut wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        let last_payload_byte = wire.len() - 5;
        wire[last_payload_byte] = wire[last_payload_byte].wrapping_add(1);
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::MessageChecksum
        );
    }

    #[test]
    fn declared_length_shorter_than_the_framing_overhead_fails() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&15_u32.to_be_bytes());
        wire.extend_from_slice(&0_u32.to_be_bytes());
        wire.extend_from_slice(&crc32(&wire).to_be_bytes());
        wire.extend_from_slice(&[0; 8]);
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::InvalidLength {
                total_length: 15,
                headers_length: 0
            }
        );
    }

    #[test]
    fn headers_longer_than_the_declared_message_fail() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&32_u32.to_be_bytes());
        wire.extend_from_slice(&64_u32.to_be_bytes());
        wire.extend_from_slice(&crc32(&wire).to_be_bytes());
        wire.extend_from_slice(&[0; 20]);
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::InvalidLength {
                total_length: 32,
                headers_length: 64
            }
        );
    }

    #[test]
    fn declared_message_beyond_the_hard_cap_fails_without_buffering_it() {
        let total_length = u32::try_from(MAX_MESSAGE_BYTES).unwrap() + 1;
        let mut wire = Vec::new();
        wire.extend_from_slice(&total_length.to_be_bytes());
        wire.extend_from_slice(&0_u32.to_be_bytes());
        wire.extend_from_slice(&crc32(&wire).to_be_bytes());
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::MessageTooLarge { total_length }
        );
    }

    #[test]
    fn declared_headers_beyond_the_documented_maximum_fail() {
        let headers_length = u32::try_from(MAX_HEADER_BYTES).unwrap() + 1;
        let total_length = headers_length + u32::try_from(MESSAGE_OVERHEAD_BYTES).unwrap();
        let mut wire = Vec::new();
        wire.extend_from_slice(&total_length.to_be_bytes());
        wire.extend_from_slice(&headers_length.to_be_bytes());
        wire.extend_from_slice(&crc32(&wire).to_be_bytes());
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::HeadersTooLarge { headers_length }
        );
    }

    #[test]
    fn zero_length_header_name_fails() {
        let mut wire = encode(&[(":x", HeaderValue::Text("y"))], b"{}");
        // Blank the one-byte header-name length and repair the message CRC so
        // the decoder reaches header parsing rather than the checksum.
        wire[PRELUDE_BYTES] = 0;
        let total = wire.len();
        let crc = crc32(&wire[..total - 4]);
        wire.splice(total - 4.., crc.to_be_bytes());
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::InvalidHeader {
                field: "name length"
            }
        );
    }

    #[test]
    fn header_value_running_past_the_header_block_fails() {
        // A string header whose declared length exceeds the block it lives in.
        let wire = encode(
            &[(":x", HeaderValue::Raw(7, vec![0xFF, 0xFF, b'a']))],
            b"{}",
        );
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::InvalidHeader {
                field: "string value"
            }
        );
    }

    #[test]
    fn unknown_header_type_indicator_fails() {
        let wire = encode(&[(":x", HeaderValue::Raw(10, vec![0]))], b"{}");
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::UnknownHeaderType { indicator: 10 }
        );
    }

    #[test]
    fn duplicate_header_name_fails() {
        let wire = encode(
            &[
                (":event-type", HeaderValue::Text("a")),
                (":event-type", HeaderValue::Text("b")),
            ],
            b"{}",
        );
        assert_eq!(
            decode_all(&[&wire]).unwrap_err(),
            EventStreamError::InvalidHeader {
                field: "duplicate name"
            }
        );
    }

    #[test]
    fn non_string_headers_are_skipped_by_their_exact_encoded_width() {
        let wire = encode(
            &[
                (":message-type", HeaderValue::Text("event")),
                ("sequence", HeaderValue::Long(42)),
                ("blob", HeaderValue::Bytes(&[1, 2, 3])),
                (":event-type", HeaderValue::Text("messageStart")),
            ],
            b"{}",
        );
        let messages = decode_all(&[&wire]).unwrap();
        assert_eq!(messages[0].header(":event-type"), Some("messageStart"));
        assert_eq!(messages[0].header("sequence"), None);
        assert_eq!(messages[0].payload, b"{}".to_vec());
    }

    #[test]
    fn headerless_payloadless_message_decodes() {
        let wire = encode(&[], b"");
        let messages = decode_all(&[&wire]).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].headers.is_empty());
        assert!(messages[0].payload.is_empty());
    }

    #[test]
    fn message_debug_never_prints_payload_bytes() {
        let wire = text_message("messageStart", r#"{"secret":"value"}"#);
        let messages = decode_all(&[&wire]).unwrap();
        let rendered = format!("{:?}", messages[0]);
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(rendered.contains("payload_bytes"), "{rendered}");
    }

    #[test]
    fn config_debug_redacts_the_credential() {
        let config = BedrockConverseConfig::with_api_key(
            ProviderDescriptor {
                id: "bedrock".to_owned(),
                display_name: "Bedrock".to_owned(),
                protocols: vec![ProviderProtocol::BedrockConverse],
            },
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            "super-secret-key",
        );
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("super-secret-key"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
    }

    #[test]
    fn model_id_is_percent_encoded_as_a_non_greedy_uri_label() {
        let url = converse_stream_url(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Ainference-profile%2Fus.anthropic.claude/converse-stream"
        );
    }

    fn event_stream_response(body: Vec<u8>) -> heycode_http::HttpResponse {
        heycode_http::HttpResponse {
            status: 200,
            content_type: Some(EVENT_STREAM_MEDIA_TYPE.to_owned()),
            headers: BTreeMap::new(),
            body,
        }
    }

    fn complete_text_stream() -> Vec<u8> {
        let mut wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        wire.extend_from_slice(&text_message("messageStop", r#"{"stopReason":"end_turn"}"#));
        wire.extend_from_slice(&text_message(
            "metadata",
            r#"{"usage":{"inputTokens":1,"outputTokens":1,"totalTokens":2}}"#,
        ));
        wire
    }

    #[test]
    fn decoding_never_appends_a_framing_error_after_the_terminal_finish() {
        let mut wire = complete_text_stream();
        wire.extend_from_slice(&[0xDE, 0xAD]);
        let events = decode_converse_stream(
            &event_stream_response(wire),
            &BTreeSet::new(),
            "bedrock",
            "model",
        );
        assert!(events.iter().all(Result::is_ok), "{events:?}");
        assert!(matches!(
            events.last(),
            Some(Ok(InferenceEvent::Finish(FinishReason::Stop)))
        ));
    }

    #[test]
    fn decoding_reports_a_truncated_tail_while_the_stream_is_still_open() {
        let mut wire = text_message("messageStart", r#"{"role":"assistant"}"#);
        wire.extend_from_slice(&[0xDE, 0xAD]);
        let events = decode_converse_stream(
            &event_stream_response(wire),
            &BTreeSet::new(),
            "bedrock",
            "model",
        );
        assert!(matches!(
            events.last(),
            Some(Err(LlmError::InvalidResponse(_)))
        ));
    }
}
