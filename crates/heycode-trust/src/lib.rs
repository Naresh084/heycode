//! Durable workspace identity, trust decisions, and project authority gates.
//!
//! The trust service is intentionally below config/profile/MCP/plugin
//! Consumers. It answers whether one source may contribute authority; it does
//! not discover project files or silently activate them.
//! The owner-only store protects the product from untrusted project authority;
//! it does not claim isolation from another process already controlling the
//! same OS user account.
//! Persistent file-backed trust currently fails closed outside Unix; the
//! in-memory backend remains portable for embedding and deterministic tests.

mod error;
mod model;
mod plugin;
mod service;
mod store;

pub use error::WorkspaceTrustError;
pub use model::{
    ExplicitWorkspaceTrust, ProjectAccess, ProjectContentPolicy, ProjectInputKind, TrustFrontend,
    TrustPersistence, TrustStartupState, UntrustedProjectAccess, WorkspaceId, WorkspaceIdentity,
    WorkspaceTrustAction, WorkspaceTrustActionOutcome, WorkspaceTrustDecision,
    WorkspaceTrustDialogState, WorkspaceTrustSnapshot,
};
pub use plugin::trust_service_plugin;
pub use service::{WorkspaceTrustPrompt, WorkspaceTrustService};

/// Durable workspace-trust service key.
pub const SERVICE_TRUST: heycode_core::ServiceKey = heycode_core::ServiceKey::new("trust");

/// Current owner-only trust-store schema.
pub const TRUST_FILE_SCHEMA_VERSION: u32 = 1;
