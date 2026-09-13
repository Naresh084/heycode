//! Client-side assembly of streamed chunks into one final message state.
//!
//! The agent loop consumes chunks live; `accumulate` is the reference fold
//! for tests, retries, and non-streaming consumers.

use std::collections::BTreeMap;

use crate::vocab::{FinishReason, StreamChunk, TokenUsage};

/// Fully assembled result of one streamed completion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Accumulated {
    /// Concatenated text content.
    pub content: String,
    /// Concatenated reasoning trace (reasoning models; empty otherwise).
    pub reasoning: String,
    /// Assembled tool calls ordered by stream index.
    pub tool_calls: Vec<AccumulatedToolCall>,
    /// Terminal reason, present once the stream carried a `Finish`.
    pub finish: Option<FinishReason>,
    /// Token usage, present once the stream carried a `Usage`.
    pub usage: Option<TokenUsage>,
}

/// One tool call rebuilt from its [`StreamChunk::ToolCallDelta`] fragments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccumulatedToolCall {
    /// Call id from the first fragment; empty if the stream never sent one.
    pub id: String,
    /// Tool name from the first fragment; empty if the stream never sent one.
    pub name: String,
    /// Concatenated raw JSON arguments text.
    pub arguments: String,
}

/// Fold `chunks` into one [`Accumulated`]. Tool-call fragments merge by
/// index order; argument fragments join in arrival order; later `id`/`name`
/// values overwrite earlier ones; `Usage`/`Finish` keep the final occurrence.
#[must_use]
pub fn accumulate(chunks: impl IntoIterator<Item = StreamChunk>) -> Accumulated {
    let mut acc = Accumulated::default();
    let mut tools: BTreeMap<u16, PartialToolCall> = BTreeMap::new();

    for chunk in chunks {
        match chunk {
            StreamChunk::TextDelta(text) => acc.content.push_str(&text),
            StreamChunk::ReasoningDelta(reasoning) => acc.reasoning.push_str(&reasoning),
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                let tool = tools.entry(index).or_default();
                if id.is_some() {
                    tool.id = id;
                }
                if name.is_some() {
                    tool.name = name;
                }
                tool.arguments.push_str(&arguments_delta);
            }
            StreamChunk::Usage(usage) => acc.usage = Some(usage),
            StreamChunk::Finish(finish) => acc.finish = Some(finish),
        }
    }

    acc.tool_calls = tools
        .into_values()
        .map(|tool| AccumulatedToolCall {
            id: tool.id.unwrap_or_default(),
            name: tool.name.unwrap_or_default(),
            arguments: tool.arguments,
        })
        .collect();
    acc
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn call(index: u16, id: Option<&str>, name: Option<&str>, args: &str) -> StreamChunk {
        StreamChunk::ToolCallDelta {
            index,
            id: id.map(str::to_owned),
            name: name.map(str::to_owned),
            arguments_delta: args.to_owned(),
        }
    }

    #[test]
    fn merges_tool_calls_by_index_and_joins_arguments() {
        let acc = accumulate(vec![
            call(1, Some("call_b"), Some("write"), "{\"path\":\"a.rs\""),
            call(0, Some("call_a"), Some("read"), "{\"file\""),
            call(0, None, None, ":\"x\"}"),
            call(1, None, None, "}"),
        ]);
        assert_eq!(acc.tool_calls.len(), 2);
        assert_eq!(acc.tool_calls[0].id, "call_a");
        assert_eq!(acc.tool_calls[0].name, "read");
        assert_eq!(acc.tool_calls[0].arguments, "{\"file\":\"x\"}");
        assert_eq!(acc.tool_calls[1].id, "call_b");
        assert_eq!(acc.tool_calls[1].arguments, "{\"path\":\"a.rs\"}");
    }

    #[test]
    fn concatenates_text_reasoning_and_keeps_usage_finish() {
        let acc = accumulate(vec![
            StreamChunk::ReasoningDelta("thin".into()),
            StreamChunk::ReasoningDelta("king".into()),
            StreamChunk::TextDelta("he".into()),
            StreamChunk::TextDelta("llo".into()),
            StreamChunk::Usage(TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 5,
            }),
            StreamChunk::Finish(FinishReason::Stop),
        ]);
        assert_eq!(acc.reasoning, "thinking");
        assert_eq!(acc.content, "hello");
        assert_eq!(
            acc.usage,
            Some(TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 5
            })
        );
        assert_eq!(acc.finish, Some(FinishReason::Stop));
        assert!(acc.tool_calls.is_empty());
    }
}
