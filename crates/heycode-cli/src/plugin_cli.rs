//! PL06 command-line surface for plugin lifecycle.
//!
//! Same shape as `mcp_cli`: parsing and rendering are pure functions over a
//! closed `PluginOperation`, so a TUI panel (U13) drives the same
//! `PluginLifecycle` and parity is structural rather than maintained by hand.

use heycode_extensions::lifecycle::{LifecycleError, PluginLifecycle, PluginOperation};
use heycode_extensions::{PluginId, PluginVersion};

/// One fully parsed lifecycle command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginCommand {
    /// `heycode plugin install <directory>`: verify a package directory into the
    /// cache, then install it.
    InstallDirectory {
        /// Absolute or relative path to the package directory.
        path: std::path::PathBuf,
    },
    /// `heycode plugin install <id> <version>`
    Install {
        /// Plugin identity.
        id: PluginId,
        /// Version to activate.
        version: PluginVersion,
    },
    /// `heycode plugin enable <id>` / `disable <id>`
    SetEnabled {
        /// Plugin identity.
        id: PluginId,
        /// Whether to activate.
        enabled: bool,
    },
    /// `heycode plugin update <id> <version>`
    Update {
        /// Plugin identity.
        id: PluginId,
        /// Version to move to.
        version: PluginVersion,
    },
    /// `heycode plugin rollback <id>`
    Rollback {
        /// Plugin identity.
        id: PluginId,
    },
    /// `heycode plugin remove <id>`
    Remove {
        /// Plugin identity.
        id: PluginId,
    },
}

impl PluginCommand {
    /// Which operation this command invokes.
    ///
    /// `enable` and `disable` share one command carrying a flag, so this is the
    /// place the two words rejoin the closed operation set.
    #[must_use]
    pub const fn operation(&self) -> PluginOperation {
        match self {
            Self::Install { .. } | Self::InstallDirectory { .. } => PluginOperation::Install,
            Self::SetEnabled { enabled: true, .. } => PluginOperation::Enable,
            Self::SetEnabled { enabled: false, .. } => PluginOperation::Disable,
            Self::Update { .. } => PluginOperation::Update,
            Self::Rollback { .. } => PluginOperation::Rollback,
            Self::Remove { .. } => PluginOperation::Remove,
        }
    }
}

/// Usage text generated from the operation set, so it cannot omit an operation.
#[must_use]
pub fn usage() -> String {
    let mut text = String::from("usage: heycode plugin <operation>\n\noperations:\n");
    for operation in PluginOperation::ALL {
        let detail = match operation {
            PluginOperation::Install => "<directory> | <id> <version>",
            PluginOperation::Update => "<id> <version>",
            PluginOperation::Enable
            | PluginOperation::Disable
            | PluginOperation::Rollback
            | PluginOperation::Remove => "<id>",
        };
        text.push_str(&format!("  {:<9} {detail}\n", operation.as_str()));
    }
    text.push_str("  list\n");
    text
}

/// Parse `heycode plugin ...` arguments.
///
/// # Errors
/// A message suitable for stderr; an unknown operation lists what is available.
pub fn parse(args: &[String]) -> Result<Option<PluginCommand>, String> {
    let Some(word) = args.first() else {
        return Err(usage());
    };
    // `list` is a read, not a lifecycle transition, so it is not part of the
    // closed operation set — putting it there would force every surface to
    // treat a query as a state change.
    if word == "list" {
        return Ok(None);
    }
    let operation = PluginOperation::parse(word)
        .ok_or_else(|| format!("unknown plugin operation `{word}`\n\n{}", usage()))?;

    // `install <directory>`: one argument that names an existing directory is a
    // package to verify into the cache, not a plugin id.
    if operation == PluginOperation::Install
        && args.len() == 2
        && std::path::Path::new(&args[1]).is_dir()
    {
        return Ok(Some(PluginCommand::InstallDirectory {
            path: std::path::PathBuf::from(&args[1]),
        }));
    }
    let id = args
        .get(1)
        .ok_or_else(|| format!("`heycode plugin {operation}` needs a plugin id"))
        .and_then(|raw| {
            PluginId::new(raw.clone()).map_err(|error| format!("invalid plugin id: {error}"))
        })?;
    let version = |index: usize| -> Result<PluginVersion, String> {
        let raw = args
            .get(index)
            .ok_or_else(|| format!("`heycode plugin {operation}` needs a version"))?;
        PluginVersion::parse(raw.clone()).map_err(|error| format!("invalid version: {error}"))
    };

    Ok(Some(match operation {
        PluginOperation::Install => PluginCommand::Install {
            id,
            version: version(2)?,
        },
        PluginOperation::Update => PluginCommand::Update {
            id,
            version: version(2)?,
        },
        PluginOperation::Enable => PluginCommand::SetEnabled { id, enabled: true },
        PluginOperation::Disable => PluginCommand::SetEnabled { id, enabled: false },
        PluginOperation::Rollback => PluginCommand::Rollback { id },
        PluginOperation::Remove => PluginCommand::Remove { id },
    }))
}

