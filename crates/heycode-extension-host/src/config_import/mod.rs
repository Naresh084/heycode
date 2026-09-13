//! Human-reviewed competitor imports and one pinned native resource generation.
//!
//! Parsers never execute or resolve source configuration. Plans own their bytes,
//! and a successful store commit still requires a fresh product composition.

mod mount;
mod parser;
mod service;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_config::imports::{
    ImportError, ImportProduct, ImportResource, ImportResourceKind, ImportSource, ImportSourceFile,
    ImportTarget,
};
use serde::{Deserialize, Serialize};

pub use mount::{PinnedImportMount, SERVICE_IMPORT_MOUNT, imported_resources_plugin};
pub use service::{
    ConfigImportService, ConfirmedConfigImport, PreparedConfigImport, SERVICE_CONFIG_IMPORT,
};

/// Explicit selected source. User roots are foreign home directories;
/// project roots are the same host-authorized workspace represented by `target`.
#[derive(Clone)]
pub struct ConfigImportRequest {
    /// Verified adapter family.
    pub product: ImportProduct,
    /// Explicit foreign home directory or project root. Only fixed product paths are read.
    pub source_root: PathBuf,
    /// Host-assigned destination scope; never parsed from source files.
    pub target: ImportTarget,
}

/// A native destination key, without payload or source-path material.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportResourceKey {
    /// Native resource family.
    pub kind: ImportResourceKind,
    /// Screened destination name.
    pub name: String,
}

/// Host baseline. The opaque revision is never rendered; conflicts are exact keys.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportHostSnapshot {
    revision: String,
    conflicts: BTreeSet<ImportResourceKey>,
}

impl std::fmt::Debug for ImportHostSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportHostSnapshot")
            .field("conflicts", &self.conflicts)
            .finish_non_exhaustive()
    }
}

impl ImportHostSnapshot {
    /// Hash private host observations into a bounded opaque revision without
    /// retaining their values in the public snapshot.
    pub fn from_private_observations(
        observations: &[String],
        conflicts: BTreeSet<ImportResourceKey>,
    ) -> Result<Self, ImportError> {
        use sha2::{Digest as _, Sha256};
        if observations.iter().map(String::len).sum::<usize>()
            > heycode_config::imports::MAX_TOTAL_BYTES
        {
            return Err(ImportError::Limit);
        }
        let mut hash = Sha256::new();
        for value in observations {
            hash.update(value.len().to_le_bytes());
            hash.update(value.as_bytes());
        }
        Self::new(format!("{:x}", hash.finalize()), conflicts)
    }
    /// Bind an opaque settings/trust/workspace/native-file revision and conflicts.
    pub fn new(
        revision: String,
        conflicts: BTreeSet<ImportResourceKey>,
    ) -> Result<Self, ImportError> {
        if revision.len() > 4096 {
            return Err(ImportError::Limit);
        }
        Ok(Self {
            revision,
            conflicts,
        })
    }
}

/// Product-owned authority and native destination observation. Implementations
/// must not infer grants, perform network calls, or read credential stores.
pub trait ConfigImportHost: Send + Sync {
    /// Admit an explicit source before any bytes are read. A product host must
    /// prevent project content from being promoted through user-scope discovery.
    fn admit_source(&self, request: &ConfigImportRequest) -> Result<(), ImportError> {
        self.snapshot(&request.target, &[]).map(|_| ())
    }

    /// Validate scope and capture relevant native keys, policy and file revisions.
    /// Called before project discovery and again under the store commit lock.
    fn snapshot(
        &self,
        target: &ImportTarget,
        keys: &[ImportResourceKey],
    ) -> Result<ImportHostSnapshot, ImportError>;
}

/// Preview disposition. Unsupported data is never silently converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportItemStatus {
    /// Exact supported resource, ready for explicit selection.
    Ready,
    /// A native or imported destination owns different content.
    Conflict,
    /// The same native content already exists in the import store.
    Duplicate,
    /// The source key cannot be represented without an explicit destination name.
    NeedsRename,
    /// Transport, credential, provider or activation semantics need a separate binding.
    NeedsBinding,
    /// This adapter does not implement the source behavior.
    Unsupported,
    /// Secret or authority material is excluded from both preview and installation.
    Excluded,
}

/// Safe, fixed-code explanation rather than a source-derived parser excerpt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportItemReason {
    /// Exact verified subset.
    Supported,
    /// A server will remain disabled until explicitly enabled in native controls.
    McpInstalledDisabled,
    /// A source key needs explicit renaming.
    InvalidDestinationName,
    /// A destination already exists.
    ExistingDestination,
    /// Credential or environment metadata cannot be silently omitted.
    CredentialOrInterpolation,
    /// Provider/runtime/model selection needs an explicit native binding.
    ProviderBinding,
    /// Legacy/ambiguous transport is not an exact native transport.
    TransportBinding,
    /// Native mutation ownership cannot yet retain the source's project scope.
    ScopeBinding,
    /// Settings or policy fields lack an exact adapter.
    UnsupportedFields,
    /// Rule or skill activation/consent behavior is unsupported.
    ActivationSemantics,
    /// Shell/file expansion or extra bundled assets are unsupported.
    ExternalDependency,
    /// The source document is malformed.
    InvalidDocument,
    /// Foreign trust/permission/executable authority is never imported.
    AuthorityExcluded,
}

