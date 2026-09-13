//! Deterministic doubles for the injected environment boundary.
//!
//! Application Default Credentials discovery is defined over environment
//! variables and well-known paths, so a faithful test needs to control both.
//! [`MapGcpEnvironment`] does that without touching the host, which is the
//! only way the Windows layout can be exercised from a Unix CI leg and the
//! only way a credential fixture can carry realistic key material safely.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::env::{GcpEnvironment, GcpFileError};

/// An in-memory environment and filesystem.
#[derive(Debug, Clone, Default)]
pub struct MapGcpEnvironment {
    vars: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, Vec<u8>>,
    unreadable: BTreeMap<PathBuf, ()>,
}

impl MapGcpEnvironment {
    /// An environment with nothing set and no files.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one environment variable.
    #[must_use]
    pub fn with_var(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }

    /// Place one readable file.
    #[must_use]
    pub fn with_file(mut self, path: impl Into<PathBuf>, contents: impl Into<Vec<u8>>) -> Self {
        self.files.insert(path.into(), contents.into());
        self
    }

    /// Place one path that exists but cannot be read.
    #[must_use]
    pub fn with_unreadable(mut self, path: impl Into<PathBuf>) -> Self {
        self.unreadable.insert(path.into(), ());
        self
    }
}

impl GcpEnvironment for MapGcpEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        self.vars
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }

    fn read_file(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>, GcpFileError> {
        if self.unreadable.contains_key(path) {
            return Err(GcpFileError::Unreadable);
        }
        let bytes = self.files.get(path).ok_or(GcpFileError::NotFound)?;
        if bytes.len() > max_bytes {
            return Err(GcpFileError::TooLarge);
        }
        Ok(bytes.clone())
    }
}
