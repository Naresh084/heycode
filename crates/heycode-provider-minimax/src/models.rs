//! Maintained normalization for MiniMax's current language models.
//!
//! Neither MiniMax list endpoint publishes limits or capabilities — the
//! OpenAI-compatible row is `{id, object, created, owned_by}` and the
//! Anthropic-compatible row is `{id, created_at, display_name, type}`. Anything
//! beyond identity therefore comes from MiniMax's documentation, per id, with
//! the page cited beside it. An id outside this table keeps its identity and
//! nothing else, exactly like an unknown row from any other catalog.
//!
//! Because this table — not the wire — owns the normalized shape of a current
//! model, both list endpoints necessarily agree about them. That is a
//! structural property, not an observation that happens to hold today.

use heycode_llm::{CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle};

/// Current low-latency/frontier MiniMax model id.
pub const MINIMAX_M3: &str = "MiniMax-M3";

/// Documented context window shared by the M2 generation.
///
/// Source: <https://platform.minimax.io/docs/guides/text-generation> lists
/// `204,800` for every M2-series model. `models-intro` writes the same limit
/// as the rounded prose "200k tokens"; the exact figure is used here.
const M2_SERIES_CONTEXT_WINDOW: u64 = 204_800;

/// Documented MiniMax-M3 context window.
///
/// Sources: <https://platform.minimax.io/docs/guides/text-generation> lists
/// `1,000,000`; <https://platform.minimax.io/docs/token-plan/codex> configures
/// `model_context_window = 1000000`; and
/// <https://platform.minimax.io/docs/token-plan/claude-code> sets
/// `CLAUDE_CODE_AUTO_COMPACT_WINDOW` to `"1000000"`.
const M3_CONTEXT_WINDOW: u64 = 1_000_000;

/// One MiniMax language model heycode has documentary evidence for.
struct CurrentModel {
    id: &'static str,
    context_window: u64,
    image_input: CapabilitySupport,
}

/// MiniMax's current and legacy language models, in documentation order.
///
/// `image_input` is `Supported` only for MiniMax-M3, which two MiniMax pages
/// describe as accepting image input by id:
/// <https://platform.minimax.io/docs/api-reference/text-openai-api>
/// ("OpenAI-compatible Chat Completions support text, image, and video input
/// for `MiniMax-M3`") and
/// <https://platform.minimax.io/docs/guides/text-generation> ("Supports
/// multimodal inputs, including text, images, and video").
///
/// Every other capability stays `Unknown` for every model. MiniMax documents a
/// `tools` request parameter on the endpoint, but an endpoint parameter is
/// protocol evidence, not per-model capability evidence, and the marketing
/// feature bullets on `models-intro` ("agentic reasoning", "Function calling")
/// are not capability statements either. PMM03 owns proving tool and reasoning
/// state against real traffic.
const CURRENT_MODELS: &[CurrentModel] = &[
    CurrentModel {
        id: MINIMAX_M3,
        context_window: M3_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Supported,
    },
    CurrentModel {
        id: "MiniMax-M2.7",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2.7-highspeed",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2.5",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2.5-highspeed",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2.1",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2.1-highspeed",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
    CurrentModel {
        id: "MiniMax-M2",
        context_window: M2_SERIES_CONTEXT_WINDOW,
        image_input: CapabilitySupport::Unknown,
    },
];

/// Every model id this crate has documentary evidence for.
pub fn documented_model_ids() -> impl Iterator<Item = &'static str> {
    CURRENT_MODELS.iter().map(|model| model.id)
}

/// Normalize one discovered model into the shared catalog vocabulary.
///
/// `wire_display_name` is the name the Anthropic-compatible list publishes and
/// the OpenAI-compatible list does not. It is used only for ids outside the
/// maintained table: a documented model takes its display name from the table
/// so the two endpoints cannot drift apart.
#[must_use]
pub fn normalize_model(id: String, wire_display_name: Option<String>) -> ModelDescriptor {
    let Some(model) = CURRENT_MODELS.iter().find(|model| model.id == id) else {
        let mut descriptor = ModelDescriptor::unknown(id);
        if let Some(display_name) = wire_display_name {
            descriptor.display_name = display_name;
        }
        return descriptor;
    };
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.image_input = model.image_input;
    ModelDescriptor {
        display_name: model.id.to_owned(),
        id,
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(model.context_window),
        // MiniMax publishes no exact maximum-output figure for any language
        // model. `models-intro` writes "128k tokens (including CoT)" in prose
        // for MiniMax-M2 alone, which resolves to neither 128,000 nor 131,072
        // without guessing, so no model claims an output cap.
        max_output_tokens: None,
        // MiniMax publishes no deprecation notice or retirement instant for any
        // language model. Its documentation groups models under "Current" and
        // "Legacy" headings, but a docs heading is not a lifecycle claim and a
        // wrong `Retired` would make a working model unselectable.
        lifecycle: ModelLifecycle::unknown(),
        capabilities,
        // MiniMax publishes prices on its pricing pages, not through either
        // list endpoint. Transcribing a docs table into a catalog generation
        // would make a stale price look like live evidence.
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        reasoning: None,
    }
}
