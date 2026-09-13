//! TOML load/replace implementation.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use atomic_write_file::AtomicWriteFile;
use heycode_settings::{SettingsDocuments, SettingsNamespace, SettingsService, SettingsWriter};
use notify::Watcher as _;
use serde::Deserialize;
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::{FILE_SCHEMA_VERSION, FileSettingsConfig, FileSettingsError};

#[derive(Deserialize, Default)]
struct DiskDocument {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    settings: BTreeMap<String, Value>,
}

/// Format-preserving user settings writer and user/project loader.
pub struct FileSettingsProvider {
    config: FileSettingsConfig,
    overlay: Option<Arc<dyn PinnedSettingsOverlay>>,
    writer: Mutex<()>,
    last_reload_error: Mutex<Option<String>>,
}

/// A composition-pinned lower-priority document layer. Implementations must
/// use captured values; reloads must never discover a new import generation.
pub trait PinnedSettingsOverlay: Send + Sync {
    /// Merge into detached native documents without persistence or activation.
    /// Native values and managed authority must keep their existing precedence.
    fn merge(&self, native: SettingsDocuments) -> Result<SettingsDocuments, String>;
}

impl FileSettingsProvider {
    /// Build a provider. No file is touched until load or persist.
    #[must_use]
    pub fn new(config: FileSettingsConfig) -> Self {
        Self {
            config,
            overlay: None,
            writer: Mutex::new(()),
            last_reload_error: Mutex::new(None),
        }
    }

    /// Attach one immutable overlay for this provider's complete lifetime.
    #[must_use]
    pub fn with_pinned_overlay(mut self, overlay: Arc<dyn PinnedSettingsOverlay>) -> Self {
        self.overlay = Some(overlay);
        self
    }

    /// Load detached user and optional trusted-project documents.
    ///
    /// Missing files are empty layers. Existing symlinks/non-files, malformed
    /// TOML, invalid namespaces/sections, and future schemas fail loud.
    ///
    /// # Errors
    /// [`FileSettingsError`] names the offending path and boundary.
    pub fn load_documents(&self) -> Result<SettingsDocuments, FileSettingsError> {
        let mut documents = SettingsDocuments::new();
        if let Some(raw) = read_optional_regular(&self.config.user_path)? {
            let disk = parse_disk(&self.config.user_path, &raw)?;
            for (raw_namespace, section) in disk.settings {
                let namespace = SettingsNamespace::new(raw_namespace).map_err(|error| {
                    FileSettingsError::parse(&self.config.user_path, error.to_string())
                })?;
                documents.set_user(namespace, section).map_err(|error| {
                    FileSettingsError::parse(&self.config.user_path, error.to_string())
                })?;
            }
        }
        if let Some(project_path) = self.config.project_path.as_ref()
            && let Some(raw) = read_optional_regular(project_path)?
        {
            let disk = parse_disk(project_path, &raw)?;
            for (raw_namespace, section) in disk.settings {
                let namespace = SettingsNamespace::new(raw_namespace)
                    .map_err(|error| FileSettingsError::parse(project_path, error.to_string()))?;
                documents
                    .set_project(namespace, section)
                    .map_err(|error| FileSettingsError::parse(project_path, error.to_string()))?;
            }
        }
        match &self.overlay {
            Some(overlay) => overlay
                .merge(documents)
                .map_err(|message| FileSettingsError::ReloadRejected { message }),
            None => Ok(documents),
        }
    }

    /// Re-read both documents and publish one validated generation.
    ///
    /// # Errors
    /// File boundaries or settings validation failures keep the last good
    /// service generation.
    pub fn reload_into(&self, service: &SettingsService) -> Result<(), FileSettingsError> {
        let documents = self.load_documents()?;
        service
            .publish_documents(documents)
            .map_err(|error| FileSettingsError::ReloadRejected {
                message: error.to_string(),
            })
    }

    /// Latest automatic reload failure, cleared after the next success.
    #[must_use]
    pub fn last_reload_error(&self) -> Option<String> {
        self.last_reload_error.lock().ok()?.clone()
    }

