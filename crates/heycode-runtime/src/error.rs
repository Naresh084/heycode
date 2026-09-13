//! Stable contract, operation and registry failures.

/// Invalid caller/provider metadata at the typed boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeContractError {
    /// A named field violated its closed validation contract.
    #[error("invalid runtime {field}: {requirement}")]
    InvalidField {
        /// Stable field label.
        field: &'static str,
        /// Static requirement text.
        requirement: &'static str,
    },
}

impl RuntimeContractError {
    pub(crate) const fn invalid(field: &'static str, requirement: &'static str) -> Self {
        Self::InvalidField { field, requirement }
    }
}

/// Stable operation failure class safe for UI/telemetry routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeErrorCode {
    /// Runtime process/service is currently unavailable.
    Unavailable,
    /// Runtime account is not authorized.
    Unauthorized,
    /// Requested capability is explicitly unsupported.
    Unsupported,
    /// Requested external session/resource does not exist.
    NotFound,
    /// Runtime state conflicts with the requested operation.
    Conflict,
    /// Caller/lifecycle cancellation settled the operation.
    Cancelled,
    /// External protocol violated its contract.
    Protocol,
    /// Typed request was rejected at a runtime boundary.
    InvalidRequest,
    /// Session already completed quiescent close.
    Closed,
    /// Redacted internal implementation failure.
    Internal,
}

/// Redacted runtime operation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RuntimeError {
    code: RuntimeErrorCode,
    message: String,
}

impl RuntimeError {
    /// Construct from a caller-declared safe, trimmed one-line diagnostic.
    ///
    /// # Errors
    /// Empty, over-512-byte or control-bearing text is rejected so provider
    /// bodies/tokens cannot accidentally become diagnostics.
    pub fn try_new(
        code: RuntimeErrorCode,
        message: impl Into<String>,
    ) -> Result<Self, RuntimeContractError> {
        let message = message.into();
        if message.is_empty()
            || message.trim() != message
            || message.len() > 512
            || message.chars().any(char::is_control)
        {
            return Err(RuntimeContractError::invalid(
                "error message",
                "1..=512 trimmed control-free bytes",
            ));
        }
        Ok(Self { code, message })
    }

    /// Standard cancellation error.
    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            code: RuntimeErrorCode::Cancelled,
            message: "runtime operation cancelled".to_owned(),
        }
    }

    /// Standard closed-session error.
    #[must_use]
    pub fn closed() -> Self {
        Self {
            code: RuntimeErrorCode::Closed,
            message: "runtime session is closed".to_owned(),
        }
    }

    /// Standard unsupported-capability error.
    #[must_use]
    pub fn unsupported() -> Self {
        Self {
            code: RuntimeErrorCode::Unsupported,
            message: "runtime capability is unsupported".to_owned(),
        }
    }

    /// Unsupported error that safely names the rejected configuration fields.
    #[must_use]
    pub fn unsupported_fields(configuration: &crate::RuntimeConfiguration) -> Self {
        let mut fields = Vec::new();
        if configuration.system_prompt().is_some() {
            fields.push("system_prompt");
        }
        if configuration.tools_configured() {
            fields.push("tools");
        }
        if configuration.model().is_some() {
            fields.push("model");
        }
        if configuration.reasoning_effort().is_some() {
            fields.push("reasoning_effort");
        }
        Self::unsupported_field_names(&fields)
    }

    /// Unsupported error that safely names an already-classified field set.
    #[must_use]
    pub fn unsupported_field_names(fields: &[&str]) -> Self {
        const ALLOWED: [&str; 4] = ["system_prompt", "tools", "model", "reasoning_effort"];
        let classified = fields.iter().all(|field| ALLOWED.contains(field)).then(|| {
            ALLOWED
                .into_iter()
                .filter(|field| fields.contains(field))
                .collect::<Vec<_>>()
        });
        let message = if classified.as_ref().is_none_or(Vec::is_empty) {
            "runtime configuration update is unsupported".to_owned()
        } else {
            format!(
                "runtime configuration fields are unsupported: {}",
                classified.unwrap_or_default().join(", ")
            )
        };
        Self {
            code: RuntimeErrorCode::Unsupported,
            message,
        }
    }

    /// Standard unavailable-runtime/service error.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            code: RuntimeErrorCode::Unavailable,
            message: "runtime capability is unavailable".to_owned(),
        }
    }

    /// Standard unauthorized-account error.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self {
            code: RuntimeErrorCode::Unauthorized,
            message: "runtime account is not authorized".to_owned(),
        }
    }

    /// Standard invalid-request error.
    #[must_use]
    pub fn invalid_request() -> Self {
        Self {
            code: RuntimeErrorCode::InvalidRequest,
            message: "runtime request does not match the active session".to_owned(),
        }
    }

    /// Standard conflicting-state error.
    #[must_use]
    pub fn conflict() -> Self {
        Self {
            code: RuntimeErrorCode::Conflict,
            message: "runtime state conflicts with the requested operation".to_owned(),
        }
    }

    /// Standard external event/protocol error.
    #[must_use]
    pub fn protocol() -> Self {
        Self {
            code: RuntimeErrorCode::Protocol,
            message: "runtime event protocol failed".to_owned(),
        }
    }

    /// Fixed internal failure that never includes the supplied private cause.
    #[must_use]
    pub fn internal(_private_cause: &str) -> Self {
        Self {
            code: RuntimeErrorCode::Internal,
            message: "runtime operation failed".to_owned(),
        }
    }

    /// Stable classification.
    #[must_use]
    pub const fn code(&self) -> RuntimeErrorCode {
        self.code
    }

    /// Already-redacted human diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Runtime registry publication/lookup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentRuntimeRegistryError {
    /// Two live plugins claimed one runtime id.
    #[error("agent runtime `{id}` is already registered")]
    Duplicate {
        /// Contested id.
        id: String,
    },
    /// Registry mutex was poisoned.
    #[error("agent runtime registry is unavailable")]
    RegistryUnavailable,
}
