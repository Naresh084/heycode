//! Bounded body-private JSONL/JSON-RPC envelope handling.

use std::hash::{Hash, Hasher};

use serde_json::{Map, Value};

use crate::budget::InboundPermit;
use crate::config::WIRE_LINE_LIMIT;
use crate::{CodexAppServerError, CodexAppServerErrorCode};

const MAX_METHOD_BYTES: usize = 256;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_NODES: usize = 65_536;
const MAX_REMOTE_MESSAGE_BYTES: usize = 4 * 1024;
const RETAINED_JSON_NODE_CHARGE: usize = 128;

/// Opaque string-or-integer JSON-RPC request identity.
#[derive(Clone, PartialEq, Eq)]
pub struct CodexRequestId(RequestIdValue);

#[derive(Clone, PartialEq, Eq, Hash)]
enum RequestIdValue {
    Integer(i64),
    String(String),
}

impl CodexRequestId {
    pub(crate) const fn integer(value: i64) -> Self {
        Self(RequestIdValue::Integer(value))
    }

    fn parse(value: &Value) -> Result<Self, CodexAppServerError> {
        match value {
            Value::Number(number) => number.as_i64().map(Self::integer).ok_or_else(protocol),
            Value::String(value) if valid_opaque_id(value) => {
                Ok(Self(RequestIdValue::String(value.clone())))
            }
            _ => Err(protocol()),
        }
    }

    pub(crate) const fn as_client_integer(&self) -> Option<i64> {
        match self.0 {
            RequestIdValue::Integer(value) => Some(value),
            RequestIdValue::String(_) => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match &self.0 {
            RequestIdValue::Integer(value) => Value::from(*value),
            RequestIdValue::String(value) => Value::String(value.clone()),
        }
    }

    pub(crate) fn matches_json(&self, value: &Value) -> bool {
        self.to_json() == *value
    }
}

impl Hash for CodexRequestId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl std::fmt::Debug for CodexRequestId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CodexRequestId(<redacted>)")
    }
}

/// Body-private successful response payload.
pub struct CodexResponsePayload {
    value: Value,
    _permit: Option<InboundPermit>,
}

impl CodexResponsePayload {
    pub(crate) const fn new(value: Value) -> Self {
        Self {
            value,
            _permit: None,
        }
    }

    fn with_permit(mut self, permit: InboundPermit) -> Self {
        self._permit = Some(permit);
        self
    }

    /// Borrow the validated raw response for a typed higher-level bridge.
    #[must_use]
    pub const fn value(&self) -> &Value {
        &self.value
    }

    /// Consume the validated response for a typed higher-level bridge.
    #[must_use]
    pub fn into_value(self) -> Value {
        self.value
    }
}

impl std::fmt::Debug for CodexResponsePayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexResponsePayload")
            .field("kind", &json_kind(&self.value))
            .finish()
    }
}

impl PartialEq for CodexResponsePayload {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

/// One body-private server notification.
pub struct CodexNotification {
    method: String,
    params: Value,
    _permit: Option<InboundPermit>,
}

impl CodexNotification {
    /// Exact validated notification method.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Borrow validated opaque params for a typed higher-level bridge.
    #[must_use]
    pub const fn params(&self) -> &Value {
        &self.params
    }
}

impl std::fmt::Debug for CodexNotification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexNotification")
            .field("method_bytes", &self.method.len())
            .field("params_kind", &json_kind(&self.params))
            .finish()
    }
}

impl PartialEq for CodexNotification {
    fn eq(&self, other: &Self) -> bool {
        self.method == other.method && self.params == other.params
    }
}

/// One correlated body-private request initiated by app-server.
pub struct CodexServerRequest {
    id: CodexRequestId,
    method: String,
    params: Value,
    _permit: Option<InboundPermit>,
}

impl CodexServerRequest {
    /// Opaque request identity to echo in the client response.
    #[must_use]
    pub const fn id(&self) -> &CodexRequestId {
        &self.id
    }

    /// Exact validated request method.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Borrow validated opaque params for a typed higher-level bridge.
    #[must_use]
    pub const fn params(&self) -> &Value {
        &self.params
    }
}

impl std::fmt::Debug for CodexServerRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexServerRequest")
            .field("id", &self.id)
            .field("method_bytes", &self.method.len())
            .field("params_kind", &json_kind(&self.params))
            .finish()
    }
}

impl PartialEq for CodexServerRequest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.method == other.method && self.params == other.params
    }
}

/// Body-private inbound event for the one connection consumer.
#[derive(PartialEq)]
pub enum CodexInboundEvent {
    /// Fire-and-forget server notification.
    Notification(CodexNotification),
    /// Server request requiring one correlated response.
    Request(CodexServerRequest),
}

impl std::fmt::Debug for CodexInboundEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notification(_) => formatter.write_str("CodexInboundEvent::Notification"),
            Self::Request(_) => formatter.write_str("CodexInboundEvent::Request"),
        }
    }
}

