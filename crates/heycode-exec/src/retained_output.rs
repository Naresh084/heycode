//! Owner-scoped retained-output spill storage.
//!
//! Large process/tool results need two bounded planes: complete bytes in
//! owner-only storage, and one model/UI preview whose **entire** rendered
//! envelope fits the caller's cap. This service owns both so a Consumer cannot
//! cap the body and then accidentally exceed the budget with an id, byte count
//! or wrapper.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::SERVICE_RETAINED_OUTPUT;

/// Largest range returned by one retained-output read.
pub const MAX_RETAINED_OUTPUT_READ_BYTES: usize = 64 * 1024;
/// Largest complete rendered preview, including wrapper and metadata.
pub const MAX_RETAINED_OUTPUT_VIEW_BYTES: usize = 64 * 1024;
/// Default maximum size of one retained object.
pub const DEFAULT_RETAINED_OUTPUT_MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
/// Default maximum bytes retained by one service generation.
pub const DEFAULT_RETAINED_OUTPUT_MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

const MAX_OWNER_BYTES: usize = 128;
const MAX_RETAINED_OUTPUT_OBJECTS: usize = 1024;
#[cfg(unix)]
const IO_CHUNK_BYTES: usize = 64 * 1024;
const VIEW_FOOTER: &str = "\n[/retained-output]\n";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stable retained-output failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetainedOutputErrorCode {
    /// Invalid root, owner, id, bound, range or body.
    InvalidSpec,
    /// Host lacks the audited owner-only file boundary.
    UnsupportedSecurity,
    /// Generation entry/byte capacity is exhausted.
    Capacity,
    /// Output is absent for this owner; foreign and unknown are identical.
    UnknownOutput,
    /// Stored identity, mode, link count or content digest changed.
    Corrupt,
    /// Caller cancelled before the commit point.
    Cancelled,
    /// Owning service generation has closed.
    ServiceStopped,
    /// Owner-only storage I/O failed.
    Io,
}

impl RetainedOutputErrorCode {
    /// Stable machine name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid_spec",
            Self::UnsupportedSecurity => "unsupported_security",
            Self::Capacity => "capacity",
            Self::UnknownOutput => "unknown_output",
            Self::Corrupt => "corrupt",
            Self::Cancelled => "cancelled",
            Self::ServiceStopped => "service_stopped",
            Self::Io => "io",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidSpec => "invalid retained-output specification",
            Self::UnsupportedSecurity => {
                "retained-output owner-only security is unsupported on this host"
            }
            Self::Capacity => "retained-output storage is at its configured capacity",
            Self::UnknownOutput => "retained output is not registered for this owner",
            Self::Corrupt => "retained-output identity verification failed",
            Self::Cancelled => "retained-output operation was cancelled",
            Self::ServiceStopped => "retained-output service has stopped",
            Self::Io => "retained-output storage operation failed",
        }
    }
}

/// Fixed body/path-free retained-output failure.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RetainedOutputError {
    code: RetainedOutputErrorCode,
    message: &'static str,
}

impl RetainedOutputError {
    const fn new(code: RetainedOutputErrorCode) -> Self {
        Self {
            code,
            message: code.message(),
        }
    }

    /// Stable failure class.
    #[must_use]
    pub const fn code(&self) -> RetainedOutputErrorCode {
        self.code
    }
}

/// Validated logical authority owning retained output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetainedOutputOwner(String);

impl RetainedOutputOwner {
    /// Validate an owner scope.
    ///
    /// # Errors
    /// Empty, oversized or control/path-bearing scopes are refused.
    pub fn new(value: impl Into<String>) -> Result<Self, RetainedOutputError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_OWNER_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ))
        }
    }

    /// Stable owner scope.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// SHA-256 content identity of retained output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetainedOutputId(String);

impl RetainedOutputId {
    fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("sha256-{}", hex(&digest)))
    }

    /// Validate an identity returned by [`Self::as_str`].
    ///
    /// # Errors
    /// Anything except `sha256-` plus 64 lowercase hexadecimal digits fails.
    pub fn parse(value: &str) -> Result<Self, RetainedOutputError> {
        let valid = value.strip_prefix("sha256-").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ))
        }
    }

    /// Stable content address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Owner-only local spill-store configuration.
#[derive(Clone)]
pub struct RetainedOutputConfig {
    root: PathBuf,
    max_object_bytes: usize,
    max_total_bytes: usize,
}

