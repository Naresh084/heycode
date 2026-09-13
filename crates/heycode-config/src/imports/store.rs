use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cap_fs_ext::{FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::fs::{Anchor, digest, read_optional};
use super::{
    ImportCommitOutcome, ImportError, ImportGeneration, ImportProduct, ImportReceipt,
    ImportResource, ImportResourceKind, ImportTarget, ImportedEntry, MAX_RESOURCES,
    MAX_TOTAL_BYTES, SCHEMA_VERSION,
};

const MAX_DISK_BYTES: usize = MAX_TOTAL_BYTES * 2 + 1024 * 1024;
const POINTER: &str = "current.json";

/// Fixed owner-relative import store. Constructing and reading it never creates files.
#[derive(Clone)]
pub struct ImportStore {
    anchor: Anchor,
}

impl std::fmt::Debug for ImportStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImportStore(<private>)")
    }
}

/// Frozen complete next generation and the digest that must be reviewed.
pub struct PreparedImportBatch {
    baseline: Option<String>,
    next: DiskGeneration,
    bytes: Vec<u8>,
    digest: String,
    owner: PathBuf,
}

impl std::fmt::Debug for PreparedImportBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedImportBatch")
            .field("digest", &self.digest)
            .field("revision", &self.next.revision)
            .finish_non_exhaustive()
    }
}

impl PreparedImportBatch {
    /// Digest of this exact selection, baseline and host revision.
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// Number of added rows, excluding exact duplicates.
    pub fn added(&self) -> usize {
        self.receipt().added
    }
    /// Current destination revision the plan was prepared against.
    pub fn baseline_revision(&self) -> u64 {
        self.next.revision - 1
    }
    /// Consume this frozen batch only after the host has obtained human confirmation.
    pub fn confirm(self, reviewed_digest: &str) -> Result<ConfirmedImportBatch, ImportError> {
        if reviewed_digest != self.digest {
            return Err(ImportError::ConfirmationRequired);
        }
        Ok(ConfirmedImportBatch { prepared: self })
    }
    fn receipt(&self) -> &ImportReceipt {
        // Prepared batches always append exactly one receipt.
        &self.next.receipts[self.next.receipts.len() - 1]
    }
}

/// A confirmed, private operation, retained for reconciliation rather than blind retry.
pub struct ConfirmedImportBatch {
    prepared: PreparedImportBatch,
}

impl std::fmt::Debug for ConfirmedImportBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmedImportBatch")
            .field("transaction_id", &self.transaction_id())
            .finish_non_exhaustive()
    }
}

