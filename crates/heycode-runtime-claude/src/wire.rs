//! R09 — pinned Claude Code stream-json frame boundary.
//!
//! The CLI's stdout union is deliberately open and large: the shipped 2.1.250
//! implementation carries 95 distinct `subtype` literals and 35 documented
//! `SDKMessage` variants, and the documentation states new members may appear.
//! A parser that errors on anything it does not model would therefore kill a
//! healthy session the first time Claude emits a task notification or a hook
//! event (the exact failure class recorded as GOTCHAS #136 for Codex).
//!
//! So this boundary inverts the Codex rule for *stdout*: an unmodelled
//! top-level `type` is ignored, not fatal. What stays strict is everything the
//! session actually acts on — envelope shape, session identity, correlation
//! ids, control-request subtypes and bounded payload sizes.

use serde_json::{Map, Value};

/// Largest accepted single stdout line. The CLI emits complete assistant
/// messages, so this is generous but still bounded.
pub(crate) const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Largest accepted text projected into a runtime event.
pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Largest accepted bounded detail string.
pub(crate) const MAX_DETAIL_BYTES: usize = 4 * 1024;

/// Control-protocol envelope `type` values, from the shipped 2.1.250 bundle.
pub(crate) const CONTROL_REQUEST: &str = "control_request";
pub(crate) const CONTROL_RESPONSE: &str = "control_response";
pub(crate) const CONTROL_CANCEL_REQUEST: &str = "control_cancel_request";
pub(crate) const CONTROL_REQUEST_PROGRESS: &str = "control_request_progress";

/// Control-request subtype this session originates. The pinned bundle also
/// carries `set_model` and `set_permission_mode`; neither is declared here
/// because no consumer exists yet and an unused constant is dead code.
pub(crate) const SUBTYPE_INTERRUPT: &str = "interrupt";

/// Control-request subtypes the CLI originates that this session answers.
pub(crate) const SUBTYPE_CAN_USE_TOOL: &str = "can_use_tool";
pub(crate) const SUBTYPE_REQUEST_USER_DIALOG: &str = "request_user_dialog";

/// One classified inbound stdout line.
#[derive(Debug)]
pub(crate) enum ClaudeFrame {
    /// A `SDKMessage` this session models.
    Message(ClaudeMessage),
    /// A control request the CLI originated.
    ControlRequest {
        request_id: String,
        subtype: String,
        request: Map<String, Value>,
    },
    /// A reply to a control request this session originated.
    ControlResponse {
        request_id: String,
        response: Result<Value, String>,
    },
    /// The peer withdrew one of its own in-flight requests.
    ControlCancel { request_id: String },
    /// A pinned frame carrying nothing this session owns.
    Ignored,
}

/// The modelled subset of the open `SDKMessage` union.
#[derive(Debug)]
pub(crate) enum ClaudeMessage {
    /// `system`/`init` — session identity and readiness.
    Init {
        session_id: String,
        model: Option<String>,
    },
    /// `system`/`compact_boundary` — compaction actually happened.
    CompactBoundary { trigger: String },
    /// A complete assistant message, reduced to the blocks this session
    /// projects. Text and thinking are deliberately absent: streaming deltas
    /// already carry them live, and `result.result` carries the final text, so
    /// re-emitting complete blocks would duplicate both.
    Assistant {
        blocks: Vec<AssistantBlock>,
        /// True when the frame came from a delegated subagent rather than the
        /// main thread. Its tool ids belong to a nested context.
        subagent: bool,
    },
    /// A user frame carrying tool results.
    ToolResults { results: Vec<ToolResult> },
    /// An incremental text or thinking delta from `--include-partial-messages`.
    Delta { kind: DeltaKind, text: String },
    /// Exact input occupancy for the latest main-thread model request.
    MessageStart { model: String, input_tokens: u64 },
    /// Turn settlement.
    Result {
        subtype: String,
        is_error: bool,
        text: Option<String>,
        usage: Option<heycode_core::TokenUsage>,
        model_context_windows: Vec<ClaudeModelContextWindow>,
    },
    /// A safe bounded notice with no state of its own.
    Notice { code: &'static str },
}

/// Structured context capacity from one `result.modelUsage` row.
#[derive(Debug)]
pub(crate) struct ClaudeModelContextWindow {
    pub(crate) model: String,
    pub(crate) canonical_model: Option<String>,
    pub(crate) context_window: u64,
}

/// Which stream a partial delta belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeltaKind {
    /// Visible assistant text.
    Text,
    /// Extended-thinking text.
    Thinking,
}

