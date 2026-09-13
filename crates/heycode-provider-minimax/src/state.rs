//! MiniMax interleaved reasoning/tool continuation state.
//!
//! MiniMax-M2 and later support *interleaved thinking*: the model reasons
//! between tool calls, and the chain of thought from earlier rounds is part of
//! the input to the next one. MiniMax states the requirement three times, once
//! per output format it offers
//! (<https://platform.minimax.io/docs/api-reference/text-openai-api> and
//! <https://platform.minimax.io/docs/api-reference/text-anthropic-api>):
//!
//! - OpenAI-compatible, native format — "In the message history, do not modify
//!   the `content` field. You must preserve the model's thinking content
//!   completely, i.e., `<think>reasoning_content</think>`".
//! - OpenAI-compatible, `reasoning_split` format — "To ensure that Interleaved
//!   Thinking functions properly and the model's chain of thought remains
//!   uninterrupted, the entire `response_message` — including the
//!   `reasoning_details` field — must be preserved in the message history".
//! - Anthropic-compatible — "Append the full `response.content` list to the
//!   message history (includes all content blocks: thinking/text/tool_use)".
//!
//! Which of the two OpenAI-compatible formats a turn arrives in is decided by
//! the `reasoning_split` request parameter, documented as an "Output-format
//! switch. When enabled, separates thinking content into `reasoning_content`
//! and `reasoning_details`"
//! (<https://platform.minimax.io/docs/api-reference/text-openai-api>). It does
//! not turn thinking on or off; that is the separate `thinking` parameter,
//! whose `type` can be `disabled` or `adaptive`. MiniMax-M3 defaults differ by
//! protocol: Chat thinking is on when omitted, while Messages thinking is off;
//! explicit `adaptive` enables it on both. M2.x thinking cannot be disabled.
//!
//! Verbatim is therefore the contract, not an implementation convenience. A
//! generic Chat Completions assistant message reconstructed field by field
//! keeps whatever fields that struct happens to name; MiniMax's requirement is
//! that the turn arrive back *unchanged*. [`MiniMaxStateRoute::capture`] stores
//! the wire message as it came off the wire, and [`MiniMaxStateRoute::replay`]
//! hands the same value back. Neither builds a message.
//!
//! What this module does add is refusal. A turn that calls a tool without the
//! reasoning its dialect transports cannot be replayed into an interleaved
//! model without breaking the chain, and a `<think>` segment that was never
//! closed is a truncated chain wearing a complete one's shape. Both are
//! refused at capture *and* at replay: capture cannot see a log written by an
//! older build, and replay cannot see a response the parser mangled.

use std::collections::BTreeSet;
use std::fmt;

use heycode_core::{ProviderProtocol, ProviderStateError, ProviderStateItem, ProviderStateKind};
use serde_json::{Map, Value};

use crate::endpoint::{MiniMaxApiFamily, MiniMaxRegion};
use crate::plan::{MiniMaxPlan, MiniMaxPlanId};
use crate::profile::MiniMaxProfile;

/// Opening marker of MiniMax's native-format thinking segment.
const THINK_OPEN: &str = "<think>";
/// Closing marker of MiniMax's native-format thinking segment.
const THINK_CLOSE: &str = "</think>";
/// The one `reasoning_details` entry type MiniMax documents.
///
/// Source: <https://platform.minimax.io/docs/guides/text-m3-function-call>
/// shows `{"type": "reasoning.text", "id": …, "format": "MiniMax-response-v1",
/// "index": 0, "text": …}`.
const REASONING_TEXT_DETAIL: &str = "reasoning.text";

/// Which output format a MiniMax assistant turn arrived in.
///
/// The enum is closed: a fourth format must break every consumer that decides
/// per dialect rather than being absorbed by a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxStateDialect {
    /// OpenAI-compatible Chat Completions with `reasoning_split` off — MiniMax's
    /// native format, where thinking is inside `content` between `<think>` and
    /// `</think>`.
    ChatThinkTags,
    /// OpenAI-compatible Chat Completions with `reasoning_split: true` —
    /// thinking is lifted out of `content` into `reasoning_content` and
    /// `reasoning_details`.
    ChatReasoningSplit,
    /// Anthropic-compatible Messages — thinking arrives as `thinking` blocks
    /// inside the assistant `content` array.
    AnthropicThinkingBlocks,
}

