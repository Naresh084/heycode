//! One immutable import generation, published through one durable reference.
//!
//! The product host owns source interpretation and human authority. This owner
//! retains exact prepared bytes, checks its generation with an OS lock, and
//! never modifies native configuration files or activates runtime resources.

mod fs;
mod store;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use fs::{ImportSource, ImportSourceFile};
pub use store::{ConfirmedImportBatch, ImportStore, PreparedImportBatch};

/// Maximum bytes in one imported source/resource.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Maximum resources in one durable generation.
pub const MAX_RESOURCES: usize = 256;
/// Maximum captured source/resource bytes in one operation.
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const SCHEMA_VERSION: u32 = 1;

/// Redacted boundary failures. Source parser excerpts never appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// A path, link, file type or identifier is unsafe.
    #[error("import path or resource identity is unsafe")]
    UnsafePath,
    /// Input exceeds the fixed file, tree, depth or generation budget.
    #[error("import exceeds its bounded resource budget")]
    Limit,
    /// A strict document or digest could not be validated.
    #[error("import document is malformed or unsupported")]
    InvalidDocument,
    /// The reviewed source, destination, workspace or policy changed.
    #[error("import preview is stale; prepare and review it again")]
    Stale,
    /// A different resource already owns a destination key.
    #[error("import destination conflicts with an existing resource")]
    Conflict,
    /// Another writer owns the store.
    #[error("another import is committing; retry after it settles")]
    Busy,
    /// A caller did not confirm this exact prepared digest.
    #[error("explicit confirmation of the current import digest is required")]
    ConfirmationRequired,
    /// A filesystem/service failure prevented a pre-publication operation.
    #[error("import storage or host state is unavailable")]
    Unavailable,
    /// The service/host authority has been withdrawn.
    #[error("import service is closed or project authority is unavailable")]
    Authority,
}

/// Verified source-product adapter family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportProduct {
    /// Codex configuration and declarations.
    Codex,
    /// Gemini CLI configuration and declarations.
    Gemini,
    /// Cursor configuration and declarations.
    Cursor,
}

/// Native resource family; there is no credential, trust, hook or route variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportResourceKind {
    /// A strict native custom-agent declaration.
    Agent,
    /// A frozen native instruction-only skill.
    Skill,
    /// A verified prompt-command adapter.
    Command,
    /// Scoped static instruction text.
    Instructions,
    /// A disabled, credential-free MCP definition.
    Mcp,
}

/// Host-bound scope. Project identity cannot be supplied by source bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportTarget {
    /// Explicit user-owned resources.
    User,
    /// Resources restricted to the same canonical workspace identity.
    Project {
        /// Canonical root, used only by trusted host readers.
        root: PathBuf,
        /// Device identity captured by the host.
        device: u64,
        /// Directory inode captured by the host.
        inode: u64,
    },
}

impl std::fmt::Debug for ImportTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if matches!(self, Self::User) {
            "User"
        } else {
            "Project(<private identity>)"
        })
    }
}

impl ImportTarget {
    /// Bind a project identity from an existing host-selected workspace.
    pub fn project(root: &Path) -> Result<Self, ImportError> {
        let anchor = fs::Anchor::bind(root, false)?;
        let (device, inode) = anchor.root_identity();
        Ok(Self::Project {
            root: anchor.path(),
            device,
            inode,
        })
    }
    /// Canonical project path for the trusted composition owner.
    pub fn project_root(&self) -> Option<&Path> {
        match self {
            Self::User => None,
            Self::Project { root, .. } => Some(root),
        }
    }
    /// Revalidate the original project directory identity.
    pub fn recheck(&self) -> Result<(), ImportError> {
        if let Self::Project {
            root,
            device,
            inode,
        } = self
        {
            let current = Self::project(root)?;
            if current != *self || (*device, *inode) == (0, 0) {
                return Err(ImportError::Stale);
            }
        }
        Ok(())
    }
    pub(super) fn validate(&self) -> Result<(), ImportError> {
        if let Self::Project { root, .. } = self
            && (!root.is_absolute()
                || root.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                }))
        {
            return Err(ImportError::UnsafePath);
        }
        Ok(())
    }
}