    /// Start cross-platform parent-directory watches for configured files.
    ///
    /// # Errors
    /// Directory creation/subscription/backend failures prevent plugin load.
    pub fn start_watching(
        self: &Arc<Self>,
        service: SettingsService,
    ) -> Result<FileWatchHandle, FileSettingsError> {
        if let Some(parent) = self.config.user_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| FileSettingsError::io(parent, source))?;
        }
        let targets: Vec<PathBuf> = std::iter::once(self.config.user_path.clone())
            .chain(self.config.project_path.clone())
            .map(|path| normalize_watch_target(&path))
            .collect();
        let (sender, receiver) = mpsc::channel();
        let event_sender = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = event_sender.send(WatchMessage::Event(event));
        })
        .map_err(|error| FileSettingsError::Watch {
            message: error.to_string(),
        })?;
        let mut roots = std::collections::BTreeSet::new();
        for target in &targets {
            if let Some(parent) = target.parent()
                && parent.is_dir()
            {
                roots.insert(parent.to_path_buf());
            }
        }
        for root in roots {
            watcher
                .watch(&root, notify::RecursiveMode::NonRecursive)
                .map_err(|error| FileSettingsError::Watch {
                    message: format!("{}: {error}", root.display()),
                })?;
        }

        let provider = self.clone();
        let worker = std::thread::Builder::new()
            .name("heycode-settings-watch".to_owned())
            .spawn(move || watch_loop(receiver, provider, service, targets))
            .map_err(|source| FileSettingsError::io(Path::new("<watch-thread>"), source))?;
        Ok(FileWatchHandle {
            watcher: Some(watcher),
            sender,
            worker: Some(worker),
        })
    }

    fn persist(
        &self,
        namespace: &SettingsNamespace,
        section: &Value,
    ) -> Result<(), FileSettingsError> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| FileSettingsError::WriterUnavailable)?;
        let path = &self.config.user_path;
        let raw = read_optional_regular(path)?.unwrap_or_default();
        let mut document = if raw.trim().is_empty() {
            DocumentMut::new()
        } else {
            raw.parse::<DocumentMut>()
                .map_err(|error| FileSettingsError::parse(path, error.message()))?
        };
        validate_version(
            path,
            document.get("schema_version").and_then(Item::as_integer),
        )?;
        if document.get("schema_version").is_none() {
            document.insert("schema_version", value(i64::from(FILE_SCHEMA_VERSION)));
        }
        if document.get("settings").is_none() {
            document.insert("settings", Item::Table(Table::new()));
        }
        let settings = document
            .get_mut("settings")
            .and_then(Item::as_table_mut)
            .ok_or_else(|| FileSettingsError::parse(path, "`settings` must be a table"))?;
        settings.insert(namespace.as_str(), section_item(path, namespace, section)?);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| FileSettingsError::io(parent, source))?;
        }
        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false);
            options.mode(0o600);
        }
        let mut output = options
            .open(path)
            .map_err(|source| FileSettingsError::io(path, source))?;
        output
            .write_all(document.to_string().as_bytes())
            .map_err(|source| FileSettingsError::io(path, source))?;
        output
            .commit()
            .map_err(|source| FileSettingsError::io(path, source))?;
        Ok(())
    }
}

enum WatchMessage {
    Event(notify::Result<notify::Event>),
    Stop,
}

/// Effect-owned filesystem watcher and reload worker.
pub struct FileWatchHandle {
    watcher: Option<notify::RecommendedWatcher>,
    sender: mpsc::Sender<WatchMessage>,
    worker: Option<JoinHandle<()>>,
}