impl MiniMaxStateDialect {
    /// Every documented dialect, in a stable order.
    pub const ALL: [Self; 3] = [
        Self::ChatThinkTags,
        Self::ChatReasoningSplit,
        Self::AnthropicThinkingBlocks,
    ];

    /// MiniMax API family this dialect is served by.
    #[must_use]
    pub const fn family(self) -> MiniMaxApiFamily {
        match self {
            Self::ChatThinkTags | Self::ChatReasoningSplit => MiniMaxApiFamily::OpenAiCompatible,
            Self::AnthropicThinkingBlocks => MiniMaxApiFamily::AnthropicCompatible,
        }
    }

    /// heycode protocol this dialect replays into.
    #[must_use]
    pub const fn protocol(self) -> ProviderProtocol {
        self.family().protocol()
    }

    /// Shared provider-state kind this dialect is stored as.
    #[must_use]
    pub const fn state_kind(self) -> ProviderStateKind {
        match self.family() {
            MiniMaxApiFamily::OpenAiCompatible => ProviderStateKind::ChatAssistantMessage,
            MiniMaxApiFamily::AnthropicCompatible => ProviderStateKind::AnthropicMessage,
        }
    }

    /// Value of MiniMax's `reasoning_split` request parameter that produces
    /// this dialect, where the parameter applies.
    ///
    /// [`None`] for the Anthropic-compatible family: `reasoning_split` is a
    /// Chat Completions output-format switch and MiniMax documents no
    /// equivalent on the Messages route, whose thinking blocks are the format.
    #[must_use]
    pub const fn reasoning_split(self) -> Option<bool> {
        match self {
            Self::ChatThinkTags => Some(false),
            Self::ChatReasoningSplit => Some(true),
            Self::AnthropicThinkingBlocks => None,
        }
    }

    /// Short human label naming the wire format, not the enum.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ChatThinkTags => "OpenAI-compatible `<think>`",
            Self::ChatReasoningSplit => "OpenAI-compatible `reasoning_split`",
            Self::AnthropicThinkingBlocks => "Anthropic-compatible thinking-block",
        }
    }
}

impl fmt::Display for MiniMaxStateDialect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Whether MiniMax guarantees every turn of a request carries reasoning.
///
/// The requirement is conditioned on what the request asked for, never on the
/// model's spelling. MiniMax documents `thinking.type` as `disabled` or
/// `adaptive`; Chat and Messages have different MiniMax-M3 omission defaults,
/// while M2.x thinking cannot be disabled
/// (<https://platform.minimax.io/docs/api-reference/text-openai-api> and
/// <https://platform.minimax.io/docs/api-reference/text-anthropic-api>).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxReasoningGuarantee {
    /// MiniMax returns reasoning for every turn: `thinking` omitted, or any
    /// M2.x model, which cannot disable it. A tool-call turn that carries none
    /// is then a chain that was dropped, not one the model declined to produce.
    Always,
    /// MiniMax may legitimately return a turn with no reasoning:
    /// an explicitly disabled MiniMax-M3 request, an omitted Messages thinking
    /// setting for MiniMax-M3, or another route whose reasoning presence is not
    /// proven. Explicit `adaptive` enables thinking and should use
    /// [`Self::Always`]; this conservative variant never claims otherwise.
    /// Because absence cannot be distinguished from a dropped chain here, it
    /// is admitted rather than misreported as complete reasoning.
    Optional,
}

/// Part of a MiniMax turn that failed the completeness rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxTurnPart {
    /// The assistant `content` field itself.
    Content,
    /// One entry of the Chat `tool_calls` array.
    ToolCall,
    /// The Chat `reasoning_content` field.
    ReasoningContent,
    /// One entry of the Chat `reasoning_details` array.
    ReasoningDetail,
    /// One block of the Anthropic-compatible `content` array.
    ContentBlock,
}

impl MiniMaxTurnPart {
    /// Short human label for this part.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Content => "assistant content",
            Self::ToolCall => "tool call",
            Self::ReasoningContent => "reasoning content",
            Self::ReasoningDetail => "reasoning detail",
            Self::ContentBlock => "content block",
        }
    }
}

