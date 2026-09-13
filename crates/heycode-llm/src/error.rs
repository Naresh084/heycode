//! Errors surfaced by the LLM layer.

use thiserror::Error;

/// Stable provider failure category used by policy and UI code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorClass {
    /// Missing, expired, invalid or unauthorized credential/scope.
    Authentication,
    /// Provider/account request or token rate limit.
    RateLimited,
    /// Provider is temporarily overloaded.
    Overloaded,
    /// Provider-side internal failure.
    Server,
    /// Request/response deadline exceeded.
    Timeout,
    /// DNS/connect/TLS/body transport failure.
    Network,
    /// Provider or transport framing violated the selected protocol.
    Protocol,
    /// A bounded local/provider payload limit was exceeded.
    Overflow,
    /// Model context capacity was exceeded.
    ContextWindowExceeded,
    /// Request conflicts with transient provider state.
    Conflict,
    /// Non-retryable malformed/unsupported request.
    InvalidRequest,
    /// Caller cancellation won.
    Cancelled,
}

impl ProviderErrorClass {
    /// Stable body-free identifier for policy and telemetry dimensions.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Server => "server",
            Self::Timeout => "timeout",
            Self::Network => "network",
            Self::Protocol => "protocol",
            Self::Overflow => "overflow",
            Self::ContextWindowExceeded => "context_window_exceeded",
            Self::Conflict => "conflict",
            Self::InvalidRequest => "invalid_request",
            Self::Cancelled => "cancelled",
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Authentication => "provider authentication failed",
            Self::RateLimited => "provider rate limit reached",
            Self::Overloaded => "provider is overloaded",
            Self::Server => "provider server failed",
            Self::Timeout => "provider request timed out",
            Self::Network => "provider network request failed",
            Self::Protocol => "provider protocol failed",
            Self::Overflow => "provider payload limit exceeded",
            Self::ContextWindowExceeded => "provider context window exceeded",
            Self::Conflict => "provider request conflicted with transient state",
            Self::InvalidRequest => "provider rejected the request",
            Self::Cancelled => "provider request cancelled",
        }
    }
}

/// Where a normalized provider failure committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureOrigin {
    /// Non-success HTTP response before streamed output.
    Http,
    /// Provider-declared error event inside a successful transport stream.
    ProviderEvent,
    /// Network/deadline/framing transport boundary.
    Transport,
    /// Local request/response validation.
    Local,
}

/// Invalid provider discriminator fact.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderFactError {
    /// Discriminators are bounded printable ASCII tokens.
    #[error("provider error code must be 1..=64 safe ASCII bytes")]
    InvalidCode,
}

/// Bounded provider error discriminator retained as a structured fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderErrorCode(String);

impl ProviderErrorCode {
    /// Validate one bounded discriminator without rewriting it.
    ///
    /// # Errors
    /// Empty, overlong or non-token values fail.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderFactError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            })
        {
            return Err(ProviderFactError::InvalidCode);
        }
        Ok(Self(value))
    }

    /// Borrow the exact safe discriminator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Actionable explanations derived from recognized error facts, never response prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFailureGuidance {
    InvalidCredential,
    AgeConfirmation,
    OpenRouterAgeConfirmation,
    Credits,
    AccessDenied,
    DataPolicy,
    OpenRouterPaidModelTrainingPolicy,
    ModelUnavailable,
    UnsupportedRequest,
    UnsupportedParameter(&'static str),
}

impl ProviderFailureGuidance {
    const fn message(self) -> &'static str {
        match self {
            Self::InvalidCredential => "The API key was rejected. Use /connect to update it.",
            Self::AgeConfirmation => {
                "This model requires age confirmation. Complete it with your provider, or choose another model with /model."
            }
            Self::OpenRouterAgeConfirmation => {
                "This model requires 18+ age confirmation. Complete it at https://openrouter.ai/settings/preferences, or choose another model with /model."
            }
            Self::Credits => {
                "The provider reports insufficient credits or a billing limit. Check your account balance and spending limits, or use another connection."
            }
            Self::AccessDenied => {
                "The provider denied access to this model or account. Check model permissions, or choose another model with /model."
            }
            Self::DataPolicy => {
                "Your account's data policy does not allow this model. Review the provider's data settings, or choose another model with /model."
            }
            Self::OpenRouterPaidModelTrainingPolicy => {
                "OpenRouter reports that your account's paid-model training privacy setting excludes an endpoint for this model. Review https://openrouter.ai/settings/privacy to understand the required data policy, or choose another model with /model."
            }
            Self::ModelUnavailable => {
                "The requested model is unavailable to this account. Use /model to choose an available model."
            }
            Self::UnsupportedRequest | Self::UnsupportedParameter(_) => {
                "This model does not support the requested parameter or feature. Change the request options or choose another model with /model."
            }
        }
    }
}

/// Body-free normalized provider failure and safe retry facts.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    class: ProviderErrorClass,
    origin: ProviderFailureOrigin,
    status: Option<u16>,
    code: Option<ProviderErrorCode>,
    retry_after: Option<heycode_http::HttpRetryAfter>,
    retry_advice: Option<bool>,
    guidance: Option<ProviderFailureGuidance>,
}

