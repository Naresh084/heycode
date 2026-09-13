//! OpenAI Chat Completions interpretation over provider-neutral SSE events.
//!
//! [`heycode_http::SseDecoder`] owns byte fragmentation and SSE field framing;
//! this module owns only OpenAI-compatible JSON and normalized chunks.
//! [`SseParser`] retains the old byte-fed facade for compatibility/tests but
//! delegates every byte to that shared decoder.
//!
//! Contract enforcement (AGENTS §5): the parser holds `Usage` and `Finish`
//! markers until the stream ends (`data: [DONE]` or end of body) and then
//! emits at most one `Usage` immediately followed by at most one `Finish`.
//! Frames arriving after a `finish_reason` contribute no deltas, so nothing
//! is ever emitted after `Finish`.

use crate::error::LlmError;
use crate::vocab::{FinishReason, StreamChunk, TokenUsage};

/// Decoder for one SSE response body. Feed bytes as they arrive; call
/// [`SseParser::finish`] once the body ends.
#[derive(Debug, Default)]
pub struct SseParser {
    decoder: heycode_http::SseDecoder,
    protocol: OpenAiEventParser,
}

/// OpenAI-compatible JSON event interpreter used by the HTTP transport path.
#[derive(Debug, Default)]
pub(crate) struct OpenAiEventParser {
    done: bool,
    pending_usage: Option<TokenUsage>,
    pending_finish: Option<FinishReason>,
}

/// One decoded wire frame: deltas plus deferred usage/finish markers.
#[derive(Debug, Default)]
struct Frame {
    deltas: Vec<StreamChunk>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
}

impl SseParser {
    /// An empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: heycode_http::SseDecoder::new(),
            protocol: OpenAiEventParser::default(),
        }
    }

    /// Ingest `bytes` and return every event they completed. After
    /// `data: [DONE]` the parser is closed and all further feeds yield
    /// nothing.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Result<StreamChunk, LlmError>> {
        if self.protocol.done {
            return Vec::new();
        }
        match self.decoder.feed(bytes) {
            Ok(events) => self.protocol.events(events),
            Err(error) => vec![Err(LlmError::InvalidResponse(error.to_string()))],
        }
    }

    /// Flush decoder state at end of body: decode any unterminated final
    /// line, then release held usage/finish markers. Empty when the stream
    /// already ended cleanly.
    pub fn finish(mut self) -> Vec<Result<StreamChunk, LlmError>> {
        let mut out = Vec::new();
        if !self.protocol.done {
            match self.decoder.finish() {
                Ok(events) => out.extend(self.protocol.events(events)),
                Err(error) => out.push(Err(LlmError::InvalidResponse(error.to_string()))),
            }
        }
        if !self.protocol.done {
            self.protocol.release_held(&mut out);
        }
        out
    }
}

impl OpenAiEventParser {
    pub(crate) fn event(
        &mut self,
        event: heycode_http::SseEvent,
    ) -> Vec<Result<StreamChunk, LlmError>> {
        if self.done {
            return Vec::new();
        }
        let mut out = Vec::new();
        if event.data.trim() == "[DONE]" {
            self.done = true;
            self.release_held(&mut out);
        } else if !event.data.trim().is_empty() {
            match parse_frame(&event.data) {
                Ok(frame) => self.absorb(frame, &mut out),
                Err(error) => out.push(Err(error)),
            }
        }
        out
    }

    pub(crate) fn events(
        &mut self,
        events: impl IntoIterator<Item = heycode_http::SseEvent>,
    ) -> Vec<Result<StreamChunk, LlmError>> {
        let mut out = Vec::new();
        for event in events {
            out.extend(self.event(event));
        }
        out
    }