impl fmt::Display for MiniMaxTurnPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Closed reasons a MiniMax turn cannot be captured or replayed.
///
/// No variant carries turn content: a refusal names the MiniMax rule and the
/// field that broke it, never the model's words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MiniMaxStateError {
    /// The value is not a MiniMax assistant message.
    #[error("a MiniMax {dialect} turn must be a JSON object whose `role` is `assistant`")]
    NotAssistantMessage {
        /// Dialect the turn was offered as.
        dialect: MiniMaxStateDialect,
    },
    /// The turn carries a field belonging to the other output format.
    #[error(
        "a MiniMax {dialect} turn cannot carry `{field}`, which MiniMax returns only under the other `reasoning_split` setting"
    )]
    DialectMismatch {
        /// Dialect the turn was offered as.
        dialect: MiniMaxStateDialect,
        /// Field that belongs to the other format.
        field: &'static str,
    },
    /// A `<think>` segment opens without closing, closes without opening, or
    /// nests — in every case the chain of thought is truncated.
    #[error(
        "a MiniMax `<think>` segment in `content` is unbalanced, so the thinking chain is truncated"
    )]
    UnbalancedThinking,
    /// The turn calls tools but carries none of the reasoning MiniMax requires
    /// to be replayed with it.
    #[error(
        "a MiniMax {dialect} tool-call turn must carry its own thinking; replaying it without one breaks interleaved thinking"
    )]
    MissingReasoning {
        /// Dialect the turn was offered as.
        dialect: MiniMaxStateDialect,
    },
    /// A part of the turn cannot be replayed as it stands.
    #[error(
        "a MiniMax {part} is missing or malformed at `{field}`, so the turn cannot be replayed"
    )]
    Incomplete {
        /// Part of the turn that failed.
        part: MiniMaxTurnPart,
        /// Stable field name inside that part.
        field: &'static str,
    },
    /// The stored state belongs to MiniMax's other product.
    #[error("this state was recorded for MiniMax route `{recorded}`, not `{expected}`")]
    ForeignRoute {
        /// Registry name the state was recorded under.
        recorded: String,
        /// Registry name this route replays.
        expected: &'static str,
    },
    /// The stored state belongs to another MiniMax model.
    #[error("this MiniMax state was recorded for another model")]
    ForeignModel,
    /// The stored state is not the shape this dialect replays.
    #[error(
        "this state is {kind:?} on {protocol:?}, which is not the MiniMax {dialect} replay shape"
    )]
    ForeignShape {
        /// Dialect that was asked to replay it.
        dialect: MiniMaxStateDialect,
        /// Protocol the state was recorded under.
        protocol: ProviderProtocol,
        /// Kind the state was recorded under.
        kind: ProviderStateKind,
    },
    /// The shared provider-state vocabulary refused the turn.
    #[error("the shared provider-state vocabulary refused this MiniMax turn: {message}")]
    Vocabulary {
        /// Redacted vocabulary failure text.
        message: String,
    },
    /// The route names no usable model.
    #[error("a MiniMax state route needs a non-blank, trimmed model id")]
    InvalidModel,
}

/// One MiniMax assistant-continuation route: a product, a dialect, a model and
/// the reasoning guarantee the request selected.
///
/// The plan is a type parameter inherited from [`MiniMaxProfile`], so a turn
/// recorded against one MiniMax product cannot be replayed by a route built for
/// the other — the same boundary PMM01 draws around credentials, extended to
/// the session log.
#[derive(Clone, PartialEq, Eq)]
pub struct MiniMaxStateRoute<P: MiniMaxPlan> {
    profile: MiniMaxProfile<P>,
    dialect: MiniMaxStateDialect,
    model: String,
    reasoning: MiniMaxReasoningGuarantee,
}

impl<P: MiniMaxPlan> MiniMaxStateRoute<P> {
    /// Bind a product, dialect, model and reasoning guarantee.
    ///
    /// # Errors
    /// A blank or untrimmed model id is refused before it can become the
    /// identity of a durable state item.
    pub fn new(
        profile: MiniMaxProfile<P>,
        dialect: MiniMaxStateDialect,
        model: impl Into<String>,
        reasoning: MiniMaxReasoningGuarantee,
    ) -> Result<Self, MiniMaxStateError> {
        let model = model.into();
        if model.is_empty() || model.trim() != model {
            return Err(MiniMaxStateError::InvalidModel);
        }
        Ok(Self {
            profile,
            dialect,
            model,
            reasoning,
        })
    }

