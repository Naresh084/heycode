use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use heycode_config::imports::{
    ConfirmedImportBatch, ImportCommitOutcome, ImportGeneration, ImportReceipt, ImportStore,
    PreparedImportBatch,
};
use heycode_core::ServiceKey;
use tokio_util::sync::CancellationToken;

use super::*;

/// Human configuration-import service. No model tool is registered for this owner.
pub const SERVICE_CONFIG_IMPORT: ServiceKey = ServiceKey::new("config-import");

/// Private prepared bytes and the exact safe confirmation projection.
pub struct PreparedConfigImport {
    preview: ConfigImportPreview,
    batch: PreparedImportBatch,
    inputs: Arc<InventoryInputs>,
    baseline: ImportHostSnapshot,
    target: ImportTarget,
    keys: Vec<ImportResourceKey>,
    owner: String,
}

impl std::fmt::Debug for PreparedConfigImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedConfigImport")
            .field("preview", &self.preview)
            .finish_non_exhaustive()
    }
}

impl PreparedConfigImport {
    /// Complete metadata-only view for deliberate human confirmation.
    pub fn preview(&self) -> &ConfigImportPreview {
        &self.preview
    }
}

/// A human-confirmed operation. Retain this handle until its outcome is reconciled.
pub struct ConfirmedConfigImport {
    batch: ConfirmedImportBatch,
    inputs: Arc<InventoryInputs>,
    baseline: ImportHostSnapshot,
    target: ImportTarget,
    keys: Vec<ImportResourceKey>,
    owner: String,
}

impl std::fmt::Debug for ConfirmedConfigImport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmedConfigImport")
            .field("transaction_id", &self.transaction_id())
            .finish_non_exhaustive()
    }
}

impl ConfirmedConfigImport {
    /// Stable receipt identity for reconciliation.
    pub fn transaction_id(&self) -> &str {
        self.batch.transaction_id()
    }
}

struct ActiveCommit {
    id: String,
    cancellation: CancellationToken,
}

/// Scope-aware import planning and commit coordination for one composition.
pub struct ConfigImportService {
    store: ImportStore,
    host: Arc<dyn ConfigImportHost>,
    pinned: Arc<ImportGeneration>,
    owner: String,
    closed: AtomicBool,
    active: Mutex<Option<ActiveCommit>>,
    unresolved: Mutex<Option<String>>,
}

impl std::fmt::Debug for ConfigImportService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigImportService")
            .field("active_revision", &self.pinned.revision())
            .finish_non_exhaustive()
    }
}

impl ConfigImportService {
    /// Bind the actual store, host authority and the exact generation mounted in this world.
    pub fn new(
        store: ImportStore,
        host: Arc<dyn ConfigImportHost>,
        pinned: Arc<ImportGeneration>,
    ) -> Self {
        Self {
            store,
            host,
            pinned,
            owner: uuid::Uuid::new_v4().to_string(),
            closed: AtomicBool::new(false),
            active: Mutex::new(None),
            unresolved: Mutex::new(None),
        }
    }

    /// The generation this composition actually activated. Committing never mutates it.
    pub fn active_generation(&self) -> Arc<ImportGeneration> {
        self.pinned.clone()
    }

