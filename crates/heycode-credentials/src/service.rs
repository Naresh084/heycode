//! Effect-owned credential provider registry and precedence resolution.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use heycode_core::Context;
use sha2::{Digest as _, Sha256};

use crate::{
    CredentialDescriptor, CredentialKind, CredentialProvider, CredentialProviderId,
    CredentialQuery, CredentialReference, CredentialSecret, CredentialValidation, CredentialsError,
};

struct ProviderEntry {
    provider: Arc<dyn CredentialProvider>,
    token: Arc<()>,
}

struct CredentialsInner {
    providers: Mutex<Vec<ProviderEntry>>,
    validations: Mutex<
        BTreeMap<(CredentialReference, CredentialKind, CredentialProviderId), ValidationEntry>,
    >,
}

struct ValidationEntry {
    validation: CredentialValidation,
    fingerprint: [u8; 32],
    expires_at_ms: u64,
}

/// Shared ordered credential provider registry.
#[derive(Clone)]
pub struct CredentialsService {
    inner: Arc<CredentialsInner>,
}

impl Default for CredentialsService {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialsService {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CredentialsInner {
                providers: Mutex::new(Vec::new()),
                validations: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Number of currently active credential providers, without inspecting or
    /// resolving any credential reference.
    ///
    /// # Errors
    /// A poisoned registry fails loud.
    pub fn provider_count(&self) -> Result<usize, CredentialsError> {
        Ok(self
            .inner
            .providers
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?
            .len())
    }

    /// Register one provider as a context effect.
    ///
    /// # Errors
    /// Duplicate ids or a poisoned registry fail before publication.
    pub fn register(
        &self,
        context: &Context,
        provider: Arc<dyn CredentialProvider>,
    ) -> Result<(), CredentialsError> {
        let mut providers = self
            .inner
            .providers
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        if providers
            .iter()
            .any(|entry| entry.provider.id() == provider.id())
        {
            return Err(CredentialsError::DuplicateProvider {
                provider: provider.id().as_str().to_owned(),
            });
        }
        let token = Arc::new(());
        let id = provider.id().clone();
        providers.push(ProviderEntry {
            provider,
            token: token.clone(),
        });
        providers.sort_by(|left, right| {
            left.provider
                .precedence()
                .cmp(&right.provider.precedence())
                .then_with(|| left.provider.id().cmp(right.provider.id()))
        });
        drop(providers);
        let registration = ProviderRegistration {
            inner: Arc::downgrade(&self.inner),
            id,
            token,
            active: true,
        };
        context.effect(move || drop(registration));
        Ok(())
    }