/// One projected block of a complete assistant message.
#[derive(Debug)]
pub(crate) enum AssistantBlock {
    /// A client tool call the CLI will execute itself.
    ToolUse {
        id: String,
        name: String,
        arguments: Value,
    },
}

/// One correlated tool result.
#[derive(Debug)]
pub(crate) struct ToolResult {
    pub(crate) tool_use_id: String,
    pub(crate) text: String,
    pub(crate) is_error: bool,
}

/// Why a stdout line was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameError {
    /// The line exceeded the accepted bound.
    Oversized,
    /// The line was not a JSON object, or a modelled field had the wrong shape.
    Malformed,
    /// The frame named a different session than the one this handle owns.
    ForeignSession,
}

/// Classify one stdout line against the pinned boundary.
///
/// `expected_session` is `None` only before `system/init` establishes identity.
pub(crate) fn parse_frame(
    line: &str,
    expected_session: Option<&str>,
) -> Result<ClaudeFrame, FrameError> {
    if line.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversized);
    }
    let value: Value = serde_json::from_str(line).map_err(|_| FrameError::Malformed)?;
    let object = value.as_object().ok_or(FrameError::Malformed)?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or(FrameError::Malformed)?;

    // Session identity is checked before any payload is projected, so a frame
    // belonging to another session can never reach the event stream.
    if let (Some(expected), Some(actual)) = (
        expected_session,
        object.get("session_id").and_then(Value::as_str),
    ) && expected != actual
    {
        return Err(FrameError::ForeignSession);
    }

    match kind {
        CONTROL_REQUEST => parse_control_request(object),
        CONTROL_RESPONSE => parse_control_response(object),
        CONTROL_CANCEL_REQUEST => Ok(ClaudeFrame::ControlCancel {
            request_id: required_id(object, "request_id")?,
        }),
        // A liveness heartbeat with no payload; the documentation states
        // receivers must ignore it.
        CONTROL_REQUEST_PROGRESS => Ok(ClaudeFrame::Ignored),
        "system" => parse_system(object),
        "assistant" => parse_assistant(object),
        "user" => parse_user(object),
        "stream_event" => parse_stream_event(object),
        "result" => parse_result(object),
        // Open union: an unmodelled pinned message carries nothing this session
        // owns, and killing the session over it would be the GOTCHAS #136 bug.
        _ => Ok(ClaudeFrame::Ignored),
    }
}

fn parse_control_request(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    let request_id = required_id(object, "request_id")?;
    let request = object
        .get("request")
        .and_then(Value::as_object)
        .ok_or(FrameError::Malformed)?;
    let subtype = request
        .get("subtype")
        .and_then(Value::as_str)
        .ok_or(FrameError::Malformed)?
        .to_owned();
    Ok(ClaudeFrame::ControlRequest {
        request_id,
        subtype,
        request: request.clone(),
    })
}

fn parse_control_response(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    let response = object
        .get("response")
        .and_then(Value::as_object)
        .ok_or(FrameError::Malformed)?;
    let request_id = required_id(response, "request_id")?;
    let response = match response.get("subtype").and_then(Value::as_str) {
        Some("error") => Err(response
            .get("error")
            .and_then(Value::as_str)
            .map_or_else(|| "control request failed".to_owned(), bounded_detail)),
        Some("success") => Ok(response
            .get("response")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()))),
        _ => return Err(FrameError::Malformed),
    };
    Ok(ClaudeFrame::ControlResponse {
        request_id,
        response,
    })
}

fn parse_system(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    match object.get("subtype").and_then(Value::as_str) {
        Some("init") => Ok(ClaudeFrame::Message(ClaudeMessage::Init {
            session_id: required_id(object, "session_id")?,
            model: object
                .get("model")
                .and_then(Value::as_str)
                .map(bounded_detail),
        })),
        Some("compact_boundary") => {
            let trigger = object
                .get("compact_metadata")
                .and_then(Value::as_object)
                .and_then(|metadata| metadata.get("trigger"))
                .and_then(Value::as_str)
                .ok_or(FrameError::Malformed)?;
            if trigger != "manual" && trigger != "auto" {
                return Err(FrameError::Malformed);
            }
            Ok(ClaudeFrame::Message(ClaudeMessage::CompactBoundary {
                trigger: trigger.to_owned(),
            }))
        }
        Some("api_retry") => Ok(ClaudeFrame::Message(ClaudeMessage::Notice {
            code: "claude.api.retry",
        })),
        // Every other pinned system subtype is startup or telemetry noise.
        _ => Ok(ClaudeFrame::Ignored),
    }
}

