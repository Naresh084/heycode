//! Validated identity failures for the AWS auth vocabulary.

/// Failures raised when constructing a validated AWS identity newtype.
///
/// These describe *shape*, never content: the rejected region id is echoed
/// because it is a user-typed configuration value, while nothing that could
/// carry credential material ever reaches this type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AwsAuthError {
    /// Region id was empty, over-long, or outside `[a-z0-9-]`.
    #[error("invalid AWS region `{value}`; expected [a-z][a-z0-9-]* of 2..=64 bytes")]
    InvalidRegion {
        /// Rejected region id.
        value: String,
    },
    /// Profile name was empty, over-long, or contained unsupported bytes.
    #[error("invalid AWS profile name; expected 1..=64 printable ASCII bytes without `[`/`]`")]
    InvalidProfileName,
}