impl ConfirmedImportBatch {
    /// Idempotency identity, safe to retain in an unresolved-operation receipt.
    pub fn transaction_id(&self) -> &str {
        &self.prepared.next.id
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    schema_version: u32,
    revision: u64,
    generation: String,
    sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskEntry {
    target: ImportTarget,
    kind: ImportResourceKind,
    name: String,
    payload: String,
    product: ImportProduct,
    adapter_schema: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskGeneration {
    schema_version: u32,
    id: String,
    revision: u64,
    entries: Vec<DiskEntry>,
    receipts: Vec<ImportReceipt>,
}

impl ImportStore {
    /// Bind `<heycode-home>/state/config-imports` without creating it.
    pub fn new(heycode_home: &Path) -> Result<Self, ImportError> {
        Ok(Self {
            anchor: Anchor::bind(&heycode_home.join("state/config-imports"), true)?,
        })
    }

    /// Read one fully verified immutable generation. Readers never follow a moving object.
    pub fn snapshot(&self) -> Result<ImportGeneration, ImportError> {
        let Some(directory) = self.anchor.open(Path::new(""), false)? else {
            return Ok(ImportGeneration::default());
        };
        load(&directory)
    }

    /// Freeze a complete additive transaction against the currently published generation.
    /// Native conflicts and source/schema validation belong to the host before this call.
    pub fn prepare(
        &self,
        target: ImportTarget,
        resources: Vec<ImportResource>,
        host_revision: &str,
    ) -> Result<PreparedImportBatch, ImportError> {
        target.validate()?;
        target.recheck()?;
        if resources.is_empty() || resources.len() > MAX_RESOURCES || host_revision.len() > 4096 {
            return Err(ImportError::Limit);
        }
        let current = self.snapshot()?;
        let mut entries = current.entries.iter().map(disk_entry).collect::<Vec<_>>();
        let mut selected = BTreeSet::new();
        let mut added = 0;
        for resource in resources {
            if !selected.insert((resource.kind, resource.name.clone())) {
                return Err(ImportError::Conflict);
            }
            if let Some(existing) = current.entries.iter().find(|entry| {
                entry.target == target
                    && entry.resource.kind == resource.kind
                    && entry.resource.name == resource.name
            }) {
                if !existing.resource.same_content(&resource) {
                    return Err(ImportError::Conflict);
                }
                continue;
            }
            entries.push(disk_entry(&ImportedEntry {
                target: target.clone(),
                resource,
            }));
            added += 1;
        }
        if entries.len() > MAX_RESOURCES
            || entries
                .iter()
                .map(|entry| entry.payload.len())
                .sum::<usize>()
                > MAX_TOTAL_BYTES
            || (added > 0 && current.receipts.len() >= MAX_RESOURCES)
        {
            return Err(ImportError::Limit);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let revision = current.revision.checked_add(1).ok_or(ImportError::Limit)?;
        // Source bytes never become public digest input. Only selected native adapter
        // bytes, a random operation id, and private host baseline participate.
        let digest_input =
            serde_json::to_vec(&(&id, current.id(), revision, &entries, host_revision))
                .map_err(|_| ImportError::InvalidDocument)?;
        let plan_digest = digest(&digest_input);
        let mut receipts = current.receipts;
        receipts.push(ImportReceipt {
            transaction_id: id.clone(),
            reviewed_digest: plan_digest.clone(),
            revision,
            added,
        });
        let next = DiskGeneration {
            schema_version: SCHEMA_VERSION,
            id,
            revision,
            entries,
            receipts,
        };
        let bytes = serde_json::to_vec(&next).map_err(|_| ImportError::InvalidDocument)?;
        if bytes.len() > MAX_DISK_BYTES {
            return Err(ImportError::Limit);
        }
        Ok(PreparedImportBatch {
            baseline: current.id,
            next,
            bytes,
            digest: plan_digest,
            owner: self.anchor.path(),
        })
    }

    /// Revalidate the host under the store lock, then publish one complete generation.
    /// The same confirmed handle is safe to retry after a resolved pre-publication error.
    /// A RecoveryRequired outcome must first be passed to `reconcile`.
    pub fn commit(
        &self,
        confirmed: &ConfirmedImportBatch,
        cancellation: &CancellationToken,
        recheck_host: impl FnOnce() -> Result<(), ImportError>,
    ) -> Result<ImportCommitOutcome, ImportError> {
        self.commit_inner(confirmed, cancellation, recheck_host, Fault::None)
    }

    /// Reconcile a possibly published operation and acknowledge directory durability.
    /// `None` proves this operation is not in the current published generation.
    pub fn reconcile(&self, transaction_id: &str) -> Result<Option<ImportReceipt>, ImportError> {
        validate_uuid(transaction_id)?;
        let Some(directory) = self.anchor.open(Path::new(""), false)? else {
            return Ok(None);
        };
        let _lock = lock(&directory)?;
        let snapshot = load(&directory)?;
        let result = snapshot.receipt(transaction_id).cloned();
        if result.is_some() {
            sync_directory(&directory)?;
        }
        Ok(result)
    }

    fn commit_inner(
        &self,
        confirmed: &ConfirmedImportBatch,
        cancellation: &CancellationToken,
        recheck_host: impl FnOnce() -> Result<(), ImportError>,
        fault: Fault,
    ) -> Result<ImportCommitOutcome, ImportError> {
        let plan = &confirmed.prepared;
        if plan.owner != self.anchor.path() {
            return Err(ImportError::Stale);
        }
        // Check a committed receipt before cancellation: a late cancel never changes
        // a successfully published transaction into a cancelled one.
        if let Some(receipt) = self.snapshot()?.receipt(&plan.next.id) {
            if receipt.reviewed_digest != plan.digest {
                return Err(ImportError::Stale);
            }
            return Ok(match self.reconcile(&plan.next.id) {
                Ok(Some(receipt)) => ImportCommitOutcome::Committed(receipt),
                Ok(None) | Err(_) => ImportCommitOutcome::RecoveryRequired {
                    transaction_id: plan.next.id.clone(),
                },
            });
        }
        if cancellation.is_cancelled() {
            return Ok(ImportCommitOutcome::CancelledBeforePublication);
        }
        let directory = self
            .anchor
            .open(Path::new(""), true)?
            .ok_or(ImportError::Unavailable)?;
        let _lock = lock(&directory)?;
        let current = load(&directory)?;
        if let Some(receipt) = current.receipt(&plan.next.id) {
            if receipt.reviewed_digest != plan.digest {
                return Err(ImportError::Stale);
            }
            return Ok(if sync_directory(&directory).is_ok() {
                ImportCommitOutcome::Committed(receipt.clone())
            } else {
                ImportCommitOutcome::RecoveryRequired {
                    transaction_id: plan.next.id.clone(),
                }
            });
        }
        if current.id != plan.baseline {
            return Err(ImportError::Stale);
        }
        recheck_host()?;
        if plan.receipt().added == 0 {
            return Ok(ImportCommitOutcome::Unchanged {
                revision: current.revision,
            });
        }
        for entry in &plan.next.entries {
            // Other projects in an older generation may legitimately be offline;
            // only the selected transaction target is rechecked by the host.
            entry.target.validate()?;
        }
        if cancellation.is_cancelled() {
            return Ok(ImportCommitOutcome::CancelledBeforePublication);
        }
        let object = format!("{}.json", plan.next.id);
        write_frozen(&directory, &object, &plan.bytes)?;
        sync_directory(&directory)?;
        if matches!(fault, Fault::BeforePublication) {
            return Err(ImportError::Unavailable);
        }
        let pointer = Pointer {
            schema_version: SCHEMA_VERSION,
            revision: plan.next.revision,
            generation: plan.next.id.clone(),
            sha256: digest(&plan.bytes),
        };
        let pointer_bytes =
            serde_json::to_vec(&pointer).map_err(|_| ImportError::InvalidDocument)?;
        let staging = format!("{}.pointer.tmp", uuid::Uuid::new_v4());
        write_frozen(&directory, &staging, &pointer_bytes)?;
        if cancellation.is_cancelled() {
            let _ = directory.remove_file(&staging);
            return Ok(ImportCommitOutcome::CancelledBeforePublication);
        }
        if directory.rename(&staging, &directory, POINTER).is_err() {
            let _ = directory.remove_file(&staging);
            // A platform or filesystem may return an error after publication.
            return Ok(ImportCommitOutcome::RecoveryRequired {
                transaction_id: plan.next.id.clone(),
            });
        }
        if matches!(fault, Fault::CancelAfterPublication) {
            cancellation.cancel();
        }
        if matches!(fault, Fault::AfterPublication) || sync_directory(&directory).is_err() {
            return Ok(ImportCommitOutcome::RecoveryRequired {
                transaction_id: plan.next.id.clone(),
            });
        }
        Ok(ImportCommitOutcome::Committed(plan.receipt().clone()))
    }
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum Fault {
    None,
    BeforePublication,
    AfterPublication,
    CancelAfterPublication,
}

fn disk_entry(entry: &ImportedEntry) -> DiskEntry {
    DiskEntry {
        target: entry.target.clone(),
        kind: entry.resource.kind,
        name: entry.resource.name.clone(),
        payload: entry.resource.payload.clone(),
        product: entry.resource.product,
        adapter_schema: SCHEMA_VERSION,
    }
}

fn load(directory: &Dir) -> Result<ImportGeneration, ImportError> {
    let Some(bytes) = read_optional(directory, Path::new(POINTER), 4096)? else {
        return Ok(ImportGeneration::default());
    };
    let pointer: Pointer =
        serde_json::from_slice(&bytes).map_err(|_| ImportError::InvalidDocument)?;
    if pointer.schema_version != SCHEMA_VERSION
        || pointer.revision == 0
        || !valid_digest(&pointer.sha256)
    {
        return Err(ImportError::InvalidDocument);
    }
    validate_uuid(&pointer.generation)?;
    let object = format!("{}.json", pointer.generation);
    let bytes = read_optional(directory, Path::new(&object), MAX_DISK_BYTES)?
        .ok_or(ImportError::InvalidDocument)?;
    if digest(&bytes) != pointer.sha256 {
        return Err(ImportError::InvalidDocument);
    }
    let disk: DiskGeneration =
        serde_json::from_slice(&bytes).map_err(|_| ImportError::InvalidDocument)?;
    if disk.schema_version != SCHEMA_VERSION
        || disk.id != pointer.generation
        || disk.revision != pointer.revision
        || disk.entries.len() > MAX_RESOURCES
        || disk.receipts.len() > MAX_RESOURCES
        || disk.receipts.is_empty()
    {
        return Err(ImportError::InvalidDocument);
    }
    let mut entries: Vec<ImportedEntry> = Vec::new();
    let mut total = 0usize;
    for entry in disk.entries {
        if entry.adapter_schema != SCHEMA_VERSION {
            return Err(ImportError::InvalidDocument);
        }
        entry.target.validate()?;
        total = total
            .checked_add(entry.payload.len())
            .ok_or(ImportError::Limit)?;
        if total > MAX_TOTAL_BYTES {
            return Err(ImportError::Limit);
        }
        let resource = ImportResource::new(entry.kind, entry.name, entry.payload, entry.product)?;
        if entries.iter().any(|old| {
            old.target == entry.target
                && old.resource.kind == resource.kind
                && old.resource.name == resource.name
        }) {
            return Err(ImportError::Conflict);
        }
        entries.push(ImportedEntry {
            target: entry.target,
            resource,
        });
    }
    let mut receipts = BTreeSet::new();
    for (index, receipt) in disk.receipts.iter().enumerate() {
        validate_uuid(&receipt.transaction_id)?;
        if !valid_digest(&receipt.reviewed_digest)
            || receipt.revision != index as u64 + 1
            || receipt.added > MAX_RESOURCES
            || !receipts.insert(receipt.transaction_id.clone())
        {
            return Err(ImportError::InvalidDocument);
        }
    }
    let last = disk.receipts.last().ok_or(ImportError::InvalidDocument)?;
    if last.transaction_id != disk.id || last.revision != disk.revision {
        return Err(ImportError::InvalidDocument);
    }
    Ok(ImportGeneration {
        id: Some(disk.id),
        revision: disk.revision,
        entries,
        receipts: disk.receipts,
    })
}

fn validate_uuid(id: &str) -> Result<(), ImportError> {
    if uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err(ImportError::UnsafePath)
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn lock(directory: &Dir) -> Result<std::fs::File, ImportError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = directory
        .open_with("writer.lock", &options)
        .map_err(|_| ImportError::UnsafePath)?;
    let meta = file.metadata().map_err(|_| ImportError::Unavailable)?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(ImportError::UnsafePath);
    }
    let file = file.into_std();
    file.try_lock().map_err(|_| ImportError::Busy)?;
    Ok(file)
}

fn write_frozen(directory: &Dir, name: &str, bytes: &[u8]) -> Result<(), ImportError> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut file = match directory.open_with(name, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_optional(directory, Path::new(name), MAX_DISK_BYTES)?.as_deref() == Some(bytes)
            {
                return Ok(());
            }
            return Err(ImportError::Conflict);
        }
        Err(_) => return Err(ImportError::Unavailable),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(cap_std::fs::Permissions::from_std(
            std::fs::Permissions::from_mode(0o600),
        ))
        .map_err(|_| ImportError::Unavailable)?;
    }
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        let _ = directory.remove_file(name);
        return Err(ImportError::Unavailable);
    }
    Ok(())
}

fn sync_directory(directory: &Dir) -> Result<(), ImportError> {
    directory
        .try_clone()
        .map_err(|_| ImportError::Unavailable)?
        .into_std_file()
        .sync_all()
        .map_err(|_| ImportError::Unavailable)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn resource(name: &str, text: &str) -> ImportResource {
        ImportResource::new(
            ImportResourceKind::Instructions,
            name.to_owned(),
            text.to_owned(),
            ImportProduct::Codex,
        )
        .unwrap()
    }
    fn confirmed(store: &ImportStore, name: &str) -> ConfirmedImportBatch {
        let plan = store
            .prepare(
                ImportTarget::User,
                vec![resource(name, "PRIVATE_CONTENT")],
                "host-1",
            )
            .unwrap();
        let digest = plan.digest().to_owned();
        plan.confirm(&digest).unwrap()
    }

    #[test]
    fn prepare_is_read_only_and_public_values_hide_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let store = ImportStore::new(&home).unwrap();
        let plan = store
            .prepare(
                ImportTarget::User,
                vec![resource("sample", "SECRET_SENTINEL")],
                "host",
            )
            .unwrap();
        assert!(!home.exists());
        assert_eq!(store.snapshot().unwrap().revision(), 0);
        assert!(!format!("{plan:?}").contains("SECRET_SENTINEL"));
        assert!(matches!(
            plan.confirm("wrong"),
            Err(ImportError::ConfirmationRequired)
        ));
        assert!(!home.exists());
    }