impl RetainedOutputConfig {
    /// Construct an absolute root with conservative object/generation caps.
    ///
    /// # Errors
    /// Relative, empty or NUL-bearing roots are refused.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, RetainedOutputError> {
        let root = root.into();
        if !root.is_absolute() || root.as_os_str().is_empty() || path_contains_nul(&root) {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        Ok(Self {
            root,
            max_object_bytes: DEFAULT_RETAINED_OUTPUT_MAX_OBJECT_BYTES,
            max_total_bytes: DEFAULT_RETAINED_OUTPUT_MAX_TOTAL_BYTES,
        })
    }

    /// Replace the per-object content-byte cap.
    ///
    /// # Errors
    /// Zero or values above the generation cap are refused.
    pub fn with_max_object_bytes(mut self, bytes: usize) -> Result<Self, RetainedOutputError> {
        if bytes == 0 || bytes > self.max_total_bytes {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        self.max_object_bytes = bytes;
        Ok(self)
    }

    /// Replace the complete content bytes retained by this generation.
    ///
    /// # Errors
    /// Zero or a cap below the current per-object cap is refused.
    pub fn with_max_total_bytes(mut self, bytes: usize) -> Result<Self, RetainedOutputError> {
        if bytes == 0 || bytes < self.max_object_bytes {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        self.max_total_bytes = bytes;
        Ok(self)
    }
}

impl std::fmt::Debug for RetainedOutputConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedOutputConfig")
            .field("root", &"[REDACTED]")
            .field("max_object_bytes", &self.max_object_bytes)
            .field("max_total_bytes", &self.max_total_bytes)
            .finish()
    }
}

/// Commit receipt plus complete bounded model/UI preview envelope.
pub struct RetainedOutputReceipt {
    id: RetainedOutputId,
    total_bytes: u64,
    preview_source_bytes: u64,
    rendered_preview: String,
}

impl RetainedOutputReceipt {
    /// Atomic content identity.
    #[must_use]
    pub const fn id(&self) -> &RetainedOutputId {
        &self.id
    }

    /// Complete stored byte count.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Source bytes represented inside the escaped preview body.
    #[must_use]
    pub const fn preview_source_bytes(&self) -> u64 {
        self.preview_source_bytes
    }

    /// True when the complete stored output is larger than the preview body.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.preview_source_bytes < self.total_bytes
    }

    /// Pre-rendered envelope whose wrapper, metadata and body together fit the
    /// caller's complete-output cap.
    #[must_use]
    pub fn rendered_preview(&self) -> &str {
        &self.rendered_preview
    }
}

impl std::fmt::Debug for RetainedOutputReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedOutputReceipt")
            .field("id", &self.id)
            .field("total_bytes", &self.total_bytes)
            .field("preview_source_bytes", &self.preview_source_bytes)
            .field("rendered_bytes", &self.rendered_preview.len())
            .finish()
    }
}

/// One verified bounded range read.
pub struct RetainedOutputRead {
    bytes: Vec<u8>,
    offset: u64,
    total_bytes: u64,
}

impl RetainedOutputRead {
    /// Byte-exact range.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Starting offset of this range.
    #[must_use]
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Complete object length.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Whether this range reaches exact EOF.
    #[must_use]
    pub fn eof(&self) -> bool {
        self.offset
            .checked_add(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX))
            .is_some_and(|end| end >= self.total_bytes)
    }
}

impl std::fmt::Debug for RetainedOutputRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedOutputRead")
            .field("bytes", &self.bytes.len())
            .field("offset", &self.offset)
            .field("total_bytes", &self.total_bytes)
            .field("eof", &self.eof())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EntryKey {
    owner: RetainedOutputOwner,
    id: RetainedOutputId,
}

struct StoreState {
    open: bool,
    total_bytes: usize,
    entries: BTreeMap<EntryKey, usize>,
}

struct RetainedOutputInner {
    config: RetainedOutputConfig,
    generation_root: PathBuf,
    state: Mutex<StoreState>,
    lifecycle: CancellationToken,
}

impl Drop for RetainedOutputInner {
    fn drop(&mut self) {
        self.lifecycle.cancel();
        let _ = remove_generation(&self.generation_root);
    }
}

