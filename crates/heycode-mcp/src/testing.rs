//! Reusable MCP protocol-lab runner for downstream and crate integration tests.
//!
//! A fixture still owns its transport-specific setup. The runner owns the
//! assertions shared across stdio, Streamable HTTP and OAuth: bounded request
//! counts, exactly one successful publication, cancellation without
//! publication, and body/credential-free hostile-input diagnostics.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

const MAX_LAB_REQUESTS: u32 = 64;
const MAX_LAB_DIAGNOSTIC_BYTES: usize = 256;

/// Transport/auth family exercised by one lab fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum McpProtocolLabFamily {
    /// JSON-RPC over a child process's standard streams.
    Stdio,
    /// JSON-RPC over Streamable HTTP.
    StreamableHttp,
    /// OAuth discovery, callback and token exchange.
    OAuth,
}

/// One fixture phase's observable facts.
#[derive(Clone, PartialEq, Eq)]
pub struct McpProtocolLabObservation {
    requests: u32,
    publications: u32,
    cancellation_observed: bool,
    diagnostic: Option<String>,
}

impl McpProtocolLabObservation {
    /// Successful phase facts.
    #[must_use]
    pub fn success(requests: u32, publications: u32) -> Self {
        Self {
            requests,
            publications,
            cancellation_observed: false,
            diagnostic: None,
        }
    }

    /// Cancelled phase facts.
    #[must_use]
    pub fn cancelled(requests: u32, cancellation_observed: bool) -> Self {
        Self {
            requests,
            publications: 0,
            cancellation_observed,
            diagnostic: None,
        }
    }

    /// Hostile-input rejection facts.
    #[must_use]
    pub fn rejected(requests: u32, diagnostic: impl Into<String>) -> Self {
        Self {
            requests,
            publications: 0,
            cancellation_observed: false,
            diagnostic: Some(diagnostic.into()),
        }
    }
}

impl std::fmt::Debug for McpProtocolLabObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpProtocolLabObservation")
            .field("requests", &self.requests)
            .field("publications", &self.publications)
            .field("cancellation_observed", &self.cancellation_observed)
            .field("has_diagnostic", &self.diagnostic.is_some())
            .finish()
    }
}

/// One transport-specific fixture driven by the shared lab.
#[async_trait]
pub trait McpProtocolLabFixture: Send + Sync {
    /// Fixture family.
    fn family(&self) -> McpProtocolLabFamily;

    /// Canary that must not enter a hostile-input diagnostic.
    fn secret_canary(&self) -> &'static str;

    /// Run one successful bounded exchange.
    async fn successful(&self) -> McpProtocolLabObservation;

    /// Run with an already-cancelled operation token.
    async fn cancelled(&self, cancellation: CancellationToken) -> McpProtocolLabObservation;

    /// Feed one hostile body/frame/callback.
    async fn hostile(&self) -> McpProtocolLabObservation;
}

/// Stable failure phase reported by the lab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProtocolLabPhase {
    /// Successful-path assertions.
    Success,
    /// Cancellation assertions.
    Cancellation,
    /// Hostile-input assertions.
    Hostile,
}

/// Body-free shared-lab failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpProtocolLabError {
    /// An observation violated the shared contract.
    #[error("MCP protocol lab fixture violated the shared contract")]
    Contract {
        /// Fixture family.
        family: McpProtocolLabFamily,
        /// Phase whose facts were invalid.
        phase: McpProtocolLabPhase,
    },
}

/// Passing report for one fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpProtocolLabReport {
    family: McpProtocolLabFamily,
}

impl McpProtocolLabReport {
    /// Fixture family.
    #[must_use]
    pub const fn family(self) -> McpProtocolLabFamily {
        self.family
    }

    /// A report exists only after every phase passed.
    #[must_use]
    pub const fn passed(self) -> bool {
        true
    }
}

/// Run the same lifecycle/security assertions over every supplied fixture.
///
/// # Errors
/// A fixture reports an unbounded exchange, a non-atomic successful
/// publication, a cancellation publication, or an unsafe hostile diagnostic.
pub async fn run_mcp_protocol_lab(
    fixtures: &[Arc<dyn McpProtocolLabFixture>],
) -> Result<Vec<McpProtocolLabReport>, McpProtocolLabError> {
    let mut reports = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let family = fixture.family();
        let success = fixture.successful().await;
        if success.requests == 0
            || success.requests > MAX_LAB_REQUESTS
            || success.publications != 1
            || success.cancellation_observed
            || success.diagnostic.is_some()
        {
            return Err(McpProtocolLabError::Contract {
                family,
                phase: McpProtocolLabPhase::Success,
            });
        }

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = fixture.cancelled(cancellation).await;
        if cancelled.requests > MAX_LAB_REQUESTS
            || cancelled.publications != 0
            || !cancelled.cancellation_observed
            || cancelled.diagnostic.is_some()
        {
            return Err(McpProtocolLabError::Contract {
                family,
                phase: McpProtocolLabPhase::Cancellation,
            });
        }

        let hostile = fixture.hostile().await;
        let safe_diagnostic = hostile.diagnostic.as_deref().is_some_and(|diagnostic| {
            !diagnostic.is_empty()
                && diagnostic.len() <= MAX_LAB_DIAGNOSTIC_BYTES
                && !diagnostic.chars().any(char::is_control)
                && !diagnostic.contains(fixture.secret_canary())
        });
        if hostile.requests > MAX_LAB_REQUESTS
            || hostile.publications != 0
            || hostile.cancellation_observed
            || !safe_diagnostic
        {
            return Err(McpProtocolLabError::Contract {
                family,
                phase: McpProtocolLabPhase::Hostile,
            });
        }
        reports.push(McpProtocolLabReport { family });
    }
    Ok(reports)
}
