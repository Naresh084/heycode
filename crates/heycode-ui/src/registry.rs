//! Effect-owned typed opaque contribution registry.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use crate::theme::{Theme, builtin_themes};
use crate::{UiContributionDescriptor, UiContributionId, UiRegistryError, UiSlot};

struct UiEntry {
    descriptor: UiContributionDescriptor,
    capability: Arc<dyn Any + Send + Sync>,
    token: Arc<()>,
}

struct ThemeEntry {
    theme: Theme,
    token: Arc<()>,
}

#[derive(Default)]
struct UiState {
    entries: BTreeMap<(UiSlot, UiContributionId), UiEntry>,
    themes: BTreeMap<String, ThemeEntry>,
}

/// Shared metadata and typed-handle registry for UI surfaces.
#[derive(Clone, Default)]
pub struct UiRegistry {
    inner: Arc<Mutex<UiState>>,
}

impl UiRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed capability during the owning plugin's `apply` call.
    /// The live exact row is attributed through `Context::contribute` and the
    /// registration is removed during context shutdown/rollback.
    ///
    /// # Errors
    /// Same-slot/id collision, inventory publication or poisoned state.
    pub fn register<T: Send + Sync + 'static>(
        &self,
        context: &heycode_core::Context,
        descriptor: UiContributionDescriptor,
        capability: Arc<T>,
    ) -> Result<(), UiRegistryError> {
        let key = (descriptor.slot(), descriptor.id().clone());
        let identity = descriptor.inventory_name();
        context
            .contribute(heycode_core::ContributionKind::UiSlot, identity.clone())
            .map_err(|error| UiRegistryError::Inventory {
                message: error.to_string(),
            })?;
        let mut state = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?;
        if state.entries.contains_key(&key) {
            return Err(UiRegistryError::Duplicate { identity });
        }
        let token = Arc::new(());
        let registration = UiRegistration {
            inner: Arc::downgrade(&self.inner),
            key: key.clone(),
            token: token.clone(),
            active: true,
        };
        state.entries.insert(
            key,
            UiEntry {
                descriptor,
                capability,
                token,
            },
        );
        drop(state);
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Metadata snapshot ordered by slot, descending priority and id.
    ///
    /// # Errors
    /// Poisoned state fails loud.
    pub fn snapshot(&self) -> Result<Vec<UiContributionDescriptor>, UiRegistryError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?;
        let mut descriptors: Vec<_> = state
            .entries
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect();
        descriptors.sort_by(|left, right| {
            left.slot()
                .cmp(&right.slot())
                .then_with(|| right.priority().cmp(&left.priority()))
                .then_with(|| left.id().cmp(right.id()))
        });
        Ok(descriptors)
    }

    /// Typed capability lookup. A wrong type returns `None`, matching the
    /// context service-map boundary.
    ///
    /// # Errors
    /// Poisoned registry state fails loud.
    pub fn get<T: Send + Sync + 'static>(
        &self,
        slot: UiSlot,
        id: &UiContributionId,
    ) -> Result<Option<Arc<T>>, UiRegistryError> {
        let capability = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?
            .entries
            .get(&(slot, id.clone()))
            .map(|entry| entry.capability.clone());
        Ok(capability.and_then(|capability| capability.downcast::<T>().ok()))
    }

    /// Register a theme during the owning plugin's `apply` call.
    ///
    /// Themes live here rather than behind their own service key because a
    /// theme is a UI contribution and service `"ui"` is already composed — a
    /// plugin-contributed theme therefore needs no composition-root change.
    /// Registration is an effect, so a contributed theme disappears on
    /// rollback and shutdown exactly like a panel.
    ///
    /// No `Context::contribute` row is taken: `ContributionKind` has no theme
    /// variant, and claiming `UiSlot` for something that occupies no slot
    /// would make the inventory say something specific and wrong.
    ///
    /// # Errors
    /// An id already held by a built-in or another contributed theme is a
    /// [`UiRegistryError::Duplicate`]; poisoned state fails loud.
    pub fn register_theme(
        &self,
        context: &heycode_core::Context,
        theme: Theme,
    ) -> Result<(), UiRegistryError> {
        let registration = self.register_theme_owned(theme)?;
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Register a theme and return its exact ownership handle.
    ///
    /// Aggregate declarative activation uses this lower-level form so its
    /// bridge, rather than the domain adapter, owns the disposer. Ordinary UI
    /// plugins should prefer [`Self::register_theme`].
    ///
    /// # Errors
    /// Duplicate ids or poisoned registry state fail before publication.
    pub fn register_theme_owned(&self, theme: Theme) -> Result<ThemeRegistration, UiRegistryError> {
        let id = theme.id().as_str().to_owned();
        let mut state = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?;
        let taken = state.themes.contains_key(&id)
            || builtin_themes()?
                .iter()
                .any(|builtin| builtin.id().as_str() == id);
        if taken {
            return Err(UiRegistryError::Duplicate {
                identity: format!("theme:{id}"),
            });
        }
        let token = Arc::new(());
        let registration = ThemeRegistration {
            inner: Arc::downgrade(&self.inner),
            id: id.clone(),
            token: token.clone(),
            active: true,
        };
        state.themes.insert(id, ThemeEntry { theme, token });
        Ok(registration)
    }

    /// Built-in themes plus every live contributed theme, ordered by id.
    ///
    /// # Errors
    /// Poisoned state fails loud.
    pub fn themes(&self) -> Result<Vec<Theme>, UiRegistryError> {
        let contributed: Vec<Theme> = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?
            .themes
            .values()
            .map(|entry| entry.theme.clone())
            .collect();
        let mut themes = builtin_themes()?;
        themes.extend(contributed);
        themes.sort_by(|left, right| left.id().cmp(right.id()));
        Ok(themes)
    }

    /// One theme by id, contributed or built-in.
    ///
    /// # Errors
    /// Poisoned state fails loud.
    pub fn theme(&self, id: &str) -> Result<Option<Theme>, UiRegistryError> {
        let contributed = self
            .inner
            .lock()
            .map_err(|_| UiRegistryError::RegistryUnavailable)?
            .themes
            .get(id)
            .map(|entry| entry.theme.clone());
        if let Some(theme) = contributed {
            return Ok(Some(theme));
        }
        Ok(builtin_themes()?
            .into_iter()
            .find(|theme| theme.id().as_str() == id))
    }
}

struct UiRegistration {
    inner: Weak<Mutex<UiState>>,
    key: (UiSlot, UiContributionId),
    token: Arc<()>,
    active: bool,
}

impl Drop for UiRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.lock() else {
            return;
        };
        let remove = state
            .entries
            .get(&self.key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if remove {
            state.entries.remove(&self.key);
        }
    }
}

/// Exact ownership handle for one contributed theme.
pub struct ThemeRegistration {
    inner: Weak<Mutex<UiState>>,
    id: String,
    token: Arc<()>,
    active: bool,
}

impl Drop for ThemeRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.lock() else {
            return;
        };
        let remove = state
            .themes
            .get(&self.id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token));
        if remove {
            state.themes.remove(&self.id);
        }
    }
}