impl std::fmt::Debug for ProviderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderFailure")
            .field("class", &self.class)
            .field("origin", &self.origin)
            .field("status", &self.status)
            .field("code_present", &self.code.is_some())
            .field("retry_after", &self.retry_after)
            .field("retry_advice", &self.retry_advice)
            .finish()
    }
}

impl ProviderFailure {
    /// Start one body-free failure.
    #[must_use]
    pub const fn new(class: ProviderErrorClass, origin: ProviderFailureOrigin) -> Self {
        Self {
            class,
            origin,
            status: None,
            code: None,
            retry_after: None,
            retry_advice: None,
            guidance: None,
        }
    }

    pub(crate) const fn with_guidance(mut self, guidance: ProviderFailureGuidance) -> Self {
        self.guidance = Some(guidance);
        self
    }

    /// Attach a numeric HTTP status.
    #[must_use]
    pub const fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Attach a validated provider discriminator.
    #[must_use]
    pub fn with_code(mut self, code: ProviderErrorCode) -> Self {
        self.code = Some(code);
        self
    }

    /// Attach parsed retry timing advice.
    #[must_use]
    pub const fn with_retry_after(mut self, retry_after: heycode_http::HttpRetryAfter) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    /// Attach an explicit provider approval/veto.
    #[must_use]
    pub const fn with_retry_advice(mut self, advice: bool) -> Self {
        self.retry_advice = Some(advice);
        self
    }

    /// Stable category.
    #[must_use]
    pub const fn class(&self) -> ProviderErrorClass {
        self.class
    }

    /// Failure origin.
    #[must_use]
    pub const fn origin(&self) -> ProviderFailureOrigin {
        self.origin
    }

    /// HTTP status when present.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Safe provider discriminator when present.
    #[must_use]
    pub const fn code(&self) -> Option<&ProviderErrorCode> {
        self.code.as_ref()
    }

    /// Parsed retry timing advice.
    #[must_use]
    pub const fn retry_after(&self) -> Option<heycode_http::HttpRetryAfter> {
        self.retry_after
    }

    /// Explicit provider approval/veto.
    #[must_use]
    pub const fn retry_advice(&self) -> Option<bool> {
        self.retry_advice
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ProviderFailureGuidance::UnsupportedParameter(parameter)) = self.guidance {
            return write!(
                formatter,
                "This model does not support `{parameter}`. Change that request option or choose another model with /model."
            );
        }
        if let Some(guidance) = self.guidance {
            return formatter.write_str(guidance.message());
        }
        formatter.write_str(self.class.description())
    }
}

impl std::error::Error for ProviderFailure {}

/// Failure modes of provider construction, request dispatch, and stream decoding.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum LlmError {
    /// The provider's key environment variable is unset or empty.
    #[error("missing API key {env}: export it or set [llm] config")]
    MissingApiKey {
        /// Environment variable that must hold the provider API key.
        env: &'static str,
    },

    /// Legacy non-success compatibility shape. Current adapters emit
    /// body-free [`Self::Provider`] failures.
    #[error("http {status}")]
    Http {
        /// HTTP status code returned by the server.
        status: u16,
        /// Tail of the error response body.
        body_tail: String,
    },

    /// A connection or transport failure before or during the stream.
    #[error("transport failure: {0}")]
    Transport(String),

    /// A stream payload violated the wire protocol (unparsable JSON, wrongly
    /// typed fields).
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),

    /// The route's credential could not be resolved when the operation
    /// needed it. Its text names the requested reference and no other route.
    #[error(transparent)]
    UnresolvedCredential(heycode_credentials::CredentialResolutionError),

    /// Stable body-free provider failure.
    #[error(transparent)]
    Provider(ProviderFailure),
}

impl LlmError {
    /// Stable category for policy/UI code, including compatibility variants.
    #[must_use]
    pub const fn class(&self) -> ProviderErrorClass {
        match self {
            Self::MissingApiKey { .. } | Self::UnresolvedCredential(_) => {
                ProviderErrorClass::Authentication
            }
            Self::Http { status, .. } => legacy_http_class(*status),
            Self::Transport(_) => ProviderErrorClass::Network,
            Self::InvalidResponse(_) => ProviderErrorClass::Protocol,
            Self::Provider(failure) => failure.class(),
        }
    }

    /// Structured body-free failure facts when emitted by a P08 adapter.
    #[must_use]
    pub const fn provider_failure(&self) -> Option<&ProviderFailure> {
        match self {
            Self::Provider(failure) => Some(failure),
            _ => None,
        }
    }
}

const fn legacy_http_class(status: u16) -> ProviderErrorClass {
    match status {
        401 | 403 => ProviderErrorClass::Authentication,
        408 | 504 => ProviderErrorClass::Timeout,
        409 => ProviderErrorClass::Conflict,
        413 => ProviderErrorClass::Overflow,
        429 => ProviderErrorClass::RateLimited,
        503 | 529 => ProviderErrorClass::Overloaded,
        500..=599 => ProviderErrorClass::Server,
        _ => ProviderErrorClass::InvalidRequest,
    }
}