    /// Product this route bills.
    #[must_use]
    pub const fn plan(&self) -> MiniMaxPlanId {
        P::ID
    }

    /// Deployment the captured turns came from.
    #[must_use]
    pub const fn region(&self) -> MiniMaxRegion {
        self.profile.region()
    }

    /// Output format this route captures and replays.
    #[must_use]
    pub const fn dialect(&self) -> MiniMaxStateDialect {
        self.dialect
    }

    /// Canonical model id recorded on captured state.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Reasoning guarantee the request selected.
    #[must_use]
    pub const fn reasoning_guarantee(&self) -> MiniMaxReasoningGuarantee {
        self.reasoning
    }

    /// Store one complete MiniMax assistant turn verbatim.
    ///
    /// `message` is the assistant message exactly as MiniMax returned it — the
    /// OpenAI-compatible `response_message`, or `{"role":"assistant","content":
    /// <response.content>}` on the Anthropic-compatible route. Nothing is
    /// rewritten, reordered or dropped.
    ///
    /// # Errors
    /// A turn that is not an assistant message, carries the other output
    /// format's fields, has a truncated thinking chain, calls tools without the
    /// reasoning MiniMax requires back, or has an unreplayable tool call,
    /// reasoning detail or content block.
    pub fn capture(&self, message: Value) -> Result<ProviderStateItem, MiniMaxStateError> {
        self.check(&message)?;
        ProviderStateItem::new(
            P::ID.registry_name(),
            &self.model,
            self.dialect.protocol(),
            self.dialect.state_kind(),
            message,
        )
        .map_err(vocabulary)
    }

    /// Re-check a stored turn and hand back the exact value to replay.
    ///
    /// Capture-time checking is not enough on its own: a v1 log written by an
    /// older build, or by a build that captured through a generic Chat
    /// Completions shape, can hold a tool-call turn whose thinking never
    /// survived. Replay is the last point at which that can be caught before
    /// request bytes leave.
    ///
    /// # Errors
    /// Everything [`Self::capture`] refuses, plus state recorded for MiniMax's
    /// other product or in another dialect's shape.
    pub fn replay<'state>(
        &self,
        state: &'state ProviderStateItem,
    ) -> Result<&'state Value, MiniMaxStateError> {
        state.validate().map_err(vocabulary)?;
        if state.provider() != P::ID.registry_name() {
            return Err(MiniMaxStateError::ForeignRoute {
                recorded: state.provider().to_owned(),
                expected: P::ID.registry_name(),
            });
        }
        if state.model() != self.model {
            return Err(MiniMaxStateError::ForeignModel);
        }
        if state.protocol() != self.dialect.protocol() || state.kind() != self.dialect.state_kind()
        {
            return Err(MiniMaxStateError::ForeignShape {
                dialect: self.dialect,
                protocol: state.protocol(),
                kind: state.kind(),
            });
        }
        self.check(state.data())?;
        Ok(state.data())
    }

    fn check(&self, message: &Value) -> Result<(), MiniMaxStateError> {
        let object = message
            .as_object()
            .filter(|object| object.get("role").and_then(Value::as_str) == Some("assistant"))
            .ok_or(MiniMaxStateError::NotAssistantMessage {
                dialect: self.dialect,
            })?;
        let turn = match self.dialect {
            MiniMaxStateDialect::ChatThinkTags => think_tag_turn(object)?,
            MiniMaxStateDialect::ChatReasoningSplit => reasoning_split_turn(object)?,
            MiniMaxStateDialect::AnthropicThinkingBlocks => thinking_block_turn(object)?,
        };
        if turn.tools && !turn.reasoning && self.reasoning == MiniMaxReasoningGuarantee::Always {
            return Err(MiniMaxStateError::MissingReasoning {
                dialect: self.dialect,
            });
        }
        Ok(())
    }
}

/// What one checked turn contains, reduced to the two facts the interleaved
/// thinking rule needs.
struct Turn {
    reasoning: bool,
    tools: bool,
}

