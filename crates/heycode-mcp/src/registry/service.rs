//! Effect-owned MCP definitions and token-guarded connection publications.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use heycode_core::Context;

use super::model::MCP_SNAPSHOT_SCHEMA_VERSION;
use super::{
    McpAuthenticationState, McpConnectionGeneration, McpConnectionProviderId, McpConnectionState,
    McpFailureCode, McpGenerationCandidate, McpGenerationRetention, McpRegistryError,
    McpServerDefinition, McpServerId, McpServerSnapshot, McpSnapshot,
};

struct ConnectionEntry {
    provider: McpConnectionProviderId,
    token: Arc<()>,
    state: McpConnectionState,
    authentication: McpAuthenticationState,
    last_good_generation: Option<Arc<McpConnectionGeneration>>,
}

struct DefinitionEntry {
    definition: Arc<McpServerDefinition>,
    definition_revision: u64,
    token: Arc<()>,
    connection: Option<ConnectionEntry>,
    next_generation: u64,
}

struct RegistryState {
    active: bool,
    revision: u64,
    entries: BTreeMap<McpServerId, DefinitionEntry>,
    snapshot: Arc<McpSnapshot>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            active: true,
            revision: 0,
            entries: BTreeMap::new(),
            snapshot: Arc::new(McpSnapshot {
                schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
                active: true,
                revision: 0,
                servers: Vec::new(),
            }),
        }
    }
}

/// Deterministic registry of exact definitions and successful connection generations.
#[derive(Clone, Default)]
pub struct McpRegistry {
    state: Arc<Mutex<RegistryState>>,
    // Transport plugins can be applied by both the CLI and extension host.
    // Clones of the session registry must join the same model-facing hub.
    model_resource_hub: Arc<Mutex<Weak<crate::model_tools::ModelResourceHub>>>,
}

impl McpRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn model_resource_tools(
        &self,
        tools: &Arc<heycode_tools::ToolRegistry>,
    ) -> Result<crate::model_tools::ModelResourceToolOwner, heycode_tools::RegisterError> {
        let mut shared = self
            .model_resource_hub
            .lock()
            .map_err(|_| heycode_tools::RegisterError::RegistryUnavailable)?;
        let hub = shared.upgrade().unwrap_or_else(|| {
            let hub = crate::model_tools::ModelResourceHub::new(self.clone());
            *shared = Arc::downgrade(&hub);
            hub
        });
        hub.acquire_tools(tools)
    }

    /// Register one immutable exact definition for the owning context lifetime.
    ///
    /// Registration and its public snapshot become visible at one commit point.
    /// Shutdown/rollback removes only the token-matching row and any associated
    /// connection generation.
    ///
    /// # Errors
    /// Duplicate server ids, poisoned state or revision exhaustion.
    pub fn register_definition(
        &self,
        context: &Context,
        definition: McpServerDefinition,
    ) -> Result<(), McpRegistryError> {
        let id = definition.id().clone();
        let definition = Arc::new(definition);
        let token = Arc::new(());
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?;
        ensure_active(&state)?;
        if state.entries.contains_key(&id) {
            return Err(McpRegistryError::DuplicateServer {
                id: id.as_str().to_owned(),
            });
        }
        let revision = next_revision(&state)?;
        state.entries.insert(
            id.clone(),
            DefinitionEntry {
                definition,
                definition_revision: revision,
                token: token.clone(),
                connection: None,
                next_generation: 1,
            },
        );
        commit_snapshot(&mut state, revision);
        drop(state);

        let registration = DefinitionRegistration {
            state: Arc::downgrade(&self.state),
            id,
            token,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Obtain the exact private definition for a trusted transport/provider Consumer.
    ///
    /// The returned type has no `Debug` or serialization surface because it
    /// may contain literal arguments/environment values.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn definition(
        &self,
        id: &McpServerId,
    ) -> Result<Option<Arc<McpServerDefinition>>, McpRegistryError> {
        let state = self
            .state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?;
        ensure_active(&state)?;
        Ok(state.entries.get(id).map(|entry| entry.definition.clone()))
    }

    /// Register the unique live connection provider for an enabled definition.
    ///
    /// The returned publisher is token-guarded. Context shutdown removes its
    /// generation and makes every retained clone stale before another provider
    /// can register.
    ///
    /// # Errors
    /// Missing/disabled server, duplicate provider, poisoned state or revision exhaustion.
    pub fn register_connection(
        &self,
        context: &Context,
        id: &McpServerId,
        provider: McpConnectionProviderId,
        started_at_ms: u64,
    ) -> Result<McpConnectionPublisher, McpRegistryError> {
        let token = Arc::new(());
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?;
        ensure_active(&state)?;
        let revision = next_revision(&state)?;
        let entry = state
            .entries
            .get_mut(id)
            .ok_or_else(|| McpRegistryError::ServerNotFound {
                id: id.as_str().to_owned(),
            })?;
        if !entry.definition.enabled() {
            return Err(McpRegistryError::ServerDisabled {
                id: id.as_str().to_owned(),
            });
        }
        if entry.connection.is_some() {
            return Err(McpRegistryError::DuplicateConnection {
                id: id.as_str().to_owned(),
            });
        }
        let authentication = if entry.definition.has_credential_references() {
            McpAuthenticationState::Unknown
        } else {
            McpAuthenticationState::NotRequired
        };
        entry.connection = Some(ConnectionEntry {
            provider,
            token: token.clone(),
            state: McpConnectionState::Starting {
                since_ms: started_at_ms,
            },
            authentication,
            last_good_generation: None,
        });
        commit_snapshot(&mut state, revision);
        drop(state);

        let registration = ConnectionRegistration {
            state: Arc::downgrade(&self.state),
            id: id.clone(),
            token: token.clone(),
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(McpConnectionPublisher {
            state: Arc::downgrade(&self.state),
            id: id.clone(),
            token,
        })
    }

    /// Return one immutable, redacted whole-registry snapshot.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn snapshot(&self) -> Result<Arc<McpSnapshot>, McpRegistryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?
            .snapshot
            .clone())
    }

    pub(super) fn shutdown(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.active {
            return;
        }
        state.active = false;
        state.entries.clear();
        let revision = state.revision.saturating_add(1);
        commit_snapshot(&mut state, revision);
    }
}

