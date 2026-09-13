//! Object-safe runtime and live-session interfaces.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use tokio_util::sync::CancellationToken;

use crate::{
    AccountState, AgentRuntimeDescriptor, AgentRuntimeId, RuntimeCompactOutcome,
    RuntimeConfiguration, RuntimeError, RuntimeEvent, RuntimeFork, RuntimeInput,
    RuntimeModelConfiguration, RuntimePermissionResponse, RuntimeQuestionResponse, RuntimeResume,
    RuntimeSessionId, RuntimeStart, RuntimeTurnId,
};

/// Sendable raw adapter event stream.
///
/// Delegated consumers must wrap it with `normalize_runtime_event_stream`
/// before durable/UI projection. Unsolicited EOF on a live subscription is a
/// protocol failure; finite snapshot consumers use
/// `normalize_runtime_event_replay` instead.
pub type RuntimeEventStream =
    Pin<Box<dyn Stream<Item = Result<RuntimeEvent, RuntimeError>> + Send + 'static>>;

/// A coding-agent implementation that owns (delegated) or supplies (native)
/// the complete turn loop.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Immutable discovery/capability descriptor.
    fn descriptor(&self) -> &AgentRuntimeDescriptor;

    /// Inspect account/auth state without exposing credentials.
    async fn account(&self, cancellation: CancellationToken) -> Result<AccountState, RuntimeError>;

    /// Discover runtime-owned models as a normalized catalog generation.
    async fn models(
        &self,
        cancellation: CancellationToken,
    ) -> Result<heycode_llm::CatalogSnapshot, RuntimeError>;

    /// Discover exact provider-native reasoning effort choices per model.
    ///
    /// The default preserves compatibility for runtimes that have not proved
    /// an effort catalog; it intentionally does not synthesize choices from a
    /// boolean reasoning capability.
    async fn model_configurations(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<RuntimeModelConfiguration>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        Ok(Vec::new())
    }

    /// Start a new runtime session.
    async fn start(
        &self,
        request: RuntimeStart,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError>;

    /// Resume a provider-native runtime session.
    async fn resume(
        &self,
        request: RuntimeResume,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError>;

    /// Fork a provider-native runtime session into a new heycode session.
    async fn fork(
        &self,
        request: RuntimeFork,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn RuntimeSession>, RuntimeError>;
}

/// One active native/delegated coding-agent session.
///
/// Every async operation receives exactly one caller lifecycle token. `close`
/// resolves only after the implementation has stopped accepting work, settled
/// its event stream and reaped owned tasks/processes.
#[async_trait]
pub trait RuntimeSession: Send + Sync {
    /// Provider-native session/thread id.
    fn id(&self) -> &RuntimeSessionId;

    /// Registry runtime implementation that owns this session.
    fn runtime_id(&self) -> &AgentRuntimeId;

    /// Immutable capabilities effective for this session.
    fn capabilities(&self) -> &crate::RuntimeCapabilities;

    /// Metadata advertised by this exact live session for one accepted model
    /// control value.
    ///
    /// The default is intentionally absent: runtimes must not manufacture a
    /// display label, context limit, or default effort from a model id.
    fn model_configuration(&self, _model: &str) -> Option<RuntimeModelConfiguration> {
        None
    }

    /// Subscribe to normalized runtime events. Implementations document
    /// whether multiple independent subscribers are supported.
    fn subscribe(&self) -> RuntimeEventStream;

    /// Start one user turn and return its provider-native id.
    async fn send(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError>;

    /// Consume one already durable pending input without appending it again.
    /// Hosts pass its exact occurrence id; unsupported runtimes fail closed.
    async fn send_pending(
        &self,
        _message_id: String,
        cancellation: CancellationToken,
    ) -> Result<RuntimeTurnId, RuntimeError> {
        if cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else {
            Err(RuntimeError::unsupported())
        }
    }

    /// Atomically apply model-visible controls between turns.
    ///
    /// Implementations reject this operation when unsupported or while a turn
    /// is active. The compatibility default applies no fields.
    async fn configure(
        &self,
        configuration: RuntimeConfiguration,
        cancellation: CancellationToken,
    ) -> Result<RuntimeConfiguration, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if configuration.is_empty() {
            Ok(configuration)
        } else {
            Err(RuntimeError::unsupported_fields(&configuration))
        }
    }

    /// Inject text into the next step of the active turn.
    async fn steer(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError>;

    /// Queue text for the next turn.
    async fn follow_up(
        &self,
        input: RuntimeInput,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError>;

    /// Cancel active work without closing the reusable session.
    async fn cancel(&self, cancellation: CancellationToken) -> Result<(), RuntimeError>;

    /// Answer one correlated permission request.
    async fn respond_permission(
        &self,
        response: RuntimePermissionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError>;

    /// Answer one correlated runtime question.
    async fn respond_question(
        &self,
        response: RuntimeQuestionResponse,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError>;

    /// Request runtime-native compaction when supported.
    async fn compact(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RuntimeCompactOutcome, RuntimeError>;

    /// Quiescent, idempotent lifecycle close.
    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError>;
}
