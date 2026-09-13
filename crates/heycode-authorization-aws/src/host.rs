//! Read-only view of the host facts AWS configuration is discovered from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Outcome of one configuration-file read.
///
/// "The file is not there" and "the file is there but I could not read it"
/// are different statements. Collapsing them would let an unreadable
/// `~/.aws/config` report as *no AWS configuration*, which is the one answer
/// that is certainly wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwsFileRead {
    /// The file exists and decoded as UTF-8 text.
    Found(String),
    /// The path does not exist.
    Absent,
    /// The path exists but could not be read as UTF-8 text.
    Unreadable,
}

/// Ambient host facts AWS discovery reads.
///
/// Every host access in this crate goes through this trait, so discovery is
/// deterministic under test and no code path reaches for process state on its
/// own.
pub trait AwsHost: Send + Sync {
    /// One process environment variable. Empty and whitespace-only values are
    /// absent, matching how the AWS SDKs treat a cleared variable.
    fn var(&self, name: &str) -> Option<String>;

    /// Read one configuration file.
    fn read_file(&self, path: &Path) -> AwsFileRead;

    /// The user's home directory, when one is known.
    fn home(&self) -> Option<PathBuf>;
}

/// The real process environment and filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessAwsHost;

impl AwsHost for ProcessAwsHost {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    fn read_file(&self, path: &Path) -> AwsFileRead {
        match std::fs::read_to_string(path) {
            Ok(text) => AwsFileRead::Found(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AwsFileRead::Absent,
            Err(_) => AwsFileRead::Unreadable,
        }
    }

    fn home(&self) -> Option<PathBuf> {
        self.var("HOME")
            .or_else(|| self.var("USERPROFILE"))
            .map(PathBuf::from)
    }
}

/// Deterministic map-backed host for tests and embedders.
#[derive(Debug, Clone, Default)]
pub struct MapAwsHost {
    vars: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, AwsFileRead>,
    home: Option<PathBuf>,
}

impl MapAwsHost {
    /// An empty host: no variables, no files, no home.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set one environment variable.
    #[must_use]
    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Set one readable file.
    #[must_use]
    pub fn with_file(mut self, path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        self.files
            .insert(path.into(), AwsFileRead::Found(contents.into()));
        self
    }

    /// Set one path that exists but cannot be read.
    #[must_use]
    pub fn with_unreadable_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.files.insert(path.into(), AwsFileRead::Unreadable);
        self
    }

    /// Set the home directory.
    #[must_use]
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }
}

impl AwsHost for MapAwsHost {
    fn var(&self, name: &str) -> Option<String> {
        self.vars
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }

    fn read_file(&self, path: &Path) -> AwsFileRead {
        self.files.get(path).cloned().unwrap_or(AwsFileRead::Absent)
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
}