/// Cloneable effect-owned retained-output service.
#[derive(Clone)]
pub struct RetainedOutputService {
    inner: Arc<RetainedOutputInner>,
}

impl RetainedOutputService {
    /// Open one owner-only service generation below `config.root`.
    ///
    /// # Errors
    /// Invalid/insecure roots, unsupported host security and I/O fail safely.
    pub fn open(config: RetainedOutputConfig) -> Result<Self, RetainedOutputError> {
        let generation_root = open_generation(&config.root)?;
        Ok(Self {
            inner: Arc::new(RetainedOutputInner {
                config,
                generation_root,
                state: Mutex::new(StoreState {
                    open: true,
                    total_bytes: 0,
                    entries: BTreeMap::new(),
                }),
                lifecycle: CancellationToken::new(),
            }),
        })
    }

    /// Atomically retain complete bytes and build a bounded escaped preview.
    ///
    /// `complete_output_cap_bytes` covers the entire rendered envelope:
    /// wrapper, content id, byte-count metadata, escaped preview and footer.
    ///
    /// # Errors
    /// Invalid/oversized input, cancellation, capacity, corruption or storage
    /// failures commit no receipt.
    pub fn retain(
        &self,
        owner: &RetainedOutputOwner,
        bytes: &[u8],
        complete_output_cap_bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<RetainedOutputReceipt, RetainedOutputError> {
        if bytes.is_empty() || bytes.len() > self.inner.config.max_object_bytes {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        let mut state = lock(&self.inner.state);
        ensure_open(&state)?;
        check_cancellation(&cancellation, &self.inner.lifecycle)?;
        let id = RetainedOutputId::from_bytes(bytes);
        let receipt = build_receipt(id.clone(), bytes, complete_output_cap_bytes)?;
        let key = EntryKey {
            owner: owner.clone(),
            id: id.clone(),
        };
        let path = object_path(&self.inner.generation_root, owner, &id);
        if let Some(expected) = state.entries.get(&key).copied() {
            verify_object(
                &path,
                &id,
                expected,
                None,
                &cancellation,
                &self.inner.lifecycle,
            )?;
            return Ok(receipt);
        }
        if state.entries.len() >= MAX_RETAINED_OUTPUT_OBJECTS
            || state
                .total_bytes
                .checked_add(bytes.len())
                .is_none_or(|total| total > self.inner.config.max_total_bytes)
        {
            return Err(RetainedOutputError::new(RetainedOutputErrorCode::Capacity));
        }
        publish_object(
            &self.inner.generation_root,
            owner,
            &id,
            bytes,
            &cancellation,
            &self.inner.lifecycle,
        )?;
        state.total_bytes = state.total_bytes.saturating_add(bytes.len());
        state.entries.insert(key, bytes.len());
        Ok(receipt)
    }

    /// Read one verified range, clamped to
    /// [`MAX_RETAINED_OUTPUT_READ_BYTES`].
    ///
    /// # Errors
    /// Zero bounds, out-of-range offsets, unknown/foreign ids, cancellation,
    /// corruption and stopped service state fail safely.
    pub fn read(
        &self,
        owner: &RetainedOutputOwner,
        id: &RetainedOutputId,
        offset: u64,
        max_bytes: usize,
        cancellation: CancellationToken,
    ) -> Result<RetainedOutputRead, RetainedOutputError> {
        if max_bytes == 0 {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        let state = lock(&self.inner.state);
        ensure_open(&state)?;
        check_cancellation(&cancellation, &self.inner.lifecycle)?;
        let expected = state
            .entries
            .get(&EntryKey {
                owner: owner.clone(),
                id: id.clone(),
            })
            .copied()
            .ok_or_else(|| RetainedOutputError::new(RetainedOutputErrorCode::UnknownOutput))?;
        if offset > u64::try_from(expected).unwrap_or(u64::MAX) {
            return Err(RetainedOutputError::new(
                RetainedOutputErrorCode::InvalidSpec,
            ));
        }
        let limit = max_bytes.min(MAX_RETAINED_OUTPUT_READ_BYTES);
        let path = object_path(&self.inner.generation_root, owner, id);
        let bytes = verify_object(
            &path,
            id,
            expected,
            Some((offset, limit)),
            &cancellation,
            &self.inner.lifecycle,
        )?
        .unwrap_or_default();
        Ok(RetainedOutputRead {
            bytes,
            offset,
            total_bytes: u64::try_from(expected).unwrap_or(u64::MAX),
        })
    }

    /// Stop the generation and remove every object it owns.
    ///
    /// Idempotent: a second close succeeds without touching another
    /// generation sharing the configured parent root.
    ///
    /// # Errors
    /// Cleanup I/O failure is reported after the service becomes stopped.
    pub fn close(&self) -> Result<(), RetainedOutputError> {
        let mut state = lock(&self.inner.state);
        if !state.open {
            return Ok(());
        }
        state.open = false;
        state.entries.clear();
        state.total_bytes = 0;
        self.inner.lifecycle.cancel();
        remove_generation(&self.inner.generation_root)
    }
}

impl std::fmt::Debug for RetainedOutputService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.inner.state);
        formatter
            .debug_struct("RetainedOutputService")
            .field("open", &state.open)
            .field("entries", &state.entries.len())
            .field("retained_bytes", &state.total_bytes)
            .field("max_object_bytes", &self.inner.config.max_object_bytes)
            .field("max_total_bytes", &self.inner.config.max_total_bytes)
            .finish()
    }
}

