//! Effect-owned runtime implementation registry.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use heycode_core::Context;

use crate::{AgentRuntime, AgentRuntimeDescriptor, AgentRuntimeId, AgentRuntimeRegistryError};

struct RuntimeEntry {
    descriptor: AgentRuntimeDescriptor,
    runtime: Arc<dyn AgentRuntime>,
    token: Arc<()>,
}

#[derive(Default)]
struct RegistryState {
    entries: BTreeMap<AgentRuntimeId, RuntimeEntry>,
}

/// Deterministic registry of native/delegated coding-agent runtimes.
#[derive(Clone, Default)]
pub struct AgentRuntimeRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl AgentRuntimeRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one implementation for the owning context lifetime.
    ///
    /// The contributing plugin must declare its exact `agent_runtime` inventory
    /// row before apply. Disposal removes only the token-matching registration.
    ///
    /// # Errors
    /// Duplicate ids or poisoned state fail before publication.
    pub fn register(
        &self,
        context: &Context,
        runtime: Arc<dyn AgentRuntime>,
    ) -> Result<(), AgentRuntimeRegistryError> {
        let descriptor = runtime.descriptor().clone();
        let id = descriptor.id().clone();
        let token = Arc::new(());
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentRuntimeRegistryError::RegistryUnavailable)?;
        if state.entries.contains_key(&id) {
            return Err(AgentRuntimeRegistryError::Duplicate {
                id: id.as_str().to_owned(),
            });
        }
        state.entries.insert(
            id.clone(),
            RuntimeEntry {
                descriptor,
                runtime,
                token: token.clone(),
            },
        );
        drop(state);

        let registration = RuntimeRegistration {
            state: Arc::downgrade(&self.state),
            id,
            token,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Look up a runtime by stable id.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn get(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn AgentRuntime>>, AgentRuntimeRegistryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| AgentRuntimeRegistryError::RegistryUnavailable)?
            .entries
            .iter()
            .find(|(registered, _)| registered.as_str() == id)
            .map(|(_, entry)| entry.runtime.clone()))
    }

    /// Sorted runtime ids.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn ids(&self) -> Result<Vec<String>, AgentRuntimeRegistryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| AgentRuntimeRegistryError::RegistryUnavailable)?
            .entries
            .keys()
            .map(|id| id.as_str().to_owned())
            .collect())
    }

    /// Sorted immutable discovery rows.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn descriptors(&self) -> Result<Vec<AgentRuntimeDescriptor>, AgentRuntimeRegistryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| AgentRuntimeRegistryError::RegistryUnavailable)?
            .entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect())
    }

    /// Whether no runtime implementation is registered.
    ///
    /// # Errors
    /// Poisoned registry state.
    pub fn is_empty(&self) -> Result<bool, AgentRuntimeRegistryError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| AgentRuntimeRegistryError::RegistryUnavailable)?
            .entries
            .is_empty())
    }
}

struct RuntimeRegistration {
    state: Weak<Mutex<RegistryState>>,
    id: AgentRuntimeId,
    token: Arc<()>,
}

impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let remove = state
            .entries
            .get(&self.id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if remove {
            state.entries.remove(&self.id);
        }
    }
}