/// Token-guarded publication handle owned by one connection-provider effect.
#[derive(Clone)]
pub struct McpConnectionPublisher {
    state: Weak<Mutex<RegistryState>>,
    id: McpServerId,
    token: Arc<()>,
}

impl McpConnectionPublisher {
    /// Atomically publish a complete validated successful generation.
    ///
    /// The successful number advances only at the commit point. Rejected or
    /// stale candidates cannot replace/advance the last good generation.
    ///
    /// # Errors
    /// Stale registration, poisoned state or exhausted revision/generation.
    pub fn publish_generation(
        &self,
        candidate: McpGenerationCandidate,
    ) -> Result<Arc<McpConnectionGeneration>, McpRegistryError> {
        let state = self.state.upgrade().ok_or_else(|| self.stale_error())?;
        let mut state = state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?;
        let revision = next_revision(&state)?;
        let entry = current_entry_mut(&mut state, &self.id, &self.token)?;
        let number = entry.next_generation;
        let next = number
            .checked_add(1)
            .ok_or_else(|| McpRegistryError::GenerationExhausted {
                id: self.id.as_str().to_owned(),
            })?;
        let connection = entry
            .connection
            .as_mut()
            .ok_or_else(|| self.stale_error())?;
        let committed_at_ms = candidate.observed_at_ms();
        let generation = Arc::new(candidate.into_generation(
            number,
            entry.definition_revision,
            connection.provider.clone(),
        ));
        entry.next_generation = next;
        connection.last_good_generation = Some(generation.clone());
        connection.authentication = if entry.definition.has_credential_references() {
            McpAuthenticationState::Connected
        } else {
            McpAuthenticationState::NotRequired
        };
        connection.state = McpConnectionState::Ready {
            since_ms: committed_at_ms,
        };
        commit_snapshot(&mut state, revision);
        Ok(generation)
    }