impl FileWatchHandle {
    fn stop(&mut self) {
        self.watcher.take();
        let _ = self.sender.send(WatchMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for FileWatchHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch_loop(
    receiver: mpsc::Receiver<WatchMessage>,
    provider: Arc<FileSettingsProvider>,
    service: SettingsService,
    targets: Vec<PathBuf>,
) {
    while let Ok(message) = receiver.recv() {
        match message {
            WatchMessage::Stop => return,
            WatchMessage::Event(Err(error)) => {
                provider.set_reload_error(Some(error.to_string()));
            }
            WatchMessage::Event(Ok(event)) if event_is_relevant(&event, &targets) => {
                // Atomic replace often emits a short burst. Drain it so one
                // durable generation produces one reload/notification. The
                // fixed deadline prevents a noisy directory from starving
                // publication indefinitely.
                let mut stop = false;
                let deadline = Instant::now() + Duration::from_millis(250);
                while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                    match receiver.recv_timeout(remaining.min(Duration::from_millis(40))) {
                        Ok(WatchMessage::Stop) => {
                            stop = true;
                            break;
                        }
                        Ok(WatchMessage::Event(_)) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if stop {
                    return;
                }
                match provider.reload_into(&service) {
                    Ok(()) => provider.set_reload_error(None),
                    Err(error) => provider.set_reload_error(Some(error.to_string())),
                }
            }
            WatchMessage::Event(Ok(_)) => {}
        }
    }
}

fn event_is_relevant(event: &notify::Event, targets: &[PathBuf]) -> bool {
    !matches!(event.kind, notify::EventKind::Access(_))
        && event.paths.iter().any(|event_path| {
            targets.iter().any(|target| {
                event_path == target
                    || target.parent().is_some_and(|parent| {
                        event_path == parent || event_path.parent() == Some(parent)
                    })
            })
        })
}

fn normalize_watch_target(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Ok(canonical_parent) = std::fs::canonicalize(parent) else {
        return path.to_path_buf();
    };
    match path.file_name() {
        Some(name) => canonical_parent.join(name),
        None => canonical_parent,
    }
}

impl FileSettingsProvider {
    fn set_reload_error(&self, error: Option<String>) {
        if let Ok(mut slot) = self.last_reload_error.lock() {
            *slot = error;
        }
    }
}

impl SettingsWriter for FileSettingsProvider {
    fn persist_user(&self, namespace: &SettingsNamespace, section: &Value) -> Result<(), String> {
        self.persist(namespace, section)
            .map_err(|error| error.to_string())
    }
}

fn section_item(
    path: &std::path::Path,
    namespace: &SettingsNamespace,
    section: &Value,
) -> Result<Item, FileSettingsError> {
    let mut one = BTreeMap::new();
    one.insert(namespace.as_str(), section);
    let mut generated = toml_edit::ser::to_document(&one)
        .map_err(|error| FileSettingsError::parse(path, error.to_string()))?;
    let item = generated
        .remove(namespace.as_str())
        .ok_or_else(|| FileSettingsError::parse(path, "failed to serialize namespace section"))?;
    item.into_table()
        .map(Item::Table)
        .map_err(|_| FileSettingsError::parse(path, "serialized namespace section was not a table"))
}

fn read_optional_regular(path: &std::path::Path) -> Result<Option<String>, FileSettingsError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(FileSettingsError::io(path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(FileSettingsError::SymbolicLink {
            path: path.display().to_string(),
        });
    }
    if !metadata.is_file() {
        return Err(FileSettingsError::NotRegular {
            path: path.display().to_string(),
        });
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|source| FileSettingsError::io(path, source))
}

fn parse_disk(path: &std::path::Path, raw: &str) -> Result<DiskDocument, FileSettingsError> {
    let document: DiskDocument =
        toml::from_str(raw).map_err(|error| FileSettingsError::parse(path, error.message()))?;
    validate_version(path, document.schema_version.map(i64::from))?;
    Ok(document)
}

fn validate_version(
    path: &std::path::Path,
    raw_version: Option<i64>,
) -> Result<(), FileSettingsError> {
    let Some(raw_version) = raw_version else {
        return Ok(());
    };
    let version = u32::try_from(raw_version)
        .map_err(|_| FileSettingsError::parse(path, "`schema_version` must be non-negative"))?;
    if version > FILE_SCHEMA_VERSION {
        return Err(FileSettingsError::NewerSchema {
            path: path.display().to_string(),
            found: version,
            supported: FILE_SCHEMA_VERSION,
        });
    }
    Ok(())
}