/// Read a native-format turn: thinking lives inside `content`.
fn think_tag_turn(object: &Map<String, Value>) -> Result<Turn, MiniMaxStateError> {
    // `reasoning_split` was off, so MiniMax returns neither split field. One
    // that is present means the turn was produced under the other setting, and
    // then `content` carries no thinking to find — a completeness check on it
    // would pass or fail for the wrong reason.
    for field in ["reasoning_content", "reasoning_details"] {
        if object.contains_key(field) {
            return Err(MiniMaxStateError::DialectMismatch {
                dialect: MiniMaxStateDialect::ChatThinkTags,
                field,
            });
        }
    }
    let reasoning = match object.get("content") {
        None | Some(Value::Null) => false,
        Some(Value::String(content)) => complete_think_segment(content)?,
        Some(_) => return Err(incomplete(MiniMaxTurnPart::Content, "content")),
    };
    Ok(Turn {
        reasoning,
        tools: chat_tool_calls(object)?,
    })
}

/// Read a `reasoning_split` turn: thinking lives beside an untouched `content`.
fn reasoning_split_turn(object: &Map<String, Value>) -> Result<Turn, MiniMaxStateError> {
    if let Some(value) = object.get("reasoning_content")
        && !value.as_str().is_some_and(|text| !text.trim().is_empty())
    {
        return Err(incomplete(
            MiniMaxTurnPart::ReasoningContent,
            "reasoning_content",
        ));
    }
    match object.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(content)) => {
            if content.contains(THINK_OPEN) || content.contains(THINK_CLOSE) {
                return Err(MiniMaxStateError::DialectMismatch {
                    dialect: MiniMaxStateDialect::ChatReasoningSplit,
                    field: "content",
                });
            }
        }
        Some(_) => return Err(incomplete(MiniMaxTurnPart::Content, "content")),
    }
    Ok(Turn {
        reasoning: reasoning_details(object)?,
        tools: chat_tool_calls(object)?,
    })
}

/// Read an Anthropic-compatible turn: thinking is a block of `content`.
fn thinking_block_turn(object: &Map<String, Value>) -> Result<Turn, MiniMaxStateError> {
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| incomplete(MiniMaxTurnPart::Content, "content"))?;
    let mut turn = Turn {
        reasoning: false,
        tools: false,
    };
    let mut tool_ids = BTreeSet::new();
    for block in content {
        let block = block
            .as_object()
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ContentBlock, "type"))?;
        let kind = nonblank(block, "type")
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ContentBlock, "type"))?;
        match kind {
            // MiniMax names `thinking`, `text` and `tool_use` as the blocks its
            // Messages route returns. Any block type MiniMax adds later is
            // opaque continuation material and is kept verbatim.
            "text" => {
                // An empty string is a legal, if unusual, answer block; a
                // missing or non-string `text` is a dropped one.
                if !block.get("text").is_some_and(Value::is_string) {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "text"));
                }
            }
            "thinking" => {
                if nonblank(block, "thinking").is_none() {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "thinking"));
                }
                // The shared Messages adapter treats the opaque signature as
                // required continuation state. A block without it cannot pass
                // the production replay boundary, so it is incomplete here too.
                if nonblank(block, "signature").is_none() {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "signature"));
                }
                turn.reasoning = true;
            }
            "tool_use" => {
                let Some(id) = nonblank(block, "id") else {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "id"));
                };
                if !tool_ids.insert(id) {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "id"));
                }
                if nonblank(block, "name").is_none() {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "name"));
                }
                if !block.get("input").is_some_and(Value::is_object) {
                    return Err(incomplete(MiniMaxTurnPart::ContentBlock, "input"));
                }
                turn.tools = true;
            }
            _ => {}
        }
    }
    Ok(turn)
}

