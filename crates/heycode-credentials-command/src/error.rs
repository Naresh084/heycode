//! Secret-free command provider failures.

/// Command credential configuration/execution failure. Variants deliberately
/// exclude argv, stdout, stderr and OS error text.
#[derive(Debug, thiserror::Error)]
pub enum CommandCredentialError {
    /// A safe reference claimed two command specs.
    #[error("command credential reference `{reference}` is configured more than once")]
    DuplicateReference {
        /// Non-secret reference.
        reference: String,
    },
    /// Spec fields violated caps/shape.
    #[error("invalid command credential spec for `{reference}`: {reason}")]
    InvalidSpec {
        /// Non-secret reference.
        reference: String,
        /// Fixed safe reason.
        reason: &'static str,
    },
    /// Provider id construction failed.
    #[error("command credential provider identity is invalid")]
    InvalidProviderId,
    /// Worker thread could not start or panicked.
    #[error("command credential worker failed")]
    Worker,
    /// Private Tokio runtime could not start.
    #[error("command credential runtime failed")]
    Runtime,
    /// Process could not be spawned.
    #[error("command credential could not start")]
    Spawn,
    /// Deadline elapsed; direct child was killed and reaped.
    #[error("command credential timed out")]
    Timeout,
    /// Process exited nonzero.
    #[error("command credential exited unsuccessfully")]
    UnsuccessfulExit,
    /// Stdout exceeded the fixed cap.
    #[error("command credential exceeded the output limit")]
    OutputTooLarge,
    /// Stdout pipe could not be read/settled.
    #[error("command credential output could not be read")]
    OutputRead,
    /// Nothing remained after removing terminal CR/LF.
    #[error("command credential returned empty output")]
    EmptyOutput,
    /// Output was not exactly one UTF-8 line.
    #[error("command credential output must be one UTF-8 line")]
    InvalidOutput,
}
