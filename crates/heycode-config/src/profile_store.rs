//! Safe named-profile discovery under `$HEYCODE_HOME/profiles`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heycode_core::{Context, CoreError, Plugin, PluginScope, ServiceKey};

use crate::scopes::validate_plugin_id;
use crate::{ConfigError, ProfileDocument, ProfileLayer, ProfileSource};

const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

/// Effect-owned named-profile discovery used by interactive Consumers.
pub const SERVICE_PROFILES: ServiceKey = ServiceKey::new("profiles");

/// Picker-safe named profile metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedProfileSummary {
    /// Stable selected name (file stem).
    pub name: String,
    /// Exact trusted file path.
    pub path: PathBuf,
}

/// Shared named-profile store used by CLI and future picker consumers.
#[derive(Debug, Clone)]
pub struct NamedProfileStore {
    root: PathBuf,
}

/// Live profile picker boundary over the same store the CLI uses.
#[derive(Clone)]
pub struct NamedProfileService {
    store: NamedProfileStore,
    stopped: Arc<AtomicBool>,
}

impl NamedProfileService {
    fn new(store: NamedProfileStore) -> Self {
        Self {
            store,
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// List strict named profiles in picker order.
    ///
    /// # Errors
    /// Same boundary failures as [`NamedProfileStore::list`], or a stopped
    /// plugin generation.
    pub fn list(&self) -> Result<Vec<NamedProfileSummary>, ConfigError> {
        self.ensure_live()?;
        self.store.list()
    }

    /// Load the exact User-scope layer a CLI `--profile` selection would use.
    ///
    /// # Errors
    /// Same boundary failures as [`NamedProfileStore::load`], or a stopped
    /// plugin generation.
    pub fn load(&self, name: &str) -> Result<ProfileLayer, ConfigError> {
        self.ensure_live()?;
        self.store.load(name)
    }

    fn ensure_live(&self) -> Result<(), ConfigError> {
        if self.stopped.load(Ordering::SeqCst) {
            Err(profile_store_error("named profile service is stopped"))
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Debug for NamedProfileService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedProfileService")
            .field("stopped", &self.stopped.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl NamedProfileStore {
    /// Resolve the fixed `<heycode-home>/profiles` root.
    #[must_use]
    pub fn new(heycode_home: impl AsRef<Path>) -> Self {
        Self {
            root: heycode_home.as_ref().join("profiles"),
        }
    }

    /// Profiles directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// List every strict valid `.toml` profile sorted by name. An absent store
    /// is empty; one malformed/unsafe profile fails the generation.
    ///
    /// # Errors
    /// Unsafe root/file type, I/O, schema, name, or size failure.
    pub fn list(&self) -> Result<Vec<NamedProfileSummary>, ConfigError> {
        if !self.validate_root()? {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&self.root).map_err(|source| ConfigError::Io {
            path: self.root.display().to_string(),
            source,
        })?;
        let mut profiles = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ConfigError::Io {
                path: self.root.display().to_string(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| profile_store_error("profile filename is not valid Unicode"))?
                .to_owned();
            validate_plugin_id(&name)?;
            let layer = self.load(&name)?;
            let path = match layer.source {
                ProfileSource::NamedProfile(path) => path,
                _ => {
                    return Err(profile_store_error(
                        "named profile resolved to an impossible source",
                    ));
                }
            };
            profiles.push(NamedProfileSummary { name, path });
        }
        profiles.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(profiles)
    }

    /// Load one strict named profile as the User-scope layer used by both CLI
    /// and picker selection.
    ///
    /// # Errors
    /// Traversal/malformed name, unsafe root/file, cap, parse/version, or
    /// embedded-name mismatch.
    pub fn load(&self, name: &str) -> Result<ProfileLayer, ConfigError> {
        validate_plugin_id(name)?;
        if !self.validate_root()? {
            return Err(profile_store_error(format!(
                "named profile `{name}` does not exist"
            )));
        }
        let path = self.root.join(format!("{name}.toml"));
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(profile_store_error(format!(
                "named profile path is not a regular non-symlink file: {}",
                path.display()
            )));
        }
        if metadata.len() > MAX_PROFILE_BYTES {
            return Err(profile_store_error(format!(
                "named profile `{name}` exceeds the {MAX_PROFILE_BYTES}-byte limit"
            )));
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let document = ProfileDocument::from_toml(&raw)?;
        if let Some(inside) = document.name.as_deref()
            && inside != name
        {
            return Err(profile_store_error(format!(
                "profile `{name}` declares mismatched name `{inside}`"
            )));
        }
        ProfileLayer::new(PluginScope::User, ProfileSource::named(path), document)
    }

    fn validate_root(&self) -> Result<bool, ConfigError> {
        match std::fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(profile_store_error(format!(
                    "profile store is not a regular directory: {}",
                    self.root.display()
                )))
            }
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(ConfigError::Io {
                path: self.root.display().to_string(),
                source,
            }),
        }
    }
}

fn profile_store_error(message: impl Into<String>) -> ConfigError {
    ConfigError::Parse {
        path: "<profile-store>".to_owned(),
        message: message.into(),
    }
}

/// Publish the shared named-profile store for picker Consumers.
#[must_use]
pub fn named_profiles_plugin(heycode_home: impl AsRef<Path>) -> Box<dyn Plugin> {
    struct NamedProfilesPlugin(PathBuf);

    impl Plugin for NamedProfilesPlugin {
        fn name(&self) -> &'static str {
            "profiles"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_PROFILES]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let service = NamedProfileService::new(NamedProfileStore::new(&self.0));
            let stopped = service.stopped.clone();
            context.provide(SERVICE_PROFILES, self.name(), service)?;
            context.effect(move || stopped.store(true, Ordering::SeqCst));
            Ok(())
        }
    }

    Box::new(NamedProfilesPlugin(heycode_home.as_ref().to_path_buf()))
}