/// Publish an owner-only retained-output service at
/// [`SERVICE_RETAINED_OUTPUT`].
#[must_use]
pub fn retained_output_plugin(config: RetainedOutputConfig) -> Box<dyn Plugin> {
    struct RetainedOutputPlugin(RetainedOutputConfig);

    impl Plugin for RetainedOutputPlugin {
        fn name(&self) -> &'static str {
            "retained-output-local"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_RETAINED_OUTPUT]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let service = RetainedOutputService::open(self.0.clone())
                .map_err(|error| CoreError::other(error.to_string()))?;
            let disposer = service.clone();
            context.effect(move || {
                let _ = disposer.close();
            });
            context.provide(SERVICE_RETAINED_OUTPUT, self.name(), service)
        }
    }

    Box::new(RetainedOutputPlugin(config))
}

fn ensure_open(state: &StoreState) -> Result<(), RetainedOutputError> {
    if state.open {
        Ok(())
    } else {
        Err(RetainedOutputError::new(
            RetainedOutputErrorCode::ServiceStopped,
        ))
    }
}

fn check_cancellation(
    caller: &CancellationToken,
    lifecycle: &CancellationToken,
) -> Result<(), RetainedOutputError> {
    if caller.is_cancelled() || lifecycle.is_cancelled() {
        Err(RetainedOutputError::new(RetainedOutputErrorCode::Cancelled))
    } else {
        Ok(())
    }
}

fn build_receipt(
    id: RetainedOutputId,
    bytes: &[u8],
    complete_cap: usize,
) -> Result<RetainedOutputReceipt, RetainedOutputError> {
    if complete_cap == 0 || complete_cap > MAX_RETAINED_OUTPUT_VIEW_BYTES {
        return Err(RetainedOutputError::new(
            RetainedOutputErrorCode::InvalidSpec,
        ));
    }
    let header = format!(
        "[retained-output id={} total={} bytes]\n",
        id.as_str(),
        bytes.len()
    );
    let overhead = header.len().saturating_add(VIEW_FOOTER.len());
    if overhead > complete_cap {
        return Err(RetainedOutputError::new(
            RetainedOutputErrorCode::InvalidSpec,
        ));
    }
    let mut rendered = String::with_capacity(complete_cap);
    rendered.push_str(&header);
    let body_cap = complete_cap - overhead;
    let mut represented = 0_usize;
    for byte in bytes {
        let escaped = escape_byte(*byte);
        if rendered
            .len()
            .saturating_sub(header.len())
            .checked_add(escaped.len())
            .is_none_or(|length| length > body_cap)
        {
            break;
        }
        rendered.push_str(&escaped);
        represented = represented.saturating_add(1);
    }
    rendered.push_str(VIEW_FOOTER);
    Ok(RetainedOutputReceipt {
        id,
        total_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        preview_source_bytes: u64::try_from(represented).unwrap_or(u64::MAX),
        rendered_preview: rendered,
    })
}