fn parse_assistant(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    let content = object
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .ok_or(FrameError::Malformed)?;
    let mut blocks = Vec::new();
    for block in content {
        let block = block.as_object().ok_or(FrameError::Malformed)?;
        // text and thinking already streamed as deltas; redacted_thinking,
        // server_tool_use and future block types carry nothing this session
        // projects.
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let arguments = block
                .get("input")
                .and_then(Value::as_object)
                .cloned()
                .ok_or(FrameError::Malformed)?;
            blocks.push(AssistantBlock::ToolUse {
                id: required_id(block, "id")?,
                name: bounded_detail(
                    block
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(FrameError::Malformed)?,
                ),
                arguments: Value::Object(arguments),
            });
        }
    }
    Ok(ClaudeFrame::Message(ClaudeMessage::Assistant {
        blocks,
        subagent: object
            .get("parent_tool_use_id")
            .is_some_and(|value| !value.is_null()),
    }))
}

fn parse_user(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    // A replayed echo of our own stdin carries no new state.
    if object.get("isReplay").and_then(Value::as_bool) == Some(true) {
        return Ok(ClaudeFrame::Ignored);
    }
    let Some(content) = object
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Ok(ClaudeFrame::Ignored);
    };
    let mut results = Vec::new();
    for block in content {
        let Some(block) = block.as_object() else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        results.push(ToolResult {
            tool_use_id: required_id(block, "tool_use_id")?,
            text: bounded_text(&result_text(block.get("content"))),
            is_error: block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    if results.is_empty() {
        return Ok(ClaudeFrame::Ignored);
    }
    Ok(ClaudeFrame::Message(ClaudeMessage::ToolResults { results }))
}

fn parse_stream_event(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    let Some(event) = object.get("event").and_then(Value::as_object) else {
        return Ok(ClaudeFrame::Ignored);
    };
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            // Nested subagent requests have their own context and must not
            // replace the main session's status-bar measurement.
            if object
                .get("parent_tool_use_id")
                .is_some_and(|value| !value.is_null())
            {
                return Ok(ClaudeFrame::Ignored);
            }
            let message = event
                .get("message")
                .and_then(Value::as_object)
                .ok_or(FrameError::Malformed)?;
            let model = bounded_model_id(
                message
                    .get("model")
                    .and_then(Value::as_str)
                    .ok_or(FrameError::Malformed)?,
            )?;
            let usage = message
                .get("usage")
                .and_then(Value::as_object)
                .ok_or(FrameError::Malformed)?;
            let direct = optional_u64(usage, "input_tokens")?;
            let cache_read = optional_u64(usage, "cache_read_input_tokens")?;
            let cache_creation = optional_u64(usage, "cache_creation_input_tokens")?;
            let input_tokens = direct
                .checked_add(cache_read)
                .and_then(|tokens| tokens.checked_add(cache_creation))
                .ok_or(FrameError::Malformed)?;
            return Ok(ClaudeFrame::Message(ClaudeMessage::MessageStart {
                model,
                input_tokens,
            }));
        }
        Some("content_block_delta") => {}
        _ => return Ok(ClaudeFrame::Ignored),
    }
    let Some(delta) = event.get("delta").and_then(Value::as_object) else {
        return Ok(ClaudeFrame::Ignored);
    };
    let (kind, field) = match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => (DeltaKind::Text, "text"),
        Some("thinking_delta") => (DeltaKind::Thinking, "thinking"),
        // input_json_delta and signature_delta belong to the complete message.
        _ => return Ok(ClaudeFrame::Ignored),
    };
    let text = delta
        .get(field)
        .and_then(Value::as_str)
        .ok_or(FrameError::Malformed)?;
    if text.is_empty() {
        return Ok(ClaudeFrame::Ignored);
    }
    Ok(ClaudeFrame::Message(ClaudeMessage::Delta {
        kind,
        text: bounded_text(text),
    }))
}