    /// Scan fixed product paths under an explicit root, with zero persistence/execution.
    pub fn discover(
        &self,
        request: &ConfigImportRequest,
    ) -> Result<ConfigImportInventory, ImportError> {
        self.ensure_open()?;
        self.host.admit_source(request)?;
        let mut inventory = parser::discover(request)?;
        let current = self.store.snapshot()?;
        let keys = inventory
            .candidates
            .iter()
            .flatten()
            .filter(|candidate| valid_name(&candidate.name))
            .map(|candidate| ImportResourceKey {
                kind: candidate.kind,
                name: candidate.name.clone(),
            })
            .collect::<Vec<_>>();
        let host = self.host.snapshot(&inventory.target, &keys)?;
        for (row, candidate) in inventory.rows.iter_mut().zip(&inventory.candidates) {
            let Some(candidate) = candidate else {
                continue;
            };
            let Ok(resource) = candidate.resource(None) else {
                continue;
            };
            let key = ImportResourceKey {
                kind: resource.kind(),
                name: resource.name().to_owned(),
            };
            if host.conflicts.contains(&key) {
                row.status = ImportItemStatus::Conflict;
                row.reason = ImportItemReason::ExistingDestination;
            } else if let Some(existing) = current.entries().iter().find(|entry| {
                entry.target() == &inventory.target
                    && entry.resource().kind() == resource.kind()
                    && entry.resource().name() == resource.name()
            }) {
                row.status = if existing.resource().same_content(&resource) {
                    ImportItemStatus::Duplicate
                } else {
                    ImportItemStatus::Conflict
                };
                row.reason = ImportItemReason::ExistingDestination;
            }
        }
        self.ensure_open()?;
        Ok(inventory)
    }

    /// Freeze exactly the explicitly selected supported rows. Unlisted rows stay omitted.
    pub fn prepare(
        &self,
        inventory: &ConfigImportInventory,
        decisions: &[ConfigImportDecision],
    ) -> Result<PreparedConfigImport, ImportError> {
        self.ensure_open()?;
        self.ensure_reconciled()?;
        inventory.target.recheck()?;
        inventory.inputs.recheck()?;
        if decisions.len() > heycode_config::imports::MAX_RESOURCES {
            return Err(ImportError::Limit);
        }
        let mut selected = BTreeSet::new();
        let mut resources = Vec::new();
        let mut actions = Vec::new();
        for decision in decisions {
            let (id, rename, keep) = match decision {
                ConfigImportDecision::Add { item_id } => (item_id, None, false),
                ConfigImportDecision::Rename { item_id, name } => {
                    (item_id, Some(name.as_str()), false)
                }
                ConfigImportDecision::KeepExisting { item_id } => (item_id, None, true),
            };
            if !selected.insert(id) {
                return Err(ImportError::InvalidDocument);
            }
            let index = inventory
                .rows
                .iter()
                .position(|row| &row.id == id)
                .ok_or(ImportError::InvalidDocument)?;
            if keep {
                continue;
            }
            let candidate = inventory.candidates[index]
                .as_ref()
                .ok_or(ImportError::InvalidDocument)?;
            let resource = candidate.resource(rename)?;
            if parser::suspected_secret(resource.name()) {
                return Err(ImportError::UnsafePath);
            }
            actions.push(ConfigImportAction {
                item_id: id.clone(),
                kind: resource.kind(),
                name: resource.name().to_owned(),
            });
            resources.push(resource);
        }
        if resources.is_empty() {
            return Err(ImportError::InvalidDocument);
        }
        let keys = resources
            .iter()
            .map(|resource| ImportResourceKey {
                kind: resource.kind(),
                name: resource.name().to_owned(),
            })
            .collect::<Vec<_>>();
        let baseline = self.host.snapshot(&inventory.target, &keys)?;
        if keys.iter().any(|key| baseline.conflicts.contains(key)) {
            return Err(ImportError::Conflict);
        }
        let batch = self
            .store
            .prepare(inventory.target.clone(), resources, &baseline.revision)?;
        let preview = ConfigImportPreview {
            inventory_id: inventory.id.clone(),
            digest: batch.digest().to_owned(),
            scope: if matches!(inventory.target, ImportTarget::User) {
                "user"
            } else {
                "project"
            }
            .to_owned(),
            baseline_revision: batch.baseline_revision(),
            duplicates: actions.len() - batch.added(),
            actions,
            requires_recomposition: true,
        };
        Ok(PreparedConfigImport {
            preview,
            batch,
            inputs: inventory.inputs.clone(),
            baseline,
            target: inventory.target.clone(),
            keys,
            owner: self.owner.clone(),
        })
    }

