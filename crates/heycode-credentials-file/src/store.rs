//! Owner-only store operations and crash-safe legacy migration.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::Mutex;

use atomic_write_file::AtomicWriteFile;
use heycode_credentials::{
    CredentialProvider, CredentialProviderId, CredentialProviderState, CredentialQuery,
    CredentialSecret, CredentialSource,
};
use secrecy::zeroize::{Zeroize as _, Zeroizing};
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::{FILE_SCHEMA_VERSION, FileCredentialConfig, FileCredentialError};

#[derive(Deserialize, Default)]
struct DiskDocument {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    credentials: BTreeMap<String, String>,
}

struct SecretMap(BTreeMap<String, String>);

impl std::ops::Deref for SecretMap {
    type Target = BTreeMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SecretMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for SecretMap {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            value.zeroize();
        }
    }
}

/// Precedence-20 owner-only credential provider.
pub struct FileCredentialProvider {
    id: CredentialProviderId,
    config: FileCredentialConfig,
    writer: Mutex<()>,
}

impl FileCredentialProvider {
    /// Secure the root and migrate a legacy file before constructing.
    ///
    /// # Errors
    /// Path safety, mode, parsing, conflicts, backup, and I/O failures.
    pub fn open(config: FileCredentialConfig) -> Result<Self, FileCredentialError> {
        secure_root(&config.root)?;
        migrate_legacy(&config)?;
        let _validated = read_current(&config.current_path())?;
        Ok(Self {
            id: CredentialProviderId::new("file").map_err(|error| {
                FileCredentialError::parse(&config.current_path(), error.to_string())
            })?,
            config,
            writer: Mutex::new(()),
        })
    }

    fn read_values(&self) -> Result<SecretMap, FileCredentialError> {
        read_current(&self.config.current_path())
    }

    fn update(
        &self,
        reference: &str,
        secret: Option<&CredentialSecret>,
    ) -> Result<(), FileCredentialError> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| FileCredentialError::WriterUnavailable)?;
        let mut values = self.read_values()?;
        match secret {
            Some(secret) => {
                values.insert(reference.to_owned(), secret.expose().to_owned());
            }
            None => {
                values.remove(reference);
            }
        }
        write_current(&self.config.current_path(), &values)
    }
}

impl CredentialProvider for FileCredentialProvider {
    fn id(&self) -> &CredentialProviderId {
        &self.id
    }

    fn precedence(&self) -> u16 {
        20
    }

    fn inspect(&self, query: &CredentialQuery) -> Result<CredentialProviderState, String> {
        let values = self.read_values().map_err(|error| error.to_string())?;
        Ok(
            if values
                .get(query.reference.as_str())
                .is_some_and(|value| !value.trim().is_empty())
            {
                CredentialProviderState::configured(CredentialSource::File, true)
            } else {
                CredentialProviderState::unconfigured(true)
            },
        )
    }