fn escape_byte(byte: u8) -> String {
    match byte {
        b'\n' => "\n".to_owned(),
        b'\t' => "\t".to_owned(),
        b' '..=b'~' if byte != b'\\' => char::from(byte).to_string(),
        b'\\' => "\\\\".to_owned(),
        _ => format!("\\x{byte:02x}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn owner_directory(owner: &RetainedOutputOwner) -> String {
    let digest = Sha256::digest(owner.as_str().as_bytes());
    format!("owner-{}", hex(&digest))
}

fn object_path(
    generation_root: &Path,
    owner: &RetainedOutputOwner,
    id: &RetainedOutputId,
) -> PathBuf {
    generation_root
        .join(owner_directory(owner))
        .join(id.as_str())
}

#[cfg(unix)]
fn open_generation(root: &Path) -> Result<PathBuf, RetainedOutputError> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

    if !root.is_absolute() {
        return Err(RetainedOutputError::new(
            RetainedOutputErrorCode::InvalidSpec,
        ));
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) => validate_directory(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = root
                .parent()
                .ok_or_else(|| RetainedOutputError::new(RetainedOutputErrorCode::InvalidSpec))?;
            let parent_metadata = std::fs::symlink_metadata(parent)
                .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
            if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
                return Err(RetainedOutputError::new(
                    RetainedOutputErrorCode::InvalidSpec,
                ));
            }
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(root)
                .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
            validate_directory(
                &std::fs::symlink_metadata(root)
                    .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?,
            )?;
            sync_directory(parent)?;
        }
        Err(_) => return Err(RetainedOutputError::new(RetainedOutputErrorCode::Io)),
    }
    let canonical = std::fs::canonicalize(root)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    validate_directory(
        &std::fs::symlink_metadata(&canonical)
            .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?,
    )?;
    let generation = canonical.join(format!("generation-{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(&generation)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    validate_directory(
        &std::fs::symlink_metadata(&generation)
            .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?,
    )?;
    sync_directory(&canonical)?;
    Ok(generation)
}

#[cfg(not(unix))]
fn open_generation(_root: &Path) -> Result<PathBuf, RetainedOutputError> {
    Err(RetainedOutputError::new(
        RetainedOutputErrorCode::UnsupportedSecurity,
    ))
}

#[cfg(unix)]
fn publish_object(
    generation_root: &Path,
    owner: &RetainedOutputOwner,
    id: &RetainedOutputId,
    bytes: &[u8],
    caller: &CancellationToken,
    lifecycle: &CancellationToken,
) -> Result<(), RetainedOutputError> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    let owner_dir = generation_root.join(owner_directory(owner));
    match std::fs::symlink_metadata(&owner_dir) {
        Ok(metadata) => validate_directory(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(&owner_dir)
                .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
            validate_directory(
                &std::fs::symlink_metadata(&owner_dir)
                    .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?,
            )?;
            sync_directory(generation_root)?;
        }
        Err(_) => return Err(RetainedOutputError::new(RetainedOutputErrorCode::Io)),
    }
    let final_path = owner_dir.join(id.as_str());
    match std::fs::symlink_metadata(&final_path) {
        Ok(_) => {
            verify_object(&final_path, id, bytes.len(), None, caller, lifecycle)?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RetainedOutputError::new(RetainedOutputErrorCode::Io)),
    }
    let temporary = owner_dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    let mut guard = TemporaryFile {
        path: temporary.clone(),
        active: true,
    };
    for chunk in bytes.chunks(IO_CHUNK_BYTES) {
        check_cancellation(caller, lifecycle)?;
        file.write_all(chunk)
            .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    }
    file.sync_all()
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
    validate_file(
        &file
            .metadata()
            .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?,
        bytes.len(),
    )?;
    drop(file);
    check_cancellation(caller, lifecycle)?;
    match std::fs::hard_link(&temporary, &final_path) {
        Ok(()) => {
            std::fs::remove_file(&temporary)
                .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
            guard.active = false;
            sync_directory(&owner_dir)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_object(&final_path, id, bytes.len(), None, caller, lifecycle)?;
            return Ok(());
        }
        Err(_) => return Err(RetainedOutputError::new(RetainedOutputErrorCode::Io)),
    }
    verify_object(
        &final_path,
        id,
        bytes.len(),
        None,
        &CancellationToken::new(),
        &CancellationToken::new(),
    )?;
    Ok(())
}

#[cfg(not(unix))]
fn publish_object(
    _generation_root: &Path,
    _owner: &RetainedOutputOwner,
    _id: &RetainedOutputId,
    _bytes: &[u8],
    _caller: &CancellationToken,
    _lifecycle: &CancellationToken,
) -> Result<(), RetainedOutputError> {
    Err(RetainedOutputError::new(
        RetainedOutputErrorCode::UnsupportedSecurity,
    ))
}

