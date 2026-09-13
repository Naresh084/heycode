//! Human-only execution telemetry, separate from ordered durable tool records.
//!
//! Tool records preserve provider call/result order. These events record when
//! admitted work actually runs, including overlapping calls that have not yet
//! reached the durable commit cursor. They never become model history.

/// Actual execution phase of one stable model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionPhase {
    /// Waiting on tool policy/admission; execution has not begun.
    Admitting,
    /// Approved execution began.
    Running,
    /// Execution returned; its durable result may still wait behind a predecessor.
    Finished {
        /// Whether execution returned a successful tool outcome.
        ok: bool,
    },
    /// The final result was committed durably, including denied/not-started results.
    Committed {
        /// Whether the committed result is successful.
        ok: bool,
    },
}

/// Owner-aware frontends subscribe to the exact Agent's UI bus for these facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionEvent {
    /// Correlation with the provider call and durable result.
    pub call_id: heycode_core::CallId,
    /// Tool's registered name.
    pub name: String,
    /// Actual execution phase.
    pub phase: ToolExecutionPhase,
    /// Wall-clock milliseconds at the source; never guessed from render time.
    pub timestamp_ms: u64,
}

impl ToolExecutionEvent {
    pub(crate) fn new(call_id: &str, name: &str, phase: ToolExecutionPhase) -> Self {
        Self {
            call_id: heycode_core::CallId::from_raw(call_id.to_owned()),
            name: name.to_owned(),
            phase,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                }),
        }
    }
}