    /// Build a safe descriptor without resolving any secret.
    ///
    /// # Errors
    /// Registry/provider failures are redacted and fail loud.
    pub fn describe(
        &self,
        query: &CredentialQuery,
    ) -> Result<CredentialDescriptor, CredentialsError> {
        let providers = self.snapshot()?;
        let mut writable_fallback = None;
        for provider in providers {
            let state = provider
                .inspect(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            if state.configured {
                let validation = self
                    .validation(query, provider.id())?
                    .unwrap_or(state.validation);
                return Ok(CredentialDescriptor {
                    reference: query.reference.clone(),
                    kind: query.kind.clone(),
                    configured: true,
                    source: state.source,
                    provider: Some(provider.id().clone()),
                    writable: state.writable,
                    validation,
                });
            }
            if state.writable && writable_fallback.is_none() {
                writable_fallback = Some(provider.id().clone());
            }
        }
        let mut descriptor = CredentialDescriptor::unconfigured(query.clone());
        descriptor.writable = writable_fallback.is_some();
        descriptor.provider = writable_fallback;
        Ok(descriptor)
    }

    /// Resolve from the highest-precedence configured provider.
    ///
    /// # Errors
    /// Registry/provider failures or an inspection/resolution contradiction.
    pub fn resolve(
        &self,
        query: &CredentialQuery,
    ) -> Result<Option<CredentialSecret>, CredentialsError> {
        for provider in self.snapshot()? {
            let state = provider
                .inspect(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            if !state.configured {
                continue;
            }
            let resolved = provider
                .resolve(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?
                .ok_or_else(|| CredentialsError::InconsistentProvider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            self.invalidate_if_rotated(query, provider.id(), &resolved)?;
            return Ok(Some(resolved));
        }
        Ok(None)
    }

    /// Write through the authoritative configured provider, or the first
    /// writable fallback when none is configured.
    ///
    /// A configured read-only provider blocks fallback writes because the
    /// next resolution would continue reading the shadowing record.
    ///
    /// # Errors
    /// Registry/inspection/provider failures, read-only shadowing, or no
    /// writable provider.
    pub fn write(
        &self,
        query: &CredentialQuery,
        secret: &CredentialSecret,
    ) -> Result<CredentialProviderId, CredentialsError> {
        let providers = self.snapshot()?;
        let mut writable_fallback = None;
        for provider in providers {
            let state = provider
                .inspect(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            if state.configured {
                if !state.writable {
                    return Err(CredentialsError::ShadowedReadOnly {
                        provider: provider.id().as_str().to_owned(),
                        credential_source: state.source,
                    });
                }
                provider
                    .write(query, secret)
                    .map_err(|_| CredentialsError::Provider {
                        provider: provider.id().as_str().to_owned(),
                    })?;
                self.clear_validation(query, provider.id())?;
                return Ok(provider.id().clone());
            }
            if state.writable && writable_fallback.is_none() {
                writable_fallback = Some(provider);
            }
        }
        let Some(provider) = writable_fallback else {
            return Err(CredentialsError::NoWritableProvider);
        };
        provider
            .write(query, secret)
            .map_err(|_| CredentialsError::Provider {
                provider: provider.id().as_str().to_owned(),
            })?;
        self.clear_validation(query, provider.id())?;
        Ok(provider.id().clone())
    }

    /// Delete through the authoritative configured provider.
    ///
    /// # Errors
    /// Registry/inspection/provider failures or read-only shadowing. An
    /// unconfigured reference is an idempotent `Ok(None)`.
    pub fn delete(
        &self,
        query: &CredentialQuery,
    ) -> Result<Option<CredentialProviderId>, CredentialsError> {
        for provider in self.snapshot()? {
            let state = provider
                .inspect(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            if !state.configured {
                continue;
            }
            if !state.writable {
                return Err(CredentialsError::ShadowedReadOnly {
                    provider: provider.id().as_str().to_owned(),
                    credential_source: state.source,
                });
            }
            provider
                .delete(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            self.clear_validation(query, provider.id())?;
            return Ok(Some(provider.id().clone()));
        }
        Ok(None)
    }

    /// Delete every configured writable record for one exact reference.
    ///
    /// Read-only environment, command, ambient runtime and vendor-owned
    /// records are preserved. This is used by explicit heycode logout so a
    /// writable file record cannot remain hidden beneath a higher-precedence
    /// environment credential and silently return on a later launch.
    ///
    /// # Errors
    /// Registry inspection or deletion failures fail loud. An unconfigured or
    /// read-only-only reference is an idempotent empty result.
    pub fn delete_writable(
        &self,
        query: &CredentialQuery,
    ) -> Result<Vec<CredentialProviderId>, CredentialsError> {
        let mut deleted = Vec::new();
        for provider in self.snapshot()? {
            let state = provider
                .inspect(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            if !state.configured || !state.writable {
                continue;
            }
            provider
                .delete(query)
                .map_err(|_| CredentialsError::Provider {
                    provider: provider.id().as_str().to_owned(),
                })?;
            self.clear_validation(query, provider.id())?;
            deleted.push(provider.id().clone());
        }
        Ok(deleted)
    }

    /// Record safe validation metadata for one committed provider record.
    ///
    /// # Errors
    /// A poisoned validation registry fails loud.
    pub fn record_validation(
        &self,
        query: &CredentialQuery,
        provider: &CredentialProviderId,
        validation: CredentialValidation,
        secret: &CredentialSecret,
        ttl_ms: u64,
    ) -> Result<(), CredentialsError> {
        let mut validations = self
            .inner
            .validations
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        validations.insert(
            (
                query.reference.clone(),
                query.kind.clone(),
                provider.clone(),
            ),
            ValidationEntry {
                validation,
                fingerprint: fingerprint(secret),
                expires_at_ms: now_ms().saturating_add(ttl_ms),
            },
        );
        Ok(())
    }

    fn validation(
        &self,
        query: &CredentialQuery,
        provider: &CredentialProviderId,
    ) -> Result<Option<CredentialValidation>, CredentialsError> {
        let validations = self
            .inner
            .validations
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        let entry = validations.get(&(
            query.reference.clone(),
            query.kind.clone(),
            provider.clone(),
        ));
        Ok(entry.map(|entry| {
            if now_ms() >= entry.expires_at_ms {
                CredentialValidation::Stale {
                    checked_at_ms: checked_at(&entry.validation),
                }
            } else {
                entry.validation.clone()
            }
        }))
    }

    fn invalidate_if_rotated(
        &self,
        query: &CredentialQuery,
        provider: &CredentialProviderId,
        secret: &CredentialSecret,
    ) -> Result<(), CredentialsError> {
        let mut validations = self
            .inner
            .validations
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        let key = (
            query.reference.clone(),
            query.kind.clone(),
            provider.clone(),
        );
        if validations
            .get(&key)
            .is_some_and(|entry| entry.fingerprint != fingerprint(secret))
        {
            validations.remove(&key);
        }
        Ok(())
    }

    fn clear_validation(
        &self,
        query: &CredentialQuery,
        provider: &CredentialProviderId,
    ) -> Result<(), CredentialsError> {
        let mut validations = self
            .inner
            .validations
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        validations.remove(&(
            query.reference.clone(),
            query.kind.clone(),
            provider.clone(),
        ));
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<Arc<dyn CredentialProvider>>, CredentialsError> {
        let providers = self
            .inner
            .providers
            .lock()
            .map_err(|_| CredentialsError::RegistryUnavailable)?;
        Ok(providers
            .iter()
            .map(|entry| entry.provider.clone())
            .collect())
    }
}

fn fingerprint(secret: &CredentialSecret) -> [u8; 32] {
    Sha256::digest(secret.expose().as_bytes()).into()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0)
}

fn checked_at(validation: &CredentialValidation) -> u64 {
    match validation {
        CredentialValidation::Valid { checked_at_ms }
        | CredentialValidation::Invalid { checked_at_ms, .. }
        | CredentialValidation::Stale { checked_at_ms } => *checked_at_ms,
        CredentialValidation::Unknown => 0,
    }
}

struct ProviderRegistration {
    inner: Weak<CredentialsInner>,
    id: CredentialProviderId,
    token: Arc<()>,
    active: bool,
}

impl ProviderRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut providers) = inner.providers.lock() else {
            return;
        };
        providers.retain(|entry| {
            entry.provider.id() != &self.id || !Arc::ptr_eq(&entry.token, &self.token)
        });
        drop(providers);
        if let Ok(mut validations) = inner.validations.lock() {
            validations.retain(|(_reference, _kind, provider), _| provider != &self.id);
        }
    }
}

impl Drop for ProviderRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}
