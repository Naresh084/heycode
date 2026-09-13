//! Owner-only atomic JSON catalog persistence.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use atomic_write_file::AtomicWriteFile;
use heycode_llm::{CatalogPersistence, CatalogPersistenceError, CatalogSnapshot};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::wire::WireDocument;
use crate::{FILE_SCHEMA_VERSION, FileCatalogConfig, FileCatalogError, MIN_FILE_SCHEMA_VERSION};

const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default)]
    schema_version: Option<u32>,
}

/// Versioned owner-only whole-generation catalog store.
pub struct FileCatalogPersistence {
    config: FileCatalogConfig,
    writer: Mutex<()>,
}

impl FileCatalogPersistence {
    /// Secure the parent/path and validate any existing document.
    ///
    /// # Errors
    /// Unsafe paths, unsupported/malformed schemas and I/O failures.
    pub fn open(config: FileCatalogConfig) -> Result<Self, FileCatalogError> {
        secure_parent(&config.path)?;
        if config.path.exists() {
            secure_regular(&config.path)?;
        }
        let persistence = Self {
            config,
            writer: Mutex::new(()),
        };
        let _validated = persistence.load_file()?;
        Ok(persistence)
    }

    fn load_file(&self) -> Result<Vec<CatalogSnapshot>, FileCatalogError> {
        if !self.config.path.exists() {
            return Ok(Vec::new());
        }
        secure_regular(&self.config.path)?;
        let metadata = std::fs::metadata(&self.config.path)
            .map_err(|source| FileCatalogError::io(&self.config.path, source))?;
        if metadata.len() > MAX_CACHE_BYTES {
            return Err(FileCatalogError::TooLarge {
                path: self.config.path.display().to_string(),
                max_bytes: MAX_CACHE_BYTES,
            });
        }
        let bytes = std::fs::read(&self.config.path)
            .map_err(|source| FileCatalogError::io(&self.config.path, source))?;
        let probe: VersionProbe = serde_json::from_slice(&bytes)
            .map_err(|error| FileCatalogError::parse(&self.config.path, error.to_string()))?;
        let found = probe.schema_version.unwrap_or(0);
        if found > FILE_SCHEMA_VERSION {
            return Err(FileCatalogError::NewerSchema {
                path: self.config.path.display().to_string(),
                found,
                supported: FILE_SCHEMA_VERSION,
            });
        }
        if found < MIN_FILE_SCHEMA_VERSION {
            return Err(FileCatalogError::OlderSchema {
                path: self.config.path.display().to_string(),
                found,
                supported: MIN_FILE_SCHEMA_VERSION,
            });
        }
        let document: WireDocument = serde_json::from_slice(&bytes)
            .map_err(|error| FileCatalogError::parse(&self.config.path, error.to_string()))?;
        document
            .into_snapshots()
            .map_err(|error| FileCatalogError::parse(&self.config.path, error))
    }

    fn save_file(
        &self,
        generations: &[Arc<CatalogSnapshot>],
        cancellation: &CancellationToken,
    ) -> Result<(), FileCatalogError> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| FileCatalogError::WriterUnavailable)?;
        if cancellation.is_cancelled() {
            return Err(FileCatalogError::Cancelled);
        }
        secure_parent(&self.config.path)?;
        if self.config.path.exists() {
            secure_regular(&self.config.path)?;
        }
        let document = WireDocument::from_snapshots(generations)
            .map_err(|error| FileCatalogError::parse(&self.config.path, error))?;
        let mut bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| FileCatalogError::parse(&self.config.path, error.to_string()))?;
        bytes.push(b'\n');
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CACHE_BYTES {
            return Err(FileCatalogError::TooLarge {
                path: self.config.path.display().to_string(),
                max_bytes: MAX_CACHE_BYTES,
            });
        }
        if cancellation.is_cancelled() {
            return Err(FileCatalogError::Cancelled);
        }

        let mut options = AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut output = options
            .open(&self.config.path)
            .map_err(|source| FileCatalogError::io(&self.config.path, source))?;
        output
            .write_all(&bytes)
            .map_err(|source| FileCatalogError::io(&self.config.path, source))?;
        if cancellation.is_cancelled() {
            return Err(FileCatalogError::Cancelled);
        }
        output
            .commit()
            .map_err(|source| FileCatalogError::io(&self.config.path, source))
    }
}

impl CatalogPersistence for FileCatalogPersistence {
    fn load(&self) -> Result<Vec<CatalogSnapshot>, CatalogPersistenceError> {
        let _guard = self.writer.lock().map_err(|_| {
            CatalogPersistenceError::new(FileCatalogError::WriterUnavailable.to_string())
        })?;
        self.load_file()
            .map_err(|error| CatalogPersistenceError::new(error.to_string()))
    }

    fn save(
        &self,
        generations: &[Arc<CatalogSnapshot>],
        cancellation: &CancellationToken,
    ) -> Result<(), CatalogPersistenceError> {
        self.save_file(generations, cancellation)
            .map_err(|error| CatalogPersistenceError::new(error.to_string()))
    }
}

fn secure_parent(path: &std::path::Path) -> Result<(), FileCatalogError> {
    let parent = path.parent().ok_or_else(|| {
        FileCatalogError::parse(path, "catalog cache path must have a parent directory")
    })?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(FileCatalogError::SymbolicLink {
                path: parent.display().to_string(),
            });
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(FileCatalogError::WrongFileType {
                path: parent.display().to_string(),
                expected: "a directory",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent)
                .map_err(|source| FileCatalogError::io(parent, source))?;
        }
        Err(source) => return Err(FileCatalogError::io(parent, source)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| FileCatalogError::io(parent, source))?;
    }
    Ok(())
}

fn secure_regular(path: &std::path::Path) -> Result<(), FileCatalogError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| FileCatalogError::io(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(FileCatalogError::SymbolicLink {
            path: path.display().to_string(),
        });
    }
    if !metadata.is_file() {
        return Err(FileCatalogError::WrongFileType {
            path: path.display().to_string(),
            expected: "a regular file",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|source| FileCatalogError::io(path, source))?;
    }
    Ok(())
}
