//! Active-turn command timing policy.

use heycode_agent::CommandTiming;

/// What the shell must do with one command submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDisposition {
    /// Start a command task immediately.
    ExecuteNow,
    /// Preserve exact text and run after active work settles.
    Queue,
    /// Ask before cancelling active work, then queue behind settlement.
    ConfirmInterrupt,
}

/// Resolve CMD01 metadata into shell behavior.
#[must_use]
pub const fn route_command(timing: CommandTiming, active_turn: bool) -> CommandDisposition {
    if !active_turn {
        return CommandDisposition::ExecuteNow;
    }
    match timing {
        CommandTiming::Immediate => CommandDisposition::ExecuteNow,
        CommandTiming::Queued | CommandTiming::ModelScheduling => CommandDisposition::Queue,
        CommandTiming::Interrupting => CommandDisposition::ConfirmInterrupt,
    }
}