#[cfg(unix)]
fn verify_object(
    path: &Path,
    id: &RetainedOutputId,
    expected: usize,
    range: Option<(u64, usize)>,
    caller: &CancellationToken,
    lifecycle: &CancellationToken,
) -> Result<Option<Vec<u8>>, RetainedOutputError> {
    use std::io::Read as _;

    check_cancellation(caller, lifecycle)?;
    let before = std::fs::symlink_metadata(path)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))?;
    validate_file(&before, expected)?;
    let identity = FileIdentity::new(&before);
    let mut file = std::fs::File::open(path)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))?;
    let opened = file
        .metadata()
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))?;
    validate_file(&opened, expected)?;
    if FileIdentity::new(&opened) != identity {
        return Err(RetainedOutputError::new(RetainedOutputErrorCode::Corrupt));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; IO_CHUNK_BYTES];
    let mut position = 0_u64;
    let mut selected = range.map(|_| Vec::new());
    loop {
        check_cancellation(caller, lifecycle)?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        if let (Some((offset, limit)), Some(output)) = (range, selected.as_mut()) {
            let chunk_end = position.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
            let wanted_end = offset.saturating_add(u64::try_from(limit).unwrap_or(u64::MAX));
            let start = offset.max(position);
            let end = wanted_end.min(chunk_end);
            if start < end {
                let local_start = usize::try_from(start - position).unwrap_or(usize::MAX);
                let local_end = usize::try_from(end - position).unwrap_or(usize::MAX);
                if local_start > local_end || local_end > count {
                    return Err(RetainedOutputError::new(RetainedOutputErrorCode::Corrupt));
                }
                output.extend_from_slice(&buffer[local_start..local_end]);
            }
        }
        position = position.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
    let after_open = file
        .metadata()
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))?;
    let after_path = std::fs::symlink_metadata(path)
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))?;
    if FileIdentity::new(&after_open) != identity
        || FileIdentity::new(&after_path) != identity
        || position != u64::try_from(expected).unwrap_or(u64::MAX)
        || RetainedOutputId(format!("sha256-{}", hex(&hasher.finalize()))) != *id
    {
        return Err(RetainedOutputError::new(RetainedOutputErrorCode::Corrupt));
    }
    Ok(selected)
}

#[cfg(not(unix))]
fn verify_object(
    _path: &Path,
    _id: &RetainedOutputId,
    _expected: usize,
    _range: Option<(u64, usize)>,
    _caller: &CancellationToken,
    _lifecycle: &CancellationToken,
) -> Result<Option<Vec<u8>>, RetainedOutputError> {
    Err(RetainedOutputError::new(
        RetainedOutputErrorCode::UnsupportedSecurity,
    ))
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    links: u64,
    mode: u32,
}

#[cfg(unix)]
impl FileIdentity {
    fn new(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            links: metadata.nlink(),
            mode: metadata.permissions().mode() & 0o777,
        }
    }
}

#[cfg(unix)]
fn validate_directory(metadata: &std::fs::Metadata) -> Result<(), RetainedOutputError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        Err(RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn validate_file(metadata: &std::fs::Metadata, expected: usize) -> Result<(), RetainedOutputError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() != u64::try_from(expected).unwrap_or(u64::MAX)
    {
        Err(RetainedOutputError::new(RetainedOutputErrorCode::Corrupt))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), RetainedOutputError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| RetainedOutputError::new(RetainedOutputErrorCode::Io))
}

#[cfg(unix)]
fn remove_generation(path: &Path) -> Result<(), RetainedOutputError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(RetainedOutputError::new(RetainedOutputErrorCode::Io)),
    }
}

#[cfg(not(unix))]
fn remove_generation(_path: &Path) -> Result<(), RetainedOutputError> {
    Ok(())
}

#[cfg(unix)]
struct TemporaryFile {
    path: PathBuf,
    active: bool,
}

#[cfg(unix)]
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().contains(&0)
}

#[cfg(windows)]
fn path_contains_nul(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str().encode_wide().any(|unit| unit == 0)
}

#[cfg(not(any(unix, windows)))]
fn path_contains_nul(path: &Path) -> bool {
    path.to_string_lossy().contains('\0')
}