/// One validated adapter payload. Serialization is intentionally private.
#[derive(Clone)]
pub struct ImportResource {
    kind: ImportResourceKind,
    name: String,
    payload: String,
    product: ImportProduct,
}

impl std::fmt::Debug for ImportResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportResource")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl ImportResource {
    /// Accept an exact adapter payload. Native activation must validate its schema again.
    pub fn new(
        kind: ImportResourceKind,
        name: String,
        payload: String,
        product: ImportProduct,
    ) -> Result<Self, ImportError> {
        if !valid_name(&name) {
            return Err(ImportError::UnsafePath);
        }
        if payload.is_empty() || payload.len() > MAX_FILE_BYTES || payload.contains('\0') {
            return Err(ImportError::Limit);
        }
        Ok(Self {
            kind,
            name,
            payload,
            product,
        })
    }
    /// Resource kind.
    pub const fn kind(&self) -> ImportResourceKind {
        self.kind
    }
    /// Screened destination key.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Exact bytes for a trusted native activation adapter, never a preview.
    pub fn payload_for_activation(&self) -> &str {
        &self.payload
    }
    /// Source-product family.
    pub const fn product(&self) -> ImportProduct {
        self.product
    }
    /// Explicitly rename a keyed native destination without altering payload bytes.
    pub fn renamed(&self, name: String) -> Result<Self, ImportError> {
        Self::new(self.kind, name, self.payload.clone(), self.product)
    }
    /// Compare native kind, key and bytes without exposing the payload.
    pub fn same_content(&self, other: &Self) -> bool {
        self.kind == other.kind && self.name == other.name && self.payload == other.payload
    }
}

/// One scope-preserving resource mounted from an immutable generation.
#[derive(Debug, Clone)]
pub struct ImportedEntry {
    target: ImportTarget,
    resource: ImportResource,
}

impl ImportedEntry {
    /// Original host-bound authority scope.
    pub fn target(&self) -> &ImportTarget {
        &self.target
    }
    /// Private native resource.
    pub fn resource(&self) -> &ImportResource {
        &self.resource
    }
}

/// Safe durable import receipt. Installation does not imply runtime activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportReceipt {
    /// Idempotency identity.
    pub transaction_id: String,
    /// Exact digest the human confirmed.
    pub reviewed_digest: String,
    /// Published store revision.
    pub revision: u64,
    /// Number of newly added resources.
    pub added: usize,
}

/// No uncertainty is represented as cancellation or a blindly retryable failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportCommitOutcome {
    /// Durable publication succeeded; a fresh composition is still required.
    Committed(ImportReceipt),
    /// Every selected resource already exists with identical native content; no write occurred.
    Unchanged {
        /// The existing published revision.
        revision: u64,
    },
    /// Cancellation won before the manifest pointer changed.
    CancelledBeforePublication,
    /// A possibly published transaction must be reconciled before retrying.
    RecoveryRequired {
        /// The operation to reconcile before any retry.
        transaction_id: String,
    },
}

/// A complete pinned read-side generation, never an independently refreshing handle.
#[derive(Debug, Clone, Default)]
pub struct ImportGeneration {
    id: Option<String>,
    revision: u64,
    entries: Vec<ImportedEntry>,
    receipts: Vec<ImportReceipt>,
}

impl ImportGeneration {
    /// Immutable object id, absent for an empty store.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// CAS revision.
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    /// All entries. The host must select user and currently authorized project scope.
    pub fn entries(&self) -> &[ImportedEntry] {
        &self.entries
    }
    /// Find a prior committed operation without replaying it.
    pub fn receipt(&self, transaction_id: &str) -> Option<&ImportReceipt> {
        self.receipts
            .iter()
            .find(|receipt| receipt.transaction_id == transaction_id)
    }
}

pub(super) fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