    fn absorb(&mut self, frame: Frame, out: &mut Vec<Result<StreamChunk, LlmError>>) {
        if self.pending_finish.is_none() {
            out.extend(frame.deltas.into_iter().map(Ok));
            if frame.finish.is_some() {
                self.pending_finish = frame.finish;
            }
        }
        // Usage may trail the finish frame (proxy-specific ordering); it is
        // held so it still lands immediately before Finish.
        if frame.usage.is_some() {
            self.pending_usage = frame.usage;
        }
    }

    fn release_held(&mut self, out: &mut Vec<Result<StreamChunk, LlmError>>) {
        if let Some(usage) = self.pending_usage.take() {
            out.push(Ok(StreamChunk::Usage(usage)));
        }
        if let Some(finish) = self.pending_finish.take() {
            out.push(Ok(StreamChunk::Finish(finish)));
        }
    }
}

/// Decode one `data:` payload into deltas plus deferred markers.
fn parse_frame(payload: &str) -> Result<Frame, LlmError> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|err| LlmError::InvalidResponse(format!("SSE data is not valid JSON: {err}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| LlmError::InvalidResponse("SSE data is not a JSON object".into()))?;

    let mut frame = Frame::default();
    if let Some(choices) = obj.get("choices").and_then(serde_json::Value::as_array) {
        for choice in choices {
            if frame.finish.is_none() {
                frame.finish = choice.get("finish_reason").and_then(finish_reason);
            }
            absorb_delta(choice.get("delta"), &mut frame)?;
        }
    }
    if let Some(usage) = obj.get("usage").filter(|usage| !usage.is_null()) {
        let fields = usage
            .as_object()
            .ok_or_else(|| LlmError::InvalidResponse("`usage` must be an object".into()))?;
        frame.usage = Some(TokenUsage {
            prompt_tokens: token_count(fields, "prompt_tokens")?,
            completion_tokens: token_count(fields, "completion_tokens")?,
        });
    }
    Ok(frame)
}

/// Map a wire `finish_reason` to the vocabulary. Unknown reasons degrade to
/// [`FinishReason::Stop`]; absent, null, or wrongly typed values yield none.
fn finish_reason(value: &serde_json::Value) -> Option<FinishReason> {
    match value.as_str() {
        Some("tool_calls") => Some(FinishReason::ToolCalls),
        Some("length") => Some(FinishReason::Length),
        Some(_) => Some(FinishReason::Stop),
        None => None,
    }
}

/// Append the deltas of one choice's `delta` object to `frame`.
fn absorb_delta(delta: Option<&serde_json::Value>, frame: &mut Frame) -> Result<(), LlmError> {
    let Some(obj) = delta.and_then(serde_json::Value::as_object) else {
        return Ok(());
    };
    if let Some(content) = optional_str(obj, "content")?
        && !content.is_empty()
    {
        frame
            .deltas
            .push(StreamChunk::TextDelta(content.to_owned()));
    }
    if let Some(reasoning) = optional_str(obj, "reasoning_content")?
        && !reasoning.is_empty()
    {
        frame
            .deltas
            .push(StreamChunk::ReasoningDelta(reasoning.to_owned()));
    }
    let Some(tool_calls) = obj.get("tool_calls").filter(|calls| !calls.is_null()) else {
        return Ok(());
    };
    let calls = tool_calls
        .as_array()
        .ok_or_else(|| LlmError::InvalidResponse("`tool_calls` must be an array".into()))?;
    for call in calls {
        let fields = call
            .as_object()
            .ok_or_else(|| LlmError::InvalidResponse("`tool_calls[]` must be an object".into()))?;
        let index = tool_index(fields)?;
        let id = optional_str(fields, "id")?.map(str::to_owned);
        let (name, arguments) = match fields.get("function").filter(|f| !f.is_null()) {
            None => (None, ""),
            Some(function) => {
                let function = function.as_object().ok_or_else(|| {
                    LlmError::InvalidResponse("`function` must be an object".into())
                })?;
                (
                    optional_str(function, "name")?,
                    optional_str(function, "arguments")?.unwrap_or(""),
                )
            }
        };
        if id.is_some() || name.is_some() || !arguments.is_empty() {
            frame.deltas.push(StreamChunk::ToolCallDelta {
                index,
                id,
                name: name.map(str::to_owned),
                arguments_delta: arguments.to_owned(),
            });
        }
    }
    Ok(())
}

/// Read `tool_calls[].index`, defaulting to 0 when absent.
fn tool_index(fields: &serde_json::Map<String, serde_json::Value>) -> Result<u16, LlmError> {
    match fields.get("index") {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .filter(|index| *index <= u64::from(u16::MAX))
            .map(|index| index as u16)
            .ok_or_else(|| LlmError::InvalidResponse("`tool_calls[].index` is not a u16".into())),
    }
}

/// Read an optional string field; a wrongly typed value is a protocol error.
fn optional_str<'a>(
    fields: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<&'a str>, LlmError> {
    match fields.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| LlmError::InvalidResponse(format!("`{key}` must be a string"))),
    }
}

