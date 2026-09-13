//! MCP Streamable HTTP transport (legacy era: `2025-06-18` / `2025-11-25`).

mod client;
mod error;
mod protocol;

pub use client::McpStreamableHttpClient;
pub use error::McpHttpError;
pub use protocol::{McpProtocolVersion, McpSessionId};
