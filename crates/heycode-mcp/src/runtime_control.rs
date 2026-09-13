//! Named, lifecycle-bound control of live MCP connection supervisors.
//!
//! This service does not own transports or retry policy. It keeps only weak
//! references to the exact supervisors already owned by established stdio
//! generations, so an operator can ask that existing owner to begin its
//! bounded recovery episode without creating a second lifecycle or keeping a
//! disposed connection alive.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::{
    McpReconnectSupervisor, McpRecoveryAdmission, McpServerDefinition, McpTransportDefinition,
};

/// Product-session service for exact named MCP reconnect requests.
pub const SERVICE_MCP_RUNTIME_CONTROL: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("mcp-runtime-control");

#[derive(Debug)]
enum RuntimeEntry {
    Stdio {
        reconnect_enabled: bool,
        supervisor: Option<Weak<McpReconnectSupervisor>>,
    },
    StreamableHttp,
}

#[derive(Debug)]
struct RuntimeControlState {
    active: bool,
    servers: BTreeMap<String, RuntimeEntry>,
}

/// A product context's exact named MCP runtime-control surface.
///
/// Clones share one state. The service deliberately retains no strong
/// supervisor handle: established generations remain the only lifecycle owner.
#[derive(Debug, Clone)]
pub struct McpRuntimeControl {
    state: Arc<Mutex<RuntimeControlState>>,
}

impl McpRuntimeControl {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeControlState {
                active: true,
                servers: BTreeMap::new(),
            })),
        }
    }

    /// Ask the one live supervisor for `name` to start bounded recovery.
    ///
    /// This returns admission immediately. Recovery itself stays owned by the
    /// supervisor and never replays or retries a model tool action.
    ///
    /// # Errors
    /// The exact server is unknown, has no live stdio generation, uses an
    /// unsupported transport, or this context has shut down.
    pub fn reconnect(&self, name: &str) -> Result<McpRecoveryAdmission, McpRuntimeControlError> {
        let supervisor = {
            let state = self.lock();
            if !state.active {
                return Err(McpRuntimeControlError::ShutDown);
            }
            match state.servers.get(name) {
                None => {
                    return Err(McpRuntimeControlError::UnknownServer {
                        name: name.to_owned(),
                    });
                }
                Some(RuntimeEntry::StreamableHttp) => {
                    return Err(McpRuntimeControlError::UnsupportedTransport {
                        name: name.to_owned(),
                    });
                }
                Some(RuntimeEntry::Stdio {
                    reconnect_enabled: false,
                    ..
                }) => return Ok(McpRecoveryAdmission::Disabled),
                Some(RuntimeEntry::Stdio {
                    supervisor: Some(supervisor),
                    ..
                }) => supervisor.upgrade(),
                Some(RuntimeEntry::Stdio {
                    supervisor: None, ..
                }) => None,
            }
        };
        supervisor
            .map(|supervisor| supervisor.recover())
            .ok_or_else(|| McpRuntimeControlError::NoLiveConnection {
                name: name.to_owned(),
            })
    }

    pub(crate) fn declare(&self, definition: &McpServerDefinition) {
        let entry = match definition.transport() {
            McpTransportDefinition::Stdio(_) => RuntimeEntry::Stdio {
                reconnect_enabled: definition.reconnect().enabled(),
                supervisor: None,
            },
            McpTransportDefinition::StreamableHttp(_) => RuntimeEntry::StreamableHttp,
        };
        let mut state = self.lock();
        if state.active {
            state
                .servers
                .insert(definition.id().as_str().to_owned(), entry);
        }
    }

    pub(crate) fn attach(&self, name: &str, supervisor: Option<&Arc<McpReconnectSupervisor>>) {
        let mut state = self.lock();
        let Some(RuntimeEntry::Stdio {
            supervisor: slot, ..
        }) = state.servers.get_mut(name)
        else {
            return;
        };
        *slot = supervisor.map(Arc::downgrade);
    }

    pub(crate) fn shutdown(&self) {
        let mut state = self.lock();
        state.active = false;
        state.servers.clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeControlState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Why a named reconnect request could not reach a live supervisor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpRuntimeControlError {
    /// The product context has no definition with this exact server name.
    #[error("no MCP runtime server named `{name}`")]
    UnknownServer {
        /// Exact name the caller requested.
        name: String,
    },
    /// Streamable HTTP sessions have no reconnect supervisor.
    #[error("MCP server `{name}` uses Streamable HTTP, which has no manual reconnect owner")]
    UnsupportedTransport {
        /// Exact Streamable HTTP server name.
        name: String,
    },
    /// The definition exists, but no established stdio generation owns it.
    #[error("MCP server `{name}` has no live stdio connection to reconnect")]
    NoLiveConnection {
        /// Exact stdio definition with no established generation.
        name: String,
    },
    /// Context disposal already retired every connection owner.
    #[error("MCP runtime control is shut down")]
    ShutDown,
}