    /// Publish a new safe authentication state without changing generation ownership.
    ///
    /// # Errors
    /// Stale registration, poisoned state or revision exhaustion.
    pub fn report_authentication(
        &self,
        authentication: McpAuthenticationState,
    ) -> Result<(), McpRegistryError> {
        self.update_connection(|definition, connection| {
            let has_references = definition.has_credential_references();
            if (has_references && authentication == McpAuthenticationState::NotRequired)
                || (!has_references && authentication != McpAuthenticationState::NotRequired)
            {
                return Err(McpRegistryError::invalid(
                    "authentication state",
                    "must agree with the definition's credential references",
                ));
            }
            if connection.authentication == authentication {
                return Ok(false);
            }
            connection.authentication = authentication;
            Ok(true)
        })
    }

    /// Report connection initialization while retaining any last-good generation.
    ///
    /// # Errors
    /// Stale registration, poisoned state or revision exhaustion.
    pub fn report_starting(&self, since_ms: u64) -> Result<(), McpRegistryError> {
        self.update_connection(|_, connection| {
            let state = McpConnectionState::Starting { since_ms };
            if connection.state == state {
                return Ok(false);
            }
            connection.state = state;
            Ok(true)
        })
    }

    /// Report that authorization must complete before readiness.
    ///
    /// # Errors
    /// Stale registration, poisoned state or revision exhaustion.
    pub fn report_authentication_required(&self, since_ms: u64) -> Result<(), McpRegistryError> {
        self.update_connection(|definition, connection| {
            if !definition.has_credential_references() {
                return Err(McpRegistryError::invalid(
                    "authentication state",
                    "cannot require authentication without credential references",
                ));
            }
            let state = McpConnectionState::AuthenticationRequired { since_ms };
            let changed = connection.state != state
                || connection.authentication != McpAuthenticationState::Required;
            connection.state = state;
            connection.authentication = McpAuthenticationState::Required;
            Ok(changed)
        })
    }

    /// Report one bounded reconnect attempt while retaining last-good metadata.
    ///
    /// # Errors
    /// Attempt bounds, stale registration, poisoned state or revision exhaustion.
    pub fn report_reconnecting(
        &self,
        attempt: u32,
        max_attempts: u32,
        next_retry_at_ms: u64,
    ) -> Result<(), McpRegistryError> {
        if attempt == 0 || max_attempts == 0 || attempt > max_attempts {
            return Err(McpRegistryError::invalid(
                "reconnect state",
                "1 <= attempt <= max_attempts",
            ));
        }
        self.update_connection(|definition, connection| {
            let policy = definition.reconnect();
            if !policy.enabled() || max_attempts != policy.max_attempts() {
                return Err(McpRegistryError::invalid(
                    "reconnect state",
                    "must use the enabled definition reconnect budget",
                ));
            }
            let state = McpConnectionState::Reconnecting {
                attempt,
                max_attempts,
                next_retry_at_ms,
            };
            if connection.state == state {
                return Ok(false);
            }
            connection.state = state;
            Ok(true)
        })
    }

    /// Publish a classified failure with explicit last-good retention.
    ///
    /// Keeping a nonexistent generation produces `Failed`, never a false
    /// degraded/usable claim. Removing a generation is used by disable,
    /// provider disposal and exhausted reconnect.
    ///
    /// # Errors
    /// Stale registration, poisoned state or revision exhaustion.
    pub fn report_failure(
        &self,
        code: McpFailureCode,
        observed_at_ms: u64,
        retention: McpGenerationRetention,
    ) -> Result<(), McpRegistryError> {
        self.update_connection(|_, connection| {
            if retention == McpGenerationRetention::Remove {
                connection.last_good_generation = None;
            }
            connection.state = if connection.last_good_generation.is_some() {
                McpConnectionState::Degraded {
                    code,
                    observed_at_ms,
                }
            } else {
                McpConnectionState::Failed {
                    code,
                    observed_at_ms,
                }
            };
            Ok(true)
        })
    }