    fn resolve(&self, query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
        let mut values = self.read_values().map_err(|error| error.to_string())?;
        Ok(values
            .remove(query.reference.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(CredentialSecret::new))
    }

    fn write(&self, query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
        self.update(query.reference.as_str(), Some(secret))
            .map_err(|error| error.to_string())
    }

    fn delete(&self, query: &CredentialQuery) -> Result<(), String> {
        self.update(query.reference.as_str(), None)
            .map_err(|error| error.to_string())
    }
}

fn migrate_legacy(config: &FileCredentialConfig) -> Result<(), FileCredentialError> {
    let legacy_path = config.legacy_path();
    if !legacy_path.exists() {
        return Ok(());
    }
    secure_regular(&legacy_path)?;
    let legacy_raw = Zeroizing::new(
        std::fs::read_to_string(&legacy_path)
            .map_err(|source| FileCredentialError::io(&legacy_path, source))?,
    );
    let legacy_values = parse_legacy(&legacy_path, &legacy_raw)?;
    let current_path = config.current_path();
    let mut current_values = read_current(&current_path)?;
    for (reference, legacy_value) in legacy_values.iter() {
        match current_values.get(reference) {
            Some(current_value) if current_value != legacy_value => {
                return Err(FileCredentialError::MigrationConflict {
                    reference: reference.clone(),
                });
            }
            Some(_) => {}
            None => {
                current_values.insert(reference.clone(), legacy_value.clone());
            }
        }
    }
    write_current(&current_path, &current_values)?;

    let backup_path = config.backup_path();
    match std::fs::read_to_string(&backup_path) {
        Ok(existing) if existing == *legacy_raw => secure_regular(&backup_path)?,
        Ok(_) => {
            return Err(FileCredentialError::BackupConflict {
                path: backup_path.display().to_string(),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            atomic_write(&backup_path, legacy_raw.as_bytes())?;
        }
        Err(source) => return Err(FileCredentialError::io(&backup_path, source)),
    }
    std::fs::remove_file(&legacy_path)
        .map_err(|source| FileCredentialError::io(&legacy_path, source))?;
    Ok(())
}

fn parse_legacy(path: &std::path::Path, raw: &str) -> Result<SecretMap, FileCredentialError> {
    let mut values = BTreeMap::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((reference, value)) = trimmed.split_once('=') else {
            return Err(FileCredentialError::parse(
                path,
                format!("line {}: expected REFERENCE=value", index + 1),
            ));
        };
        let reference = reference.trim();
        heycode_credentials::CredentialReference::new(reference).map_err(|error| {
            FileCredentialError::parse(path, format!("line {}: {error}", index + 1))
        })?;
        values.insert(reference.to_owned(), value.trim().to_owned());
    }
    Ok(SecretMap(values))
}

fn read_current(path: &std::path::Path) -> Result<SecretMap, FileCredentialError> {
    if !path.exists() {
        return Ok(SecretMap(BTreeMap::new()));
    }
    secure_regular(path)?;
    let raw = Zeroizing::new(
        std::fs::read_to_string(path).map_err(|source| FileCredentialError::io(path, source))?,
    );
    let document: DiskDocument =
        toml::from_str(&raw).map_err(|error| FileCredentialError::parse(path, error.message()))?;
    if let Some(found) = document.schema_version
        && found > FILE_SCHEMA_VERSION
    {
        return Err(FileCredentialError::NewerSchema {
            path: path.display().to_string(),
            found,
            supported: FILE_SCHEMA_VERSION,
        });
    }
    Ok(SecretMap(document.credentials))
}

fn write_current(
    path: &std::path::Path,
    values: &BTreeMap<String, String>,
) -> Result<(), FileCredentialError> {
    let mut document = DocumentMut::new();
    document.insert("schema_version", value(i64::from(FILE_SCHEMA_VERSION)));
    let mut credentials = Table::new();
    for (reference, secret) in values {
        credentials.insert(reference, value(secret.as_str()));
    }
    document.insert("credentials", Item::Table(credentials));
    let raw = Zeroizing::new(document.to_string());
    atomic_write(path, raw.as_bytes())
}

fn secure_root(root: &std::path::Path) -> Result<(), FileCredentialError> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(FileCredentialError::SymbolicLink {
                path: root.display().to_string(),
            });
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(FileCredentialError::WrongFileType {
                path: root.display().to_string(),
                expected: "a directory",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(root)
                .map_err(|source| FileCredentialError::io(root, source))?;
        }
        Err(source) => return Err(FileCredentialError::io(root, source)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| FileCredentialError::io(root, source))?;
    }
    Ok(())
}

fn secure_regular(path: &std::path::Path) -> Result<(), FileCredentialError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| FileCredentialError::io(path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(FileCredentialError::SymbolicLink {
            path: path.display().to_string(),
        });
    }
    if !metadata.is_file() {
        return Err(FileCredentialError::WrongFileType {
            path: path.display().to_string(),
            expected: "a regular file",
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|source| FileCredentialError::io(path, source))?;
    }
    Ok(())
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), FileCredentialError> {
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.preserve_mode(false).mode(0o600);
    }
    let mut output = options
        .open(path)
        .map_err(|source| FileCredentialError::io(path, source))?;
    output
        .write_all(bytes)
        .map_err(|source| FileCredentialError::io(path, source))?;
    output
        .commit()
        .map_err(|source| FileCredentialError::io(path, source))
}
