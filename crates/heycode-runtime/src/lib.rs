//! Native/delegated coding-agent runtime contracts and effect-owned registry.

mod acp;
mod discovery_workspace;
mod error;
mod event_hub;
mod id;
mod model;
mod normalization;
mod plugin;
mod process;
mod registry;
mod traits;
mod unavailable;

pub use acp::{
    AcpFrameDecoder, AcpFrameError, AcpProcess, AcpProcessFactory, AcpProcessSpec, AcpRuntime,
    AcpRuntimeConfig, DEFAULT_MAX_ACP_FRAME_BYTES, opencode_acp_descriptor, opencode_acp_runtime,
    opencode_acp_runtime_pinned,
};
pub use discovery_workspace::RuntimeDiscoveryWorkspace;
pub use error::{AgentRuntimeRegistryError, RuntimeContractError, RuntimeError, RuntimeErrorCode};
pub use event_hub::{RUNTIME_EVENT_HISTORY, RuntimeEventHub};
pub use id::{AgentRuntimeId, RuntimeRequestId, RuntimeSessionId, RuntimeTurnId};
pub use model::{
    AccountState, AccountStatus, AgentRuntimeDescriptor, AgentRuntimeKind, RuntimeCapabilities,
    RuntimeCompactOutcome, RuntimeConfiguration, RuntimeConfigurationCapabilities,
    RuntimeContextUsage, RuntimeEvent, RuntimeEventKind, RuntimeFinishReason, RuntimeFork,
    RuntimeInput, RuntimeModelConfiguration, RuntimePermissionDecision, RuntimePermissionResponse,
    RuntimeQuestionResponse, RuntimeResume, RuntimeStart, RuntimeToolCall, RuntimeToolExecutor,
    RuntimeToolOutput,
};
pub use normalization::{
    MAX_RUNTIME_EVENT_IDENTITIES, NormalizedRuntimeEvent, NormalizedRuntimeEventStream,
    RuntimeEventNormalizer, RuntimeEventViolation, RuntimeEventViolationCode,
    normalize_runtime_event_replay, normalize_runtime_event_stream,
};
pub use plugin::runtime_registry_plugin;
pub use process::ManagedAcpProcessFactory;
pub use registry::AgentRuntimeRegistry;
pub use traits::{AgentRuntime, RuntimeEventStream, RuntimeSession};
pub use unavailable::UnavailableAgentRuntime;

/// Effect-owned native/delegated runtime registry service.
pub const SERVICE_RUNTIMES: heycode_core::ServiceKey = heycode_core::ServiceKey::new("runtimes");