pub(crate) enum ParsedServerMessage {
    Response {
        id: CodexRequestId,
        payload: CodexResponsePayload,
    },
    Error {
        id: CodexRequestId,
        code: i64,
    },
    Notification(CodexNotification),
    Request(CodexServerRequest),
}

impl ParsedServerMessage {
    pub(crate) fn with_permit(self, permit: InboundPermit) -> Self {
        match self {
            Self::Response { id, payload } => Self::Response {
                id,
                payload: payload.with_permit(permit),
            },
            Self::Notification(mut notification) => {
                notification._permit = Some(permit);
                Self::Notification(notification)
            }
            Self::Request(mut request) => {
                request._permit = Some(permit);
                Self::Request(request)
            }
            Self::Error { id, code } => {
                drop(permit);
                Self::Error { id, code }
            }
        }
    }
}

impl std::fmt::Debug for ParsedServerMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = match self {
            Self::Response { .. } => "response",
            Self::Error { .. } => "error",
            Self::Notification(_) => "notification",
            Self::Request(_) => "request",
        };
        formatter
            .debug_struct("ParsedServerMessage")
            .field("phase", &phase)
            .finish()
    }
}

#[cfg(test)]
pub(crate) fn parse_server_line(line: &str) -> Result<ParsedServerMessage, CodexAppServerError> {
    parse_server_frame(line.as_bytes()).map(|(message, _weight)| message)
}

pub(crate) fn parse_server_frame(
    frame: &[u8],
) -> Result<(ParsedServerMessage, usize), CodexAppServerError> {
    if frame.is_empty() || frame.len() > WIRE_LINE_LIMIT || frame.contains(&0) {
        return Err(protocol());
    }
    let line = std::str::from_utf8(frame).map_err(|_| protocol())?;
    let value: Value = serde_json::from_str(line).map_err(|_| protocol())?;
    let nodes = validate_json(&value)?;
    let weight = frame
        .len()
        .checked_add(
            nodes
                .checked_mul(RETAINED_JSON_NODE_CHARGE)
                .ok_or_else(protocol)?,
        )
        .ok_or_else(protocol)?;
    let object = value.as_object().ok_or_else(protocol)?;
    if object.contains_key("jsonrpc") {
        return Err(protocol());
    }
    let id = object.get("id").map(CodexRequestId::parse).transpose()?;
    let method = object.get("method").map(parse_method).transpose()?;
    let result = object.get("result");
    let error = object.get("error");
    let params = object.get("params");

    match (id, method, result, error, params) {
        (Some(id), None, Some(result), None, None) => Ok((
            ParsedServerMessage::Response {
                id,
                payload: CodexResponsePayload::new(result.clone()),
            },
            weight,
        )),
        (Some(id), None, None, Some(error), None) => {
            let error = error.as_object().ok_or_else(protocol)?;
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .ok_or_else(protocol)?;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .filter(|message| valid_display_text(message, MAX_REMOTE_MESSAGE_BYTES))
                .ok_or_else(protocol)?;
            let _validated_not_retained = message;
            Ok((ParsedServerMessage::Error { id, code }, weight))
        }
        (Some(id), Some(method), None, None, Some(params)) if params.is_object() => Ok((
            ParsedServerMessage::Request(CodexServerRequest {
                id,
                method,
                params: params.clone(),
                _permit: None,
            }),
            weight,
        )),
        (None, Some(method), None, None, Some(params)) if params.is_object() => Ok((
            ParsedServerMessage::Notification(CodexNotification {
                method,
                params: params.clone(),
                _permit: None,
            }),
            weight,
        )),
        _ => Err(protocol()),
    }
}

pub(crate) fn serialize_request(
    id: i64,
    method: &str,
    params: Value,
) -> Result<String, CodexAppServerError> {
    validate_method(method)?;
    if id < 0 || !params.is_object() {
        return Err(invalid_config());
    }
    validate_json(&params).map_err(|_| invalid_config())?;
    let mut object = Map::new();
    object.insert("method".to_owned(), Value::String(method.to_owned()));
    object.insert("id".to_owned(), Value::from(id));
    object.insert("params".to_owned(), params);
    serialize_object(object)
}

pub(crate) fn serialize_notification(
    method: &str,
    params: Value,
) -> Result<String, CodexAppServerError> {
    validate_method(method)?;
    if !params.is_object() {
        return Err(invalid_config());
    }
    validate_json(&params).map_err(|_| invalid_config())?;
    let mut object = Map::new();
    object.insert("method".to_owned(), Value::String(method.to_owned()));
    object.insert("params".to_owned(), params);
    serialize_object(object)
}

pub(crate) fn serialize_success_response(
    id: &CodexRequestId,
    result: Value,
) -> Result<String, CodexAppServerError> {
    validate_json(&result).map_err(|_| invalid_config())?;
    let mut object = Map::new();
    object.insert("id".to_owned(), id.to_json());
    object.insert("result".to_owned(), result);
    serialize_object(object)
}