    /// Consume a plan at a human frontend/CLI boundary after the exact digest is reviewed.
    /// This method is never exposed as a model tool or invoked by source content.
    pub fn confirm_human(
        &self,
        prepared: PreparedConfigImport,
        reviewed_digest: &str,
    ) -> Result<ConfirmedConfigImport, ImportError> {
        self.ensure_open()?;
        if prepared.owner != self.owner {
            return Err(ImportError::Stale);
        }
        Ok(ConfirmedConfigImport {
            batch: prepared.batch.confirm(reviewed_digest)?,
            inputs: prepared.inputs,
            baseline: prepared.baseline,
            target: prepared.target,
            keys: prepared.keys,
            owner: prepared.owner,
        })
    }

    /// Persist the exact confirmed batch after source/host CAS checks. Never activates it.
    pub fn commit(
        &self,
        confirmed: &ConfirmedConfigImport,
        cancellation: &CancellationToken,
    ) -> Result<ImportCommitOutcome, ImportError> {
        if confirmed.owner != self.owner {
            return Err(ImportError::Stale);
        }
        self.ensure_reconciled()?;
        let operation = cancellation.child_token();
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut active = self.active.lock().map_err(|_| ImportError::Unavailable)?;
            self.ensure_open()?;
            if active.is_some() {
                return Err(ImportError::Busy);
            }
            *active = Some(ActiveCommit {
                id: id.clone(),
                cancellation: operation.clone(),
            });
        }
        let _guard = OperationGuard {
            active: &self.active,
            id,
        };
        let result = self.store.commit(&confirmed.batch, &operation, || {
            self.ensure_open()?;
            confirmed.target.recheck()?;
            confirmed.inputs.recheck()?;
            let current = self.host.snapshot(&confirmed.target, &confirmed.keys)?;
            if current != confirmed.baseline {
                return Err(ImportError::Stale);
            }
            Ok(())
        })?;
        if let ImportCommitOutcome::RecoveryRequired { transaction_id } = &result {
            match self.unresolved.lock() {
                Ok(mut unresolved) => *unresolved = Some(transaction_id.clone()),
                // Keep the uncertain outcome visible even if local bookkeeping
                // fails. Closing prevents a second operation from being started.
                Err(_) => self.close(),
            }
        }
        Ok(result)
    }

    /// Resolve a possibly published operation. An unavailable store keeps the retry fence.
    pub fn reconcile(
        &self,
        confirmed: &ConfirmedConfigImport,
    ) -> Result<Option<ImportReceipt>, ImportError> {
        if confirmed.owner != self.owner {
            return Err(ImportError::Stale);
        }
        let receipt = self.store.reconcile(confirmed.transaction_id())?;
        let mut unresolved = self
            .unresolved
            .lock()
            .map_err(|_| ImportError::Unavailable)?;
        if unresolved.as_deref() == Some(confirmed.transaction_id()) {
            *unresolved = None;
        }
        Ok(receipt)
    }

    /// Withdraw this generation's human owner and cancel any operation before publication.
    /// A commit that already published still returns its committed receipt.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(active) = self.active.lock()
            && let Some(active) = active.as_ref()
        {
            active.cancellation.cancel();
        }
    }

    fn ensure_open(&self) -> Result<(), ImportError> {
        if self.closed.load(Ordering::SeqCst) {
            Err(ImportError::Authority)
        } else {
            Ok(())
        }
    }
    fn ensure_reconciled(&self) -> Result<(), ImportError> {
        if self
            .unresolved
            .lock()
            .map_err(|_| ImportError::Unavailable)?
            .is_some()
        {
            Err(ImportError::Busy)
        } else {
            Ok(())
        }
    }
}

struct OperationGuard<'a> {
    active: &'a Mutex<Option<ActiveCommit>>,
    id: String,
}
impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock()
            && active.as_ref().is_some_and(|row| row.id == self.id)
        {
            *active = None;
        }
    }
}
