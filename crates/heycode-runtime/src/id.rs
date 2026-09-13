//! Validated opaque runtime identifiers.

use std::fmt::{Display, Formatter};

use crate::RuntimeContractError;

macro_rules! opaque_runtime_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validate an external identifier before it enters runtime state.
            ///
            /// # Errors
            /// Blank, over-256-byte or control-bearing values are rejected.
            pub fn new(value: impl Into<String>) -> Result<Self, RuntimeContractError> {
                let value = value.into();
                if value.is_empty()
                    || value.trim() != value
                    || value.len() > 256
                    || value.chars().any(char::is_control)
                {
                    return Err(RuntimeContractError::invalid(
                        stringify!($name),
                        "1..=256 trimmed control-free bytes",
                    ));
                }
                Ok(Self(value))
            }

            /// Provider-native opaque text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

/// Stable lowercase kebab-case runtime implementation id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentRuntimeId(String);

impl AgentRuntimeId {
    /// Validate a registry/runtime id.
    ///
    /// # Errors
    /// IDs must be 1..=64 ASCII lowercase kebab-case bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeContractError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || (byte == b'-' && index > 0)
            })
            && !value.ends_with('-')
            && !value.contains("--");
        if !valid {
            return Err(RuntimeContractError::invalid(
                "runtime id",
                "1..=64 lowercase kebab-case ASCII bytes",
            ));
        }
        Ok(Self(value))
    }

    /// Stable registry key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AgentRuntimeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

opaque_runtime_id!(
    RuntimeSessionId,
    "Provider-native opaque runtime session/thread id."
);
opaque_runtime_id!(RuntimeTurnId, "Provider-native opaque runtime turn id.");
opaque_runtime_id!(
    RuntimeRequestId,
    "Provider-native permission/question request id."
);

impl RuntimeTurnId {
    /// Construct the canonical decimal id used by the native heycode loop.
    #[must_use]
    pub fn from_native_turn(turn: u64) -> Self {
        Self(turn.to_string())
    }
}