/// Run one parsed command, or render the listing when `command` is `None`.
///
/// # Errors
/// The lifecycle error, rendered for a terminal.
pub fn run(lifecycle: &PluginLifecycle, command: Option<&PluginCommand>) -> Result<String, String> {
    run_with_cache(lifecycle, None, command)
}

/// [`run`] with the package cache root, which `install <directory>` needs.
///
/// # Errors
/// The lifecycle or cache error, rendered for a terminal.
pub fn run_with_cache(
    lifecycle: &PluginLifecycle,
    cache_root: Option<&std::path::Path>,
    command: Option<&PluginCommand>,
) -> Result<String, String> {
    let Some(command) = command else {
        let states = lifecycle.list().map_err(render)?;
        return Ok(if states.is_empty() {
            "no plugins installed".to_owned()
        } else {
            states
                .iter()
                .map(|state| {
                    format!(
                        "{:<28} {:<12} {:<9} {}",
                        state.id.as_str(),
                        state.active.to_string(),
                        if state.enabled { "enabled" } else { "disabled" },
                        state.previous.as_ref().map_or_else(
                            || "(no rollback target)".to_owned(),
                            |previous| format!("rollback -> {previous}")
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
    };

    Ok(match command {
        PluginCommand::InstallDirectory { path } => {
            let cache_root = cache_root
                .ok_or_else(|| "installing from a directory needs the package cache".to_owned())?;
            let source = std::path::absolute(path)
                .map_err(|error| format!("cannot resolve `{}`: {error}", path.display()))?;
            let cache = open_cache(cache_root).map_err(|error| error.to_string())?;
            let installed = cache
                .install_directory(&source)
                .map_err(|error| format!("package at `{}`: {error}", source.display()))?;
            let id = installed.manifest().id().clone();
            let version = installed.manifest().version().clone();
            lifecycle.install(&id, &version).map_err(render)?;
            format!(
                "installed `{}` {version} from {}",
                id.as_str(),
                source.display()
            )
        }
        PluginCommand::Install { id, version } => {
            lifecycle.install(id, version).map_err(render)?;
            format!("installed `{}` {version}", id.as_str())
        }
        PluginCommand::SetEnabled { id, enabled } => {
            lifecycle.set_enabled(id, *enabled).map_err(render)?;
            format!(
                "{} `{}`",
                if *enabled { "enabled" } else { "disabled" },
                id.as_str()
            )
        }
        PluginCommand::Update { id, version } => {
            lifecycle.update(id, version).map_err(render)?;
            format!("updated `{}` to {version}", id.as_str())
        }
        PluginCommand::Rollback { id } => {
            let restored = lifecycle.rollback(id).map_err(render)?;
            format!("rolled `{}` back to {restored}", id.as_str())
        }
        PluginCommand::Remove { id } => {
            lifecycle.remove(id).map_err(render)?;
            format!("removed `{}`", id.as_str())
        }
    })
}

fn render(error: LifecycleError) -> String {
    error.to_string()
}

/// Compose the smallest world plugin lifecycle needs.
///
/// Settings plus the lifecycle namespace, and a read-only view of the package
/// cache. Same reasoning as `heycode mcp`: managing plugins must not require a
/// provider credential or the inference stack.
///
/// # Errors
/// Settings, cache, or plugin activation failure.
pub fn compose_lifecycle_world(
    settings_user_path: std::path::PathBuf,
    cache_root: std::path::PathBuf,
) -> anyhow::Result<(heycode_core::Context, std::sync::Arc<PluginLifecycle>)> {
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_settings_file::file_settings_plugin(
            heycode_settings_file::FileSettingsConfig::user(settings_user_path).without_watch(),
        ),
        managed_lifecycle_plugin(cache_root.clone()),
    ];
    let context = heycode_core::compose(&plugins)?;
    let lifecycle = context
        .get::<PluginLifecycle>(heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE)
        .ok_or_else(|| anyhow::anyhow!("plugin lifecycle service is not mounted"))?;
    Ok((context, lifecycle))
}

/// Build the product lifecycle plugin over the verified cache view.
///
/// Standalone CLI and full product composition call this same factory so
/// package visibility, mandatory managed-policy admission and effect-owned
/// service shutdown cannot drift.
#[must_use]
pub fn managed_lifecycle_plugin(cache_root: std::path::PathBuf) -> Box<dyn heycode_core::Plugin> {
    let policy_root = cache_root.clone();
    heycode_extensions::lifecycle::plugin_lifecycle_plugin(
        std::sync::Arc::new(CacheVersions::open(cache_root)),
        std::sync::Arc::new(
            heycode_extensions::lifecycle::DeclarativeUserPluginPolicy::new(move |id, version| {
                // Asking must not create the cache: an absent cache holds no
                // versions, so the lifecycle reports "not cached" itself.
                if !policy_root.is_dir() {
                    return None;
                }
                let cache = open_cache(&policy_root).ok()?;
                let installed = cache.resolve(id, version).ok()?;
                Some(installed.manifest().code().is_some())
            }),
        ),
    )
}

/// Open the verified package cache for this build's host.
///
/// # Errors
/// An unsafe or malformed cache root.
pub fn open_cache(
    root: &std::path::Path,
) -> Result<heycode_extensions::PluginInstallCache, heycode_extensions::PackageCacheError> {
    let api = heycode_extensions::ApiVersion::new(1)
        .map_err(|_| heycode_extensions::PackageCacheError::InvalidRoot)?;
    let validator = heycode_extensions::ManifestValidator::new(api, host_platform());
    heycode_extensions::PluginInstallCache::open(root, validator)
}

/// Build the path-free package facts consumed by the interactive plugin panel.
///
/// Every row is first verified by the cache inspection and then resolved again
/// to attach manifest provenance and permission requests. An absent cache is an
/// empty index; malformed or unsafe state fails instead of disappearing.
///
/// # Errors
/// Cache admission, integrity or manifest resolution failure.
pub fn package_index(
    cache_root: &std::path::Path,
) -> anyhow::Result<heycode_tui::plugin_panel::PluginPackageIndex> {
    let mut index = heycode_tui::plugin_panel::PluginPackageIndex::new();
    match std::fs::symlink_metadata(cache_root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(index),
        Err(_) => anyhow::bail!("plugin cache inspection unavailable"),
        Ok(_) => {}
    }
    let api = heycode_extensions::ApiVersion::new(1)?;
    let validator = heycode_extensions::ManifestValidator::new(api, host_platform());
    let cache = heycode_extensions::PluginInstallCache::open(cache_root, validator)?;
    for summary in cache.inspect()?.packages {
        index.add_package(
            &summary.id,
            &summary.version,
            summary.content_hash.as_str(),
            summary.contribution_count,
            summary.default_enabled,
        );
        let installed = cache.resolve(&summary.id, &summary.version)?;
        index.add_manifest(installed.manifest());
    }
    Ok(index)
}

/// A read-only view of which versions the package cache holds.
///
/// Opened lazily and tolerant of an absent cache: a user who has installed
/// nothing has no cache directory, and that must read as "no versions", not as
/// an error before they can even see the empty list.
struct CacheVersions {
    root: std::path::PathBuf,
}

impl CacheVersions {
    const fn open(root: std::path::PathBuf) -> Self {
        Self { root }
    }
}

impl heycode_extensions::lifecycle::InstalledVersions for CacheVersions {
    fn versions(
        &self,
        id: &heycode_extensions::PluginId,
    ) -> Vec<heycode_extensions::PluginVersion> {
        // An absent cache holds no versions, and reading must not create it.
        if !self.root.is_dir() {
            return Vec::new();
        }
        // Inspection only reads committed references, so the validator's host
        // API and platform never gate this call; they matter on install.
        let Ok(api) = heycode_extensions::ApiVersion::new(1) else {
            return Vec::new();
        };
        let validator = heycode_extensions::ManifestValidator::new(api, host_platform());
        let Ok(cache) = heycode_extensions::PluginInstallCache::open(&self.root, validator) else {
            return Vec::new();
        };
        let Ok(snapshot) = cache.inspect() else {
            return Vec::new();
        };
        snapshot
            .packages
            .into_iter()
            .filter(|package| &package.id == id)
            .map(|package| package.version)
            .collect()
    }
}

/// This build's platform target.
pub(crate) fn host_platform() -> heycode_extensions::PlatformTarget {
    use heycode_extensions::{Architecture, OperatingSystem, PlatformTarget};
    let os = if cfg!(target_os = "macos") {
        OperatingSystem::Macos
    } else if cfg!(target_os = "windows") {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    };
    let arch = if cfg!(target_arch = "aarch64") {
        Architecture::Aarch64
    } else {
        Architecture::X86_64
    };
    PlatformTarget::new(os, arch)
}
