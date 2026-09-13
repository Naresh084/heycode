//! Health-history failures, with no field that can carry a value.
//!
//! Paths are the one runtime string an error here holds, and they are held as
//! a [`HealthLabel`] so the same screen that protects the retained history
//! protects the message that names it. A directory a user named after a token
//! is not a hypothetical shape of mistake.

use super::HealthLabel;

/// Why a retained health history could not be read or committed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HealthHistoryError {
    /// The path could not be read or written.
    #[error("health history `{path}` is unavailable: {operation} failed")]
    Io {
        /// Screened path.
        path: HealthLabel,
        /// Boundary that failed.
        operation: &'static str,
        /// Underlying operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// The path exists but is a directory, a symbolic link or a device.
    #[error("health history `{path}` is not a regular file")]
    NotRegularFile {
        /// Screened path.
        path: HealthLabel,
    },
    /// The first line is not a header this build recognizes. The file is left
    /// exactly as it was found.
    #[error(
        "health history `{path}` has no readable v{supported} header; \
         move or delete the file to start a new history"
    )]
    MalformedHeader {
        /// Screened path.
        path: HealthLabel,
        /// Schema version this build writes and reads.
        supported: u32,
    },
    /// A newer heycode wrote this history. It is neither read nor overwritten.
    #[error(
        "health history `{path}` was written by a newer heycode (schema v{found}); \
         this build reads v{supported}"
    )]
    NewerSchema {
        /// Screened path.
        path: HealthLabel,
        /// Version found in the header.
        found: u32,
        /// Version this build reads.
        supported: u32,
    },
    /// The file is larger than the reader will load.
    #[error("health history `{path}` is larger than the {limit}-byte read bound")]
    Oversized {
        /// Screened path.
        path: HealthLabel,
        /// Read bound in bytes.
        limit: u64,
    },
    /// A retained entry could not be encoded.
    #[error("a health history entry could not be encoded")]
    Encode,
    /// Internal state was poisoned by a prior panic.
    #[error("the health history store is unavailable")]
    Unavailable,
}