/// Read a token-count field, defaulting to 0 when absent.
fn token_count(
    fields: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<u64, LlmError> {
    match fields.get(key) {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| LlmError::InvalidResponse(format!("`{key}` must be a number"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const HELLO_FRAMES: &[&str] = &[
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"He"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"llo"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":2}}"#,
        "data: [DONE]",
    ];

    fn feed_str(parser: &mut SseParser, text: &str) -> Vec<Result<StreamChunk, LlmError>> {
        parser.feed(text.as_bytes())
    }

    fn ok_text(chunks: &[Result<StreamChunk, LlmError>]) -> Vec<String> {
        chunks
            .iter()
            .filter_map(|chunk| match chunk {
                Ok(StreamChunk::TextDelta(text)) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn decodes_full_event_stream_in_one_feed() {
        let mut parser = SseParser::new();
        let mut out = feed_str(&mut parser, &format!("{}\n\n", HELLO_FRAMES.join("\n\n")));
        out.extend(parser.finish());

        assert_eq!(ok_text(&out), vec!["He", "llo"]);
        assert!(matches!(out[2], Ok(StreamChunk::Usage(_))));
        assert!(matches!(
            out[3],
            Ok(StreamChunk::Finish(FinishReason::Stop))
        ));
        assert_eq!(
            out.len(),
            4,
            "usage must sit immediately before finish, nothing after"
        );
    }

    #[test]
    fn reassembles_events_split_across_feeds() {
        let raw = format!("{}\n\n", HELLO_FRAMES.join("\n\n"));
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        for window in raw.as_bytes().chunks(7) {
            out.extend(parser.feed(window));
        }
        out.extend(parser.finish());

        assert_eq!(out.len(), 4);
        assert_eq!(ok_text(&out), vec!["He", "llo"]);
        assert!(matches!(
            out.last(),
            Some(Ok(StreamChunk::Finish(FinishReason::Stop)))
        ));
    }

    #[test]
    fn protocol_output_is_invariant_at_every_network_byte_split() {
        let raw = format!("{}\n\n", HELLO_FRAMES.join("\n\n"));
        let expected = {
            let mut parser = SseParser::new();
            let mut items = parser.feed(raw.as_bytes());
            items.extend(parser.finish());
            items.into_iter().map(Result::unwrap).collect::<Vec<_>>()
        };
        for split in 0..=raw.len() {
            let mut parser = SseParser::new();
            let mut items = parser.feed(&raw.as_bytes()[..split]);
            items.extend(parser.feed(&raw.as_bytes()[split..]));
            items.extend(parser.finish());
            assert_eq!(
                items.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
                expected,
                "split at byte {split}"
            );
        }
    }

    #[test]
    fn multi_data_sse_event_joins_before_provider_json_parsing() {
        let mut parser = SseParser::new();
        let out =
            parser.feed(b"data: {\"choices\":\ndata: [{\"delta\":{\"content\":\"joined\"}}]}\n\n");
        assert!(matches!(
            &out[..],
            [Ok(StreamChunk::TextDelta(text))] if text == "joined"
        ));
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let mut parser = SseParser::new();
        let mut out = feed_str(&mut parser, &format!("{}\r\n\r\n", HELLO_FRAMES[0]));
        out.extend(parser.finish());

        assert_eq!(ok_text(&out), vec!["He"]);
    }

    #[test]
    fn decodes_two_events_in_one_feed() {
        let mut parser = SseParser::new();
        let out = feed_str(
            &mut parser,
            &format!("{}\n\n{}\n\n", HELLO_FRAMES[0], HELLO_FRAMES[1]),
        );

        assert_eq!(ok_text(&out), vec!["He", "llo"]);
    }

    #[test]
    fn done_stops_all_further_output() {
        let mut parser = SseParser::new();
        feed_str(&mut parser, &format!("{}\n\n", HELLO_FRAMES.join("\n\n")));
        let after = feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n",
        );
        let tail = parser.finish();

        assert!(after.is_empty() && tail.is_empty());
    }

    #[test]
    fn usage_is_emitted_immediately_before_finish() {
        let mut parser = SseParser::new();
        // Usage arrives in a frame of its own, after the finish_reason frame.
        feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        );
        feed_str(
            &mut parser,
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6}}\n\n",
        );
        let out = feed_str(&mut parser, "data: [DONE]\n\n");

        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Ok(StreamChunk::Usage(_))));
        assert!(matches!(
            out[1],
            Ok(StreamChunk::Finish(FinishReason::ToolCalls))
        ));
    }

    #[test]
    fn malformed_json_yields_invalid_response_and_stream_survives() {
        let mut parser = SseParser::new();
        let mut out = feed_str(&mut parser, "data: {oops\n\n");
        out.extend(feed_str(&mut parser, &format!("{}\n\n", HELLO_FRAMES[1])));

        assert!(matches!(out[0], Err(LlmError::InvalidResponse(_))));
        assert_eq!(ok_text(&out), vec!["llo"]);
    }

    #[test]
    fn held_finish_flushes_at_end_of_body_without_done() {
        let mut parser = SseParser::new();
        let mut out = feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n",
        );
        out.extend(parser.finish());

        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Ok(StreamChunk::Usage(_))));
        assert!(matches!(
            out[1],
            Ok(StreamChunk::Finish(FinishReason::Length))
        ));
    }

    #[test]
    fn deltas_after_finish_reason_are_dropped() {
        let mut parser = SseParser::new();
        feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let out = feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n",
        );

        assert!(out.is_empty());
    }

    #[test]
    fn unknown_finish_reason_maps_to_stop() {
        let mut parser = SseParser::new();
        feed_str(
            &mut parser,
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
        );
        let out = parser.finish();

        assert!(matches!(
            out.as_slice(),
            [Ok(StreamChunk::Finish(FinishReason::Stop))]
        ));
    }

    #[test]
    fn unterminated_final_line_is_decoded_at_finish() {
        let mut parser = SseParser::new();
        let mut out = feed_str(&mut parser, &format!("{}\n\n", HELLO_FRAMES[0]));
        // Server closed without the trailing newline of the last frame.
        out.extend(feed_str(&mut parser, HELLO_FRAMES[1]));
        out.extend(parser.finish());

        assert_eq!(ok_text(&out), vec!["He", "llo"]);
    }

    #[test]
    fn non_data_lines_are_ignored() {
        let mut parser = SseParser::new();
        let mut out = feed_str(&mut parser, ": keepalive\nevent: ping\nid: 7\n\n");
        out.extend(feed_str(&mut parser, &format!("{}\n\n", HELLO_FRAMES[0])));

        assert_eq!(ok_text(&out), vec!["He"]);
    }
}