/// Metadata-only item. No source document values or prompt text can be serialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigImportItem {
    /// Stable row id inside this inventory.
    pub id: String,
    /// Resource family when the source has one.
    pub kind: Option<ImportResourceKind>,
    /// Screened descriptive source label, or a fixed redacted placeholder.
    pub label: String,
    /// Existing supported destination name, when representable.
    pub suggested_name: Option<String>,
    /// Whether this row can be explicitly selected.
    pub status: ImportItemStatus,
    /// Fixed explanation code.
    pub reason: ImportItemReason,
}

/// Read-only source scan. Captured documents and listings remain private.
pub struct ConfigImportInventory {
    id: String,
    target: ImportTarget,
    rows: Vec<ConfigImportItem>,
    candidates: Vec<Option<Candidate>>,
    inputs: Arc<InventoryInputs>,
}

impl std::fmt::Debug for ConfigImportInventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigImportInventory")
            .field("id", &self.id)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl ConfigImportInventory {
    /// Unique inventory identity.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Safe complete inventory, including unsupported and excluded rows.
    pub fn items(&self) -> &[ConfigImportItem] {
        &self.rows
    }
    /// Host-bound original scope.
    pub fn target(&self) -> &ImportTarget {
        &self.target
    }
}

/// Explicit human item selection. Unlisted rows are not selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigImportDecision {
    /// Add at the existing supported key.
    Add {
        /// Source inventory row.
        item_id: String,
    },
    /// Add under an explicitly selected native name.
    Rename {
        /// Source inventory row.
        item_id: String,
        /// Explicit new native destination name.
        name: String,
    },
    /// Retain the native destination and omit this source row.
    KeepExisting {
        /// Source inventory row omitted from this transaction.
        item_id: String,
    },
}

/// One selected native action in an exact confirmation preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigImportAction {
    /// Source inventory row.
    pub item_id: String,
    /// Exact native resource family.
    pub kind: ImportResourceKind,
    /// Screened destination name.
    pub name: String,
}

/// Exact digest-bound confirmation view. Installation and activation stay distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigImportPreview {
    /// Inventory being reviewed.
    pub inventory_id: String,
    /// Digest required by the separate confirmation action.
    pub digest: String,
    /// Original user/project scope label.
    pub scope: String,
    /// Store revision checked when preparing.
    pub baseline_revision: u64,
    /// Exact selected actions.
    pub actions: Vec<ConfigImportAction>,
    /// Selected resources that are already semantically identical.
    pub duplicates: usize,
    /// A fresh composition is required after a durable commit.
    pub requires_recomposition: bool,
}

#[derive(Clone)]
struct Candidate {
    product: ImportProduct,
    kind: ImportResourceKind,
    name: String,
    payload: String,
}

impl Candidate {
    fn resource(&self, rename: Option<&str>) -> Result<ImportResource, ImportError> {
        ImportResource::new(
            self.kind,
            rename.unwrap_or(&self.name).to_owned(),
            self.payload.clone(),
            self.product,
        )
    }
}

#[derive(Default)]
struct InventoryInputs {
    files: Vec<(ImportSource, PathBuf, Option<ImportSourceFile>)>,
    directories: Vec<(ImportSource, PathBuf, Vec<PathBuf>)>,
}

impl InventoryInputs {
    fn recheck(&self) -> Result<(), ImportError> {
        for (source, path, file) in &self.files {
            match file {
                Some(file) => file.recheck()?,
                None if source.read(path)?.is_some() => return Err(ImportError::Stale),
                None => {}
            }
        }
        for (source, path, names) in &self.directories {
            if &source.entries(path)? != names {
                return Err(ImportError::Stale);
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedSkillDocument {
    description: String,
    disable_model_invocation: bool,
    body: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedCommandDocument {
    description: String,
    prompt: String,
    argument_mode: ImportedArgumentMode,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImportedArgumentMode {
    Gemini,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportedInstructionDocument {
    text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
enum ImportedMcpDocument {
    Stdio { command: String, args: Vec<String> },
    StreamableHttp { url: String },
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn serialize_private(value: &impl Serialize) -> Result<String, ImportError> {
    serde_json::to_string(value).map_err(|_| ImportError::InvalidDocument)
}

fn parse_private<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, ImportError> {
    serde_json::from_str(value).map_err(|_| ImportError::InvalidDocument)
}

fn safe_label(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.len() <= 160
        && !value.chars().any(char::is_control)
        && !parser::suspected_secret(&value)
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "-_.:/".contains(ch))
    {
        value.into_owned()
    } else {
        "<source label withheld>".to_owned()
    }
}