/// Whether the turn calls any tool, refusing one that cannot be replayed.
fn chat_tool_calls(object: &Map<String, Value>) -> Result<bool, MiniMaxStateError> {
    let Some(value) = object.get("tool_calls").filter(|value| !value.is_null()) else {
        return Ok(false);
    };
    let calls = value
        .as_array()
        .ok_or_else(|| incomplete(MiniMaxTurnPart::ToolCall, "tool_calls"))?;
    let mut ids = BTreeSet::new();
    for call in calls {
        let call = call
            .as_object()
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ToolCall, "tool_calls"))?;
        let Some(id) = nonblank(call, "id") else {
            return Err(incomplete(MiniMaxTurnPart::ToolCall, "id"));
        };
        if !ids.insert(id) {
            return Err(incomplete(MiniMaxTurnPart::ToolCall, "id"));
        }
        if call.get("type").and_then(Value::as_str) != Some("function") {
            return Err(incomplete(MiniMaxTurnPart::ToolCall, "type"));
        }
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ToolCall, "function"))?;
        if nonblank(function, "name").is_none() {
            return Err(incomplete(MiniMaxTurnPart::ToolCall, "name"));
        }
        // MiniMax sends arguments as JSON text; a call taking none sends `{}`.
        // An empty string is admitted rather than second-guessed, because the
        // arguments are MiniMax's to shape. A missing or non-string
        // `arguments` cannot be replayed at all.
        if function.get("arguments").and_then(Value::as_str).is_none() {
            return Err(incomplete(MiniMaxTurnPart::ToolCall, "arguments"));
        }
    }
    Ok(!calls.is_empty())
}

/// Whether the turn carries split reasoning, refusing a truncated entry.
fn reasoning_details(object: &Map<String, Value>) -> Result<bool, MiniMaxStateError> {
    let Some(value) = object
        .get("reasoning_details")
        .filter(|value| !value.is_null())
    else {
        return Ok(false);
    };
    let details = value
        .as_array()
        .ok_or_else(|| incomplete(MiniMaxTurnPart::ReasoningDetail, "reasoning_details"))?;
    for detail in details {
        let detail = detail
            .as_object()
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ReasoningDetail, "type"))?;
        let kind = nonblank(detail, "type")
            .ok_or_else(|| incomplete(MiniMaxTurnPart::ReasoningDetail, "type"))?;
        // `reasoning.text` is the only entry type MiniMax documents, and its
        // `text` is the chain of thought itself. Any other type is opaque
        // continuation material: replayed unchanged rather than dissected.
        if kind == REASONING_TEXT_DETAIL && nonblank(detail, "text").is_none() {
            return Err(incomplete(MiniMaxTurnPart::ReasoningDetail, "text"));
        }
    }
    Ok(!details.is_empty())
}

/// Whether `content` closes every thinking segment it opens, and whether any
/// closed segment actually carries thinking.
fn complete_think_segment(content: &str) -> Result<bool, MiniMaxStateError> {
    let mut rest = content;
    let mut complete = false;
    while let Some(open) = rest.find(THINK_OPEN) {
        if rest.find(THINK_CLOSE).is_some_and(|close| close < open) {
            return Err(MiniMaxStateError::UnbalancedThinking);
        }
        let after = &rest[open + THINK_OPEN.len()..];
        let Some(close) = after.find(THINK_CLOSE) else {
            return Err(MiniMaxStateError::UnbalancedThinking);
        };
        let (inner, tail) = after.split_at(close);
        if inner.contains(THINK_OPEN) {
            return Err(MiniMaxStateError::UnbalancedThinking);
        }
        if !inner.trim().is_empty() {
            complete = true;
        }
        rest = &tail[THINK_CLOSE.len()..];
    }
    if rest.contains(THINK_CLOSE) {
        return Err(MiniMaxStateError::UnbalancedThinking);
    }
    Ok(complete)
}

fn nonblank<'value>(object: &'value Map<String, Value>, key: &str) -> Option<&'value str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

const fn incomplete(part: MiniMaxTurnPart, field: &'static str) -> MiniMaxStateError {
    MiniMaxStateError::Incomplete { part, field }
}

fn vocabulary(error: ProviderStateError) -> MiniMaxStateError {
    MiniMaxStateError::Vocabulary {
        message: error.to_string(),
    }
}

/// Route identity only. A state route holds no secret; the plan and region it
/// names are exactly what a diagnostic needs to explain a refusal.
impl<P: MiniMaxPlan> fmt::Debug for MiniMaxStateRoute<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiniMaxStateRoute")
            .field("plan", &P::ID.registry_name())
            .field("region", &self.profile.region())
            .field("dialect", &self.dialect)
            .field("model", &self.model)
            .field("reasoning", &self.reasoning)
            .finish()
    }
}