    #[test]
    fn commit_cas_idempotency_and_pinned_readers() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImportStore::new(temp.path()).unwrap();
        let first = confirmed(&store, "one");
        let stale = confirmed(&store, "two");
        let old = store.snapshot().unwrap();
        let token = CancellationToken::new();
        let committed = store.commit(&first, &token, || Ok(())).unwrap();
        assert!(matches!(committed, ImportCommitOutcome::Committed(_)));
        assert!(old.entries().is_empty());
        assert_eq!(store.snapshot().unwrap().entries().len(), 1);
        token.cancel();
        assert_eq!(
            store
                .commit(&first, &token, || panic!(
                    "replay must not revalidate or republish"
                ))
                .unwrap(),
            committed
        );
        assert_eq!(
            store.commit(&stale, &CancellationToken::new(), || Ok(())),
            Err(ImportError::Stale)
        );
        assert_eq!(
            store
                .prepare(
                    ImportTarget::User,
                    vec![resource("one", "different")],
                    "host"
                )
                .unwrap_err(),
            ImportError::Conflict
        );
        let second = confirmed(&store, "two");
        store
            .commit(&second, &CancellationToken::new(), || Ok(()))
            .unwrap();
        assert_eq!(
            store
                .commit(&first, &token, || panic!("old receipt is retained"))
                .unwrap(),
            committed
        );
    }

    #[test]
    fn cancellation_and_faults_reconcile_without_partial_generations() {
        let temp = tempfile::tempdir().unwrap();
        let store = ImportStore::new(temp.path()).unwrap();
        let first = confirmed(&store, "first");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            store.commit(&first, &cancel, || Ok(())).unwrap(),
            ImportCommitOutcome::CancelledBeforePublication
        );
        assert!(!temp.path().join("state").exists());
        let token = CancellationToken::new();
        assert_eq!(
            store.commit_inner(&first, &token, || Ok(()), Fault::BeforePublication),
            Err(ImportError::Unavailable)
        );
        assert!(store.snapshot().unwrap().entries().is_empty());
        assert_eq!(store.reconcile(first.transaction_id()).unwrap(), None);
        assert!(matches!(
            store
                .commit_inner(&first, &token, || Ok(()), Fault::AfterPublication)
                .unwrap(),
            ImportCommitOutcome::RecoveryRequired { .. }
        ));
        let recovered = store.reconcile(first.transaction_id()).unwrap().unwrap();
        assert_eq!(store.snapshot().unwrap().entries().len(), 1);
        assert_eq!(
            store.commit(&first, &token, || Ok(())).unwrap(),
            ImportCommitOutcome::Committed(recovered)
        );
        let second = confirmed(&store, "second");
        assert!(matches!(
            store
                .commit_inner(&second, &token, || Ok(()), Fault::CancelAfterPublication)
                .unwrap(),
            ImportCommitOutcome::Committed(_)
        ));
        assert!(token.is_cancelled());
        assert_eq!(store.snapshot().unwrap().entries().len(), 2);
    }

    #[test]
    fn changed_project_identity_and_symlink_or_tampered_objects_fail() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let target = ImportTarget::project(&project).unwrap();
        std::fs::rename(&project, temp.path().join("old-project")).unwrap();
        std::fs::create_dir(&project).unwrap();
        assert_eq!(target.recheck(), Err(ImportError::Stale));
        let store = ImportStore::new(temp.path()).unwrap();
        let batch = confirmed(&store, "one");
        store
            .commit(&batch, &CancellationToken::new(), || Ok(()))
            .unwrap();
        let object = temp
            .path()
            .join("state/config-imports")
            .join(format!("{}.json", batch.transaction_id()));
        std::fs::write(&object, b"{}").unwrap();
        assert_eq!(store.snapshot().unwrap_err(), ImportError::InvalidDocument);
        #[cfg(unix)]
        {
            std::fs::remove_file(&object).unwrap();
            std::os::unix::fs::symlink("current.json", &object).unwrap();
            assert_eq!(store.snapshot().unwrap_err(), ImportError::UnsafePath);
        }
    }

    #[test]
    fn source_reads_reject_links_and_recheck_identity_and_bytes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("config.toml"), "model = 'example'").unwrap();
        let source = super::super::ImportSource::open(temp.path()).unwrap();
        let file = source.read(Path::new("config.toml")).unwrap().unwrap();
        file.recheck().unwrap();
        assert!(!format!("{file:?}").contains("example"));
        std::fs::write(temp.path().join("config.toml"), "model = 'changed'").unwrap();
        assert_eq!(file.recheck(), Err(ImportError::Stale));
        assert!(matches!(
            source.read(Path::new("../outside")),
            Err(ImportError::UnsafePath)
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("config.toml", temp.path().join("link.toml")).unwrap();
            assert!(matches!(
                source.read(Path::new("link.toml")),
                Err(ImportError::UnsafePath)
            ));
            std::fs::hard_link(
                temp.path().join("config.toml"),
                temp.path().join("hard.toml"),
            )
            .unwrap();
            assert!(matches!(
                source.read(Path::new("hard.toml")),
                Err(ImportError::UnsafePath)
            ));
        }
    }
}