fn serialize_object(object: Map<String, Value>) -> Result<String, CodexAppServerError> {
    let line = serde_json::to_string(&Value::Object(object)).map_err(|_| protocol())?;
    if line.len() > WIRE_LINE_LIMIT {
        return Err(protocol());
    }
    Ok(line)
}

fn parse_method(value: &Value) -> Result<String, CodexAppServerError> {
    let method = value.as_str().ok_or_else(protocol)?;
    validate_method(method).map_err(|_| protocol())?;
    Ok(method.to_owned())
}

fn validate_method(method: &str) -> Result<(), CodexAppServerError> {
    let bytes = method.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_METHOD_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'.' | b'_' | b'-'));
    if valid { Ok(()) } else { Err(invalid_config()) }
}

fn validate_json(value: &Value) -> Result<usize, CodexAppServerError> {
    let mut stack = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.checked_add(1).ok_or_else(protocol)?;
        if nodes > MAX_JSON_NODES || depth > MAX_JSON_DEPTH {
            return Err(protocol());
        }
        match value {
            Value::String(value) => {
                if !valid_display_text(value, WIRE_LINE_LIMIT) {
                    return Err(protocol());
                }
            }
            Value::Array(values) => {
                ensure_frontier(nodes, stack.len(), values.len())?;
                stack.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(values) => {
                ensure_frontier(nodes, stack.len(), values.len())?;
                if values
                    .keys()
                    .any(|key| !valid_display_text(key, MAX_METHOD_BYTES))
                {
                    return Err(protocol());
                }
                stack.extend(values.values().map(|value| (value, depth + 1)));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
    Ok(nodes)
}

fn ensure_frontier(
    visited: usize,
    pending: usize,
    incoming: usize,
) -> Result<(), CodexAppServerError> {
    let total = visited
        .checked_add(pending)
        .and_then(|count| count.checked_add(incoming))
        .ok_or_else(protocol)?;
    if total > MAX_JSON_NODES {
        Err(protocol())
    } else {
        Ok(())
    }
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_display_text(value: &str, limit: usize) -> bool {
    value.len() <= limit
        && !value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn protocol() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Protocol)
}

fn invalid_config() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::InvalidConfig)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn envelopes_are_headerless_bounded_and_strictly_classified() {
        let request = serialize_request(7, "thread/start", serde_json::json!({})).unwrap();
        assert!(!request.contains("jsonrpc"));
        assert!(matches!(
            parse_server_line(r#"{"id":7,"result":{"ok":true}}"#).unwrap(),
            ParsedServerMessage::Response { .. }
        ));
        for invalid in [
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            r#"{"id":7,"result":{},"error":{"code":1,"message":"x"}}"#,
            r#"{"id":7,"method":"bad method","params":{}}"#,
            r#"{"method":"notice","params":"not-object"}"#,
            "not-json private-canary",
        ] {
            let error = parse_server_line(invalid).unwrap_err();
            assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
            assert!(!error.to_string().contains("private-canary"));
        }
    }

    #[test]
    fn debug_surfaces_never_render_payload_values_or_request_ids() {
        let message = parse_server_line(
            r#"{"id":"private-id","method":"fixture/request","params":{"secret":"private-value"}}"#,
        )
        .unwrap();
        let ParsedServerMessage::Request(request) = message else {
            panic!("wrong parsed phase");
        };
        let debug = format!("{request:?} {:?}", request.id());
        assert!(!debug.contains("private-id"));
        assert!(!debug.contains("private-value"));
    }

    #[test]
    fn nested_terminal_controls_fail_before_projection() {
        let error =
            parse_server_line("{\"method\":\"notice\",\"params\":{\"value\":\"safe\\u001b[31m\"}}")
                .unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
    }

    #[test]
    fn oversized_line_fails_before_json_allocation_or_body_projection() {
        let canary = "private-payload-canary";
        let line = canary.repeat(WIRE_LINE_LIMIT / canary.len() + 1);
        let error = parse_server_line(&line).unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
        assert!(!error.to_string().contains(canary));
    }

    #[test]
    fn invalid_utf8_is_never_replaced_before_json_parsing() {
        let frame = b"{\"method\":\"notice\",\"params\":{\"value\":\"\xff\"}}";
        let error = parse_server_frame(frame).unwrap_err();
        assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
        assert!(!error.to_string().contains('\u{fffd}'));
    }

    #[test]
    fn request_ids_accept_schema_integer_or_string_and_reject_other_shapes() {
        for line in [
            r#"{"id":1,"result":{}}"#,
            r#"{"id":"request-1","result":{}}"#,
        ] {
            assert!(matches!(
                parse_server_line(line).unwrap(),
                ParsedServerMessage::Response { .. }
            ));
        }
        assert!(parse_server_line(r#"{"id":null,"result":{}}"#).is_err());
    }
}
