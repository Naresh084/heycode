//! Pinned Codex app-server process and JSONL/JSON-RPC runtime Provider.
//!
//! R03 owns executable resolution, exact-version verification, initialization,
//! request correlation, bounded raw framing and reported containment-group
//! teardown. R04 adds credential-blind account/model/capability projection.
//! R06 adds primary delegated sessions: start/resume/fork, send/steer/cancel,
//! compaction, permission/question callbacks and quiescent close. Ephemeral
//! delegated subagents intentionally remain R05.

mod budget;
mod client;
mod config;
mod discovery;
mod error;
mod jsonl;
mod launch;
mod plugin;
mod runtime;
mod session;
mod version;
mod wire;

pub use client::{CodexAppServerClient, CodexHandshake};
pub use config::{CodexAppServerConfig, CodexClientInfo, codex_environment_snapshot};
pub use error::{CodexAppServerError, CodexAppServerErrorCode};
pub use plugin::codex_runtime_plugin;
pub use runtime::CodexRuntime;
pub use version::{CodexCliVersion, SUPPORTED_CODEX_CLI_VERSION};
pub use wire::{
    CodexInboundEvent, CodexNotification, CodexRequestId, CodexResponsePayload, CodexServerRequest,
};
