//! Injected process-environment and file-read boundary.
//!
//! Application Default Credentials discovery is defined entirely in terms of
//! environment variables and well-known file paths. Both cross this trait, so
//! every discovery rule is exercised deterministically in tests without
//! touching the host environment or filesystem.

use std::path::Path;

/// Why one bounded file read produced no bytes.
///
/// The distinction is load bearing: a path named by
/// `GOOGLE_APPLICATION_CREDENTIALS` that does not exist is a *fault* — ADC
/// stops there rather than falling through — while a missing well-known
/// gcloud file simply means that source is not configured.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcpFileError {
    /// The path does not exist.
    NotFound,
    /// The path exists but is not a readable regular file.
    Unreadable,
    /// The file is larger than the caller's bound.
    TooLarge,
}

/// Read-only host boundary used to discover Application Default Credentials.
pub trait GcpEnvironment: Send + Sync {
    /// One process environment variable. A blank value is absent.
    fn var(&self, name: &str) -> Option<String>;

    /// Read at most `max_bytes` from `path`.
    ///
    /// # Errors
    /// [`GcpFileError::NotFound`] for an absent path, [`GcpFileError::TooLarge`]
    /// when the file exceeds `max_bytes`, and [`GcpFileError::Unreadable`] for
    /// every other failure. Implementations never surface an operating-system
    /// message, which can echo path or account detail.
    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, GcpFileError>;
}

/// The live process environment and filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessGcpEnvironment;

impl GcpEnvironment for ProcessGcpEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, GcpFileError> {
        let metadata = std::fs::metadata(path).map_err(classify)?;
        if !metadata.is_file() {
            return Err(GcpFileError::Unreadable);
        }
        // Two bounds on purpose, and they guard different things. The first
        // refuses a hostile file before it is allocated; the second is the
        // authoritative one, because a file may grow between `metadata` and
        // `read`. Removing either alone leaves observable behavior unchanged
        // on an ordinary file, which is why only removing both turns a test
        // red.
        if metadata.len() > max_bytes as u64 {
            return Err(GcpFileError::TooLarge);
        }
        let bytes = std::fs::read(path).map_err(classify)?;
        if bytes.len() > max_bytes {
            return Err(GcpFileError::TooLarge);
        }
        Ok(bytes)
    }
}

fn classify(error: std::io::Error) -> GcpFileError {
    if error.kind() == std::io::ErrorKind::NotFound {
        GcpFileError::NotFound
    } else {
        GcpFileError::Unreadable
    }
}