fn parse_result(object: &Map<String, Value>) -> Result<ClaudeFrame, FrameError> {
    let subtype = object
        .get("subtype")
        .and_then(Value::as_str)
        .ok_or(FrameError::Malformed)?
        .to_owned();
    Ok(ClaudeFrame::Message(ClaudeMessage::Result {
        subtype,
        is_error: object
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        // The CLI reports `result` as a bare string or as a content-block
        // array; both shapes carry the answer, so both project through the
        // shared block reader instead of only the string shape.
        text: Some(bounded_text(&result_text(object.get("result"))))
            .filter(|text| !text.is_empty()),
        usage: parse_usage(object.get("usage")),
        model_context_windows: parse_model_context_windows(object.get("modelUsage"))?,
    }))
}

fn parse_model_context_windows(
    value: Option<&Value>,
) -> Result<Vec<ClaudeModelContextWindow>, FrameError> {
    let Some(rows) = value else {
        return Ok(Vec::new());
    };
    let rows = rows.as_object().ok_or(FrameError::Malformed)?;
    if rows.len() > 256 {
        return Err(FrameError::Malformed);
    }
    let mut windows = Vec::with_capacity(rows.len());
    for (model, value) in rows {
        let row = value.as_object().ok_or(FrameError::Malformed)?;
        // `modelUsage` predates `contextWindow`; an older row remains
        // valid aggregate usage but carries no capacity evidence.
        let Some(context_window) = row.get("contextWindow") else {
            continue;
        };
        let context_window = context_window
            .as_u64()
            .filter(|window| *window > 0)
            .ok_or(FrameError::Malformed)?;
        let model = bounded_model_id(model)?;
        let canonical_model = row
            .get("canonicalModel")
            .map(|value| {
                value
                    .as_str()
                    .ok_or(FrameError::Malformed)
                    .and_then(bounded_model_id)
            })
            .transpose()?;
        windows.push(ClaudeModelContextWindow {
            model,
            canonical_model,
            context_window,
        });
    }
    Ok(windows)
}

fn bounded_model_id(value: &str) -> Result<String, FrameError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(FrameError::Malformed);
    }
    Ok(value.to_owned())
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<u64, FrameError> {
    match object.get(field) {
        None => Ok(0),
        Some(value) => value.as_u64().ok_or(FrameError::Malformed),
    }
}

/// Claude reports cumulative Anthropic usage; only the two components the
/// neutral vocabulary can represent are projected, and cache reads are folded
/// into prompt tokens exactly as the Anthropic adapter does.
fn parse_usage(value: Option<&Value>) -> Option<heycode_core::TokenUsage> {
    let usage = value?.as_object()?;
    let number =
        |field: &str| -> u64 { usage.get(field).and_then(Value::as_u64).unwrap_or_default() };
    let prompt_tokens = number("input_tokens")
        .saturating_add(number("cache_read_input_tokens"))
        .saturating_add(number("cache_creation_input_tokens"));
    let completion_tokens = number("output_tokens");
    if prompt_tokens == 0 && completion_tokens == 0 {
        return None;
    }
    Some(heycode_core::TokenUsage {
        prompt_tokens,
        completion_tokens,
    })
}

fn result_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .as_object()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .and_then(|block| block.get("text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn required_id(object: &Map<String, Value>, field: &str) -> Result<String, FrameError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or(FrameError::Malformed)?;
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(FrameError::Malformed);
    }
    Ok(value.to_owned())
}

fn bounded_text(value: &str) -> String {
    truncate_on_char_boundary(value, MAX_TEXT_BYTES)
}

fn bounded_detail(value: &str) -> String {
    truncate_on_char_boundary(&value.replace(['\n', '\r'], " "), MAX_DETAIL_BYTES)
}