    fn update_connection(
        &self,
        update: impl FnOnce(
            &McpServerDefinition,
            &mut ConnectionEntry,
        ) -> Result<bool, McpRegistryError>,
    ) -> Result<(), McpRegistryError> {
        let state = self.state.upgrade().ok_or_else(|| self.stale_error())?;
        let mut state = state
            .lock()
            .map_err(|_| McpRegistryError::RegistryUnavailable)?;
        let revision = next_revision(&state)?;
        let entry = current_entry_mut(&mut state, &self.id, &self.token)?;
        let definition = entry.definition.clone();
        let connection = entry
            .connection
            .as_mut()
            .ok_or_else(|| self.stale_error())?;
        if !update(&definition, connection)? {
            return Ok(());
        }
        commit_snapshot(&mut state, revision);
        Ok(())
    }

    fn stale_error(&self) -> McpRegistryError {
        McpRegistryError::StaleRegistration {
            id: self.id.as_str().to_owned(),
        }
    }
}

struct DefinitionRegistration {
    state: Weak<Mutex<RegistryState>>,
    id: McpServerId,
    token: Arc<()>,
    active: bool,
}

impl Drop for DefinitionRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let matches = state
            .entries
            .get(&self.id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if !matches {
            return;
        }
        state.entries.remove(&self.id);
        let revision = state.revision.saturating_add(1);
        commit_snapshot(&mut state, revision);
    }
}

struct ConnectionRegistration {
    state: Weak<Mutex<RegistryState>>,
    id: McpServerId,
    token: Arc<()>,
    active: bool,
}

impl Drop for ConnectionRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let Some(entry) = state.entries.get_mut(&self.id) else {
            return;
        };
        let matches = entry
            .connection
            .as_ref()
            .is_some_and(|connection| Arc::ptr_eq(&connection.token, &self.token));
        if !matches {
            return;
        }
        entry.connection = None;
        let revision = state.revision.saturating_add(1);
        commit_snapshot(&mut state, revision);
    }
}

fn current_entry_mut<'a>(
    state: &'a mut RegistryState,
    id: &McpServerId,
    token: &Arc<()>,
) -> Result<&'a mut DefinitionEntry, McpRegistryError> {
    ensure_active(state)?;
    let entry = state
        .entries
        .get_mut(id)
        .ok_or_else(|| McpRegistryError::StaleRegistration {
            id: id.as_str().to_owned(),
        })?;
    let matches = entry
        .connection
        .as_ref()
        .is_some_and(|connection| Arc::ptr_eq(&connection.token, token));
    if !matches {
        return Err(McpRegistryError::StaleRegistration {
            id: id.as_str().to_owned(),
        });
    }
    Ok(entry)
}

fn next_revision(state: &RegistryState) -> Result<u64, McpRegistryError> {
    state
        .revision
        .checked_add(1)
        .ok_or(McpRegistryError::RevisionExhausted)
}

fn ensure_active(state: &RegistryState) -> Result<(), McpRegistryError> {
    if state.active {
        Ok(())
    } else {
        Err(McpRegistryError::RegistryClosed)
    }
}

fn commit_snapshot(state: &mut RegistryState, revision: u64) {
    state.revision = revision;
    state.snapshot = Arc::new(McpSnapshot {
        schema_version: MCP_SNAPSHOT_SCHEMA_VERSION,
        active: state.active,
        revision,
        servers: state
            .entries
            .values()
            .map(|entry| {
                let (state, authentication, provider, generation) = match entry.connection.as_ref()
                {
                    Some(connection) => (
                        connection.state.clone(),
                        connection.authentication,
                        Some(connection.provider.clone()),
                        connection
                            .last_good_generation
                            .as_ref()
                            .map(|generation| generation.as_ref().clone()),
                    ),
                    None => (
                        if entry.definition.enabled() {
                            McpConnectionState::Inactive
                        } else {
                            McpConnectionState::Disabled
                        },
                        if entry.definition.has_credential_references() {
                            McpAuthenticationState::Unknown
                        } else {
                            McpAuthenticationState::NotRequired
                        },
                        None,
                        None,
                    ),
                };
                McpServerSnapshot {
                    definition: entry.definition.snapshot(entry.definition_revision),
                    state,
                    authentication,
                    connection_provider: provider,
                    last_good_generation: generation,
                }
            })
            .collect(),
    });
}
