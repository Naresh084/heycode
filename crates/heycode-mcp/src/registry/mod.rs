//! Provider-neutral MCP definition and live-generation registry.

mod error;
mod id;
mod model;
mod plugin;
mod service;

pub use error::McpRegistryError;
pub use id::{McpConnectionProviderId, McpSecretReference, McpServerId};
pub use model::{
    MCP_SNAPSHOT_SCHEMA_VERSION, McpApprovalMode, McpArgument, McpAuthenticationState,
    McpCapabilitySet, McpConnectionGeneration, McpConnectionState, McpContributionCounts,
    McpDefinitionScope, McpEnvironmentValue, McpExposurePolicy, McpFailureCode,
    McpGenerationCandidate, McpGenerationRetention, McpNamedValueSnapshot, McpReconnectPolicy,
    McpServerDefinition, McpServerDefinitionSnapshot, McpServerSnapshot, McpSnapshot,
    McpStdioTransport, McpStreamableHttpTransport, McpTimeouts, McpToolPolicy,
    McpTransportDefinition, McpTransportKind, McpTransportSnapshot, McpValueSourceKind,
    McpValueSourceSnapshot,
};
pub use plugin::mcp_registry_plugin;
pub use service::{McpConnectionPublisher, McpRegistry};
