//! Estimated growth of retained request material, independent of billing.

use crate::SessionEventKind;

/// A contribution since the last complete request-envelope measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetainedContextGrowth {
    /// Estimated count of locally readable material retained for another request.
    pub tokens: u64,
    /// A provider-owned or media contributor still needs a request-level counter.
    pub uncounted: bool,
}

/// Estimate only retained canonical text, tool arguments and tool results.
/// Billing output and display-only reasoning are deliberately excluded.
/// Provider state/audio remain explicitly uncounted; this never tokenizes
/// ciphertext or treats billing usage as retained context size.
#[must_use]
pub fn retained_context_growth(event: &SessionEventKind) -> Option<RetainedContextGrowth> {
    let (tokens, uncounted) = match event {
        SessionEventKind::AssistantMessage {
            content,
            tool_calls,
            ..
        } => (
            (content
                .len()
                .saturating_add(tool_calls.as_ref().map_or(0, |calls| {
                    serde_json::to_vec(calls).map_or(0, |bytes| bytes.len())
                })) as u64)
                .div_ceil(4),
            false,
        ),
        SessionEventKind::ToolResult { content, .. } => ((content.len() as u64).div_ceil(4), false),
        SessionEventKind::AssistantProviderItem { .. }
        | SessionEventKind::AssistantAudio { .. } => (0, true),
        _ => return None,
    };
    Some(RetainedContextGrowth { tokens, uncounted })
}
