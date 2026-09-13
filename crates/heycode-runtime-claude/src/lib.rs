//! Claude Code delegated-runtime process provider.
//!
//! This crate owns the installed Claude Code process boundary, compatible
//! version handshake, credential-blind account inspection, and a fixed
//! no-persistence query probe. R08 builds delegated sessions and event mapping
//! on this foundation; R07 does not pretend those session controls exist.

mod client;
mod config;
mod error;
mod executable;
mod lifecycle;
mod plugin;
mod runtime;
mod session;
mod version;
mod wire;

pub use client::{ClaudeCliClient, ClaudeHandshakeReceipt};
pub use config::{ClaudeRuntimeConfig, claude_environment_snapshot};
pub use error::ClaudeRuntimeConfigError;
pub use plugin::claude_runtime_plugin;
pub use runtime::{CLAUDE_RUNTIME_ID, ClaudeRuntime};
pub use version::{ClaudeCliVersion, ClaudeVersionPolicy};