fn truncate_on_char_boundary(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn unmodelled_pinned_messages_are_ignored_not_fatal() {
        // The 2.1.250 stdout union is explicitly open; these are all real
        // pinned members this session does not model. Failing on them would
        // kill a healthy session (GOTCHAS #136 in the other direction).
        for line in [
            r#"{"type":"task_started","session_id":"s","task_id":"t"}"#,
            r#"{"type":"hook_started","session_id":"s"}"#,
            r#"{"type":"rate_limit_event","session_id":"s"}"#,
            r#"{"type":"commands_changed","session_id":"s"}"#,
            r#"{"type":"system","subtype":"plugin_install","session_id":"s"}"#,
            r#"{"type":"control_request_progress","request_id":"r"}"#,
        ] {
            assert!(
                matches!(parse_frame(line, Some("s")), Ok(ClaudeFrame::Ignored)),
                "must ignore: {line}"
            );
        }
    }

    #[test]
    fn envelope_shape_and_session_identity_stay_strict() {
        assert_eq!(
            parse_frame("not json", None).unwrap_err(),
            FrameError::Malformed
        );
        assert_eq!(parse_frame("[]", None).unwrap_err(), FrameError::Malformed);
        assert_eq!(
            parse_frame(r#"{"session_id":"s"}"#, None).unwrap_err(),
            FrameError::Malformed
        );
        assert_eq!(
            parse_frame(r#"{"type":"assistant","session_id":"other"}"#, Some("mine")).unwrap_err(),
            FrameError::ForeignSession
        );
        let oversized = format!(r#"{{"type":"x","pad":"{}"}}"#, "p".repeat(MAX_FRAME_BYTES));
        assert_eq!(
            parse_frame(&oversized, None).unwrap_err(),
            FrameError::Oversized
        );
        // A control request without a subtype cannot be answered, so it is a
        // hard error rather than a silent ignore.
        assert_eq!(
            parse_frame(
                r#"{"type":"control_request","request_id":"r","request":{}}"#,
                None
            )
            .unwrap_err(),
            FrameError::Malformed
        );
    }

    #[test]
    fn assistant_frames_project_only_tool_calls_and_flag_subagent_origin() {
        let line = r#"{"type":"assistant","session_id":"s","parent_tool_use_id":null,"message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"weighing"},
            {"type":"text","text":"answer"},
            {"type":"redacted_thinking","data":"opaque"},
            {"type":"tool_use","id":"toolu_1","name":"Bash","input":{}},
            {"type":"tool_use","id":"toolu_2","name":"Read","input":{}}
        ]}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::Assistant { blocks, subagent })) =
            parse_frame(line, Some("s"))
        else {
            panic!("expected an assistant message")
        };
        assert!(!subagent);
        // Text and thinking already arrived as deltas, and the final text comes
        // from the result frame, so re-projecting them would duplicate both.
        assert_eq!(blocks.len(), 2);
        assert!(
            matches!(&blocks[0], AssistantBlock::ToolUse { id, name, arguments } if id == "toolu_1" && name == "Bash" && arguments == &serde_json::json!({}))
        );
        assert!(
            matches!(&blocks[1], AssistantBlock::ToolUse { id, name, arguments } if id == "toolu_2" && name == "Read" && arguments == &serde_json::json!({}))
        );

        let nested = r#"{"type":"assistant","session_id":"s","parent_tool_use_id":"toolu_parent","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_9","name":"Grep","input":{}}
        ]}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::Assistant { subagent, .. })) =
            parse_frame(nested, Some("s"))
        else {
            panic!("expected an assistant message")
        };
        assert!(subagent, "a nested frame must be distinguishable");

        let arguments = r#"{"type":"assistant","session_id":"s","parent_tool_use_id":null,"message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_10","name":"Bash","input":{"command":"ls -la","timeout_ms":1000}}
        ]}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::Assistant { blocks, .. })) =
            parse_frame(arguments, Some("s"))
        else {
            panic!("expected an assistant message")
        };
        assert!(matches!(
            &blocks[0],
            AssistantBlock::ToolUse { arguments, .. }
                if arguments == &serde_json::json!({"command":"ls -la","timeout_ms":1000})
        ));

        let malformed = r#"{"type":"assistant","session_id":"s","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"toolu_11","name":"Bash","input":"ls"}
        ]}}"#;
        assert_eq!(
            parse_frame(malformed, Some("s")).unwrap_err(),
            FrameError::Malformed
        );
    }

    #[test]
    fn partial_deltas_split_text_from_thinking_and_ignore_the_rest() {
        let start = r#"{"type":"stream_event","session_id":"s","parent_tool_use_id":null,"event":{"type":"message_start","message":{"model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":2765,"cache_read_input_tokens":0}}}}"#;
        assert!(matches!(
            parse_frame(start, Some("s")),
            Ok(ClaudeFrame::Message(ClaudeMessage::MessageStart {
                model,
                input_tokens: 2_767,
            })) if model == "claude-opus-5"
        ));
        let nested_start = r#"{"type":"stream_event","session_id":"s","parent_tool_use_id":"toolu_parent","event":{"type":"message_start","message":{"model":"claude-opus-5","usage":{"input_tokens":500}}}}"#;
        assert!(matches!(
            parse_frame(nested_start, Some("s")),
            Ok(ClaudeFrame::Ignored)
        ));
        let text = r#"{"type":"stream_event","session_id":"s","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#;
        assert!(matches!(
            parse_frame(text, Some("s")),
            Ok(ClaudeFrame::Message(ClaudeMessage::Delta {
                kind: DeltaKind::Text,
                ..
            }))
        ));
        let thinking = r#"{"type":"stream_event","session_id":"s","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}}"#;
        assert!(matches!(
            parse_frame(thinking, Some("s")),
            Ok(ClaudeFrame::Message(ClaudeMessage::Delta {
                kind: DeltaKind::Thinking,
                ..
            }))
        ));
        let json = r#"{"type":"stream_event","session_id":"s","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{"}}}"#;
        assert!(matches!(
            parse_frame(json, Some("s")),
            Ok(ClaudeFrame::Ignored)
        ));
    }

    #[test]
    fn replayed_user_frames_are_ignored_while_tool_results_are_correlated() {
        let replay = r#"{"type":"user","session_id":"s","isReplay":true,"message":{"role":"user","content":"hi"}}"#;
        assert!(matches!(
            parse_frame(replay, Some("s")),
            Ok(ClaudeFrame::Ignored)
        ));
        let results = r#"{"type":"user","session_id":"s","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":[{"type":"text","text":"boom"}]}
        ]}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::ToolResults { results })) =
            parse_frame(results, Some("s"))
        else {
            panic!("expected tool results")
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_use_id, "toolu_1");
        assert_eq!(results[0].text, "boom");
        assert!(results[0].is_error);
    }

    #[test]
    fn compaction_and_usage_project_exact_evidence() {
        let boundary = r#"{"type":"system","subtype":"compact_boundary","session_id":"s","compact_metadata":{"trigger":"manual","pre_tokens":50}}"#;
        assert!(matches!(
            parse_frame(boundary, Some("s")),
            Ok(ClaudeFrame::Message(ClaudeMessage::CompactBoundary { trigger })) if trigger == "manual"
        ));
        let bogus = r#"{"type":"system","subtype":"compact_boundary","session_id":"s","compact_metadata":{"trigger":"guessed"}}"#;
        assert_eq!(
            parse_frame(bogus, Some("s")).unwrap_err(),
            FrameError::Malformed
        );

        let result = r#"{"type":"result","subtype":"success","session_id":"s","is_error":false,"result":"done","usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":7},"modelUsage":{"claude-opus-5[1m]":{"contextWindow":1000000,"canonicalModel":"claude-opus-5"}}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::Result {
            usage,
            text,
            is_error,
            subtype,
            model_context_windows,
        })) = parse_frame(result, Some("s"))
        else {
            panic!("expected a result")
        };
        assert_eq!(subtype, "success");
        assert!(!is_error);
        assert_eq!(text.as_deref(), Some("done"));
        let usage = usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 15, "cache reads fold into prompt");
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(model_context_windows.len(), 1);
        assert_eq!(model_context_windows[0].model, "claude-opus-5[1m]");
        assert_eq!(
            model_context_windows[0].canonical_model.as_deref(),
            Some("claude-opus-5")
        );
        assert_eq!(model_context_windows[0].context_window, 1_000_000);

        let older = r#"{"type":"result","subtype":"success","session_id":"s","result":"done","usage":{"input_tokens":1},"modelUsage":{"claude-opus-5":{"inputTokens":1,"outputTokens":2}}}"#;
        let Ok(ClaudeFrame::Message(ClaudeMessage::Result {
            model_context_windows,
            ..
        })) = parse_frame(older, Some("s"))
        else {
            panic!("expected backwards-compatible result")
        };
        assert!(model_context_windows.is_empty());
    }
}
