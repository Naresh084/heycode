//! PL03 host-neutral activation of installed declarative contributions.
//!
//! This module owns the boundary common to skills, commands, agent presets,
//! hooks, themes and provider declarations: load their primary documents from
//! one verified installed package, bound and freeze the text, reject active-set
//! collisions, then dispatch every kind through one real [`heycode_core::Plugin`]
//! activation. The domain registry owns the document schema and the capability
//! it creates. Its adapter receives the live [`heycode_core::Context`] so every
//! successful registration can carry the disposer K09 requires.
//!
//! Bundled MCP remains outside the six-domain dispatch trait because PL04 owns
//! its transport-aware host. [`DeclarativePackage`] nevertheless freezes those
//! documents and retains the verified immutable package root, so a concrete
//! product host never reopens an unverified source tree.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use heycode_core::{
    Context, CoreError, Plugin, PluginContributionKind, PluginContributionSpec, PluginDescriptor,
    ServiceKey,
};
use thiserror::Error;

use crate::cache_source::{PackageEntryData, PreparedPackage};
use crate::{
    ContributionKind, InstalledPlugin, PluginId, PluginManifest, PluginPath, PluginVersion,
};

/// Static core plugin id for the aggregate declarative activation bridge.
///
/// External package ids remain on every [`DeclarativeContribution`]. Core's
/// current plugin descriptor identity is `&'static str`, so one bridge plugin
/// owns the dynamic active set until the composition root gains a dynamic
/// package attribution plane.
pub const DECLARATIVE_ACTIVATION_PLUGIN_ID: &str = "declarative-extensions";

const MAX_CONTRIBUTIONS: usize = 256;
const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;
const MAX_GENERATION_BYTES: usize = 16 * 1024 * 1024;

/// One immutable installed contribution document ready for its domain host.
#[derive(Clone)]
pub struct DeclarativeContribution {
    package_id: PluginId,
    package_version: PluginVersion,
    kind: ContributionKind,
    local_id: String,
    public_name: String,
    path: PluginPath,
    document: Arc<str>,
}

impl DeclarativeContribution {
    /// Package that supplied this contribution.
    #[must_use]
    pub const fn package_id(&self) -> &PluginId {
        &self.package_id
    }

    /// Exact installed package version.
    #[must_use]
    pub const fn package_version(&self) -> &PluginVersion {
        &self.package_version
    }

    /// Exact PL03 contribution registry.
    #[must_use]
    pub const fn kind(&self) -> ContributionKind {
        self.kind
    }

    /// Package-local manifest id.
    #[must_use]
    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Effective namespaced or explicitly allowed override name.
    #[must_use]
    pub fn public_name(&self) -> &str {
        &self.public_name
    }

    /// Manifest-validated path within the immutable package.
    #[must_use]
    pub const fn path(&self) -> &PluginPath {
        &self.path
    }

    /// Complete bounded UTF-8 primary document.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }
}

impl fmt::Debug for DeclarativeContribution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeclarativeContribution")
            .field("package_id", &self.package_id)
            .field("package_version", &self.package_version)
            .field("kind", &self.kind)
            .field("local_id", &self.local_id)
            .field("public_name", &self.public_name)
            .field("path", &self.path)
            .field("document_bytes", &self.document.len())
            .finish()
    }
}

/// One installed package's immutable PL03 activation input.
#[derive(Clone)]
pub struct DeclarativePackage {
    manifest: PluginManifest,
    contributions: Vec<DeclarativeContribution>,
    mcp_contributions: Vec<DeclarativeContribution>,
    package_root: Option<std::path::PathBuf>,
}

impl DeclarativePackage {
    /// Load every PL03 primary document from a verified installed package.
    ///
    /// The package root and every path component are opened without following
    /// symlinks. Files are regular, single-link, bounded, read exactly once and
    /// re-observed before their text publishes. The returned strings are
    /// immutable snapshots; domain hosts never need to re-open an ambient path.
    ///
    /// # Errors
    /// Unsafe or changing package state, missing/non-UTF-8/oversized documents,
    /// unsupported host security, or generation resource limits fail before a
    /// package can participate in composition.
    pub fn load(installed: &InstalledPlugin) -> Result<Self, DeclarativePluginError> {
        let activatable = installed.manifest().contributions().len();
        if activatable > MAX_CONTRIBUTIONS {
            return Err(DeclarativePluginError::TooManyContributions {
                package: installed.manifest().id().clone(),
                limit: MAX_CONTRIBUTIONS,
            });
        }

        let reader = PackageReader::open(installed.package_root())?;
        let mut total = 0_usize;
        let mut contributions = Vec::with_capacity(activatable);
        let mut mcp_contributions = Vec::new();
        for declared in installed.manifest().contributions() {
            let document =
                reader
                    .read(declared.path())
                    .map_err(|fault| DeclarativePluginError::Document {
                        package: installed.manifest().id().clone(),
                        kind: declared.kind(),
                        name: declared.public_name().to_owned(),
                        fault,
                    })?;
            total = total.checked_add(document.len()).ok_or_else(|| {
                DeclarativePluginError::GenerationTooLarge {
                    package: installed.manifest().id().clone(),
                    limit: MAX_GENERATION_BYTES,
                }
            })?;
            if total > MAX_GENERATION_BYTES {
                return Err(DeclarativePluginError::GenerationTooLarge {
                    package: installed.manifest().id().clone(),
                    limit: MAX_GENERATION_BYTES,
                });
            }
            let contribution = DeclarativeContribution {
                package_id: installed.manifest().id().clone(),
                package_version: installed.manifest().version().clone(),
                kind: declared.kind(),
                local_id: declared.local_id().to_owned(),
                public_name: declared.public_name().to_owned(),
                path: declared.path().clone(),
                document: Arc::from(document),
            };
            if declared.kind() == ContributionKind::Mcp {
                mcp_contributions.push(contribution);
            } else {
                contributions.push(contribution);
            }
        }

        Ok(Self {
            manifest: installed.manifest().clone(),
            contributions,
            mcp_contributions,
            package_root: Some(installed.package_root().to_path_buf()),
        })
    }

    /// Freeze contribution documents from the exact tree bytes PL02 rehashed.
    ///
    /// The managed path uses this instead of reopening the package by ambient
    /// path after policy evaluation. The documents and the digest PL08
    /// authorized therefore come from one immutable in-memory snapshot.
    pub(crate) fn from_prepared(
        prepared: &PreparedPackage,
    ) -> Result<Self, DeclarativePluginError> {
        let activatable = prepared.manifest.contributions().len();
        if activatable > MAX_CONTRIBUTIONS {
            return Err(DeclarativePluginError::TooManyContributions {
                package: prepared.manifest.id().clone(),
                limit: MAX_CONTRIBUTIONS,
            });
        }

        let mut total = 0_usize;
        let mut contributions = Vec::with_capacity(activatable);
        let mut mcp_contributions = Vec::new();
        for declared in prepared.manifest.contributions() {
            let bytes = prepared
                .entries
                .iter()
                .find(|entry| entry.path == declared.path().as_str())
                .and_then(|entry| match &entry.data {
                    PackageEntryData::File { bytes, .. } => Some(bytes.as_slice()),
                    PackageEntryData::Directory => None,
                })
                .ok_or_else(|| DeclarativePluginError::Document {
                    package: prepared.manifest.id().clone(),
                    kind: declared.kind(),
                    name: declared.public_name().to_owned(),
                    fault: DeclarativeDocumentFault::Unreadable,
                })?;
            if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_DOCUMENT_BYTES) {
                return Err(DeclarativePluginError::Document {
                    package: prepared.manifest.id().clone(),
                    kind: declared.kind(),
                    name: declared.public_name().to_owned(),
                    fault: DeclarativeDocumentFault::TooLarge,
                });
            }
            let document =
                std::str::from_utf8(bytes).map_err(|_| DeclarativePluginError::Document {
                    package: prepared.manifest.id().clone(),
                    kind: declared.kind(),
                    name: declared.public_name().to_owned(),
                    fault: DeclarativeDocumentFault::NotUtf8,
                })?;
            total = total.checked_add(document.len()).ok_or_else(|| {
                DeclarativePluginError::GenerationTooLarge {
                    package: prepared.manifest.id().clone(),
                    limit: MAX_GENERATION_BYTES,
                }
            })?;
            if total > MAX_GENERATION_BYTES {
                return Err(DeclarativePluginError::GenerationTooLarge {
                    package: prepared.manifest.id().clone(),
                    limit: MAX_GENERATION_BYTES,
                });
            }
            let contribution = DeclarativeContribution {
                package_id: prepared.manifest.id().clone(),
                package_version: prepared.manifest.version().clone(),
                kind: declared.kind(),
                local_id: declared.local_id().to_owned(),
                public_name: declared.public_name().to_owned(),
                path: declared.path().clone(),
                document: Arc::from(document),
            };
            if declared.kind() == ContributionKind::Mcp {
                mcp_contributions.push(contribution);
            } else {
                contributions.push(contribution);
            }
        }

        Ok(Self {
            manifest: prepared.manifest.clone(),
            contributions,
            mcp_contributions,
            package_root: None,
        })
    }

    /// Validated package manifest.
    #[must_use]
    pub const fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Ordered PL03 contributions, matching manifest order with MCP removed.
    #[must_use]
    pub fn contributions(&self) -> &[DeclarativeContribution] {
        &self.contributions
    }

    /// Frozen PL04 MCP declarations, in manifest order.
    #[must_use]
    pub fn mcp_contributions(&self) -> &[DeclarativeContribution] {
        &self.mcp_contributions
    }

    /// Verified immutable package root, when this package was loaded from the
    /// installed cache rather than an in-memory managed snapshot.
    #[must_use]
    pub fn package_root(&self) -> Option<&std::path::Path> {
        self.package_root.as_deref()
    }

    /// Number of PL04 declarations retained for the transport-aware host.
    #[must_use]
    pub const fn deferred_mcp_contributions(&self) -> usize {
        self.mcp_contributions.len()
    }
}

impl fmt::Debug for DeclarativePackage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeclarativePackage")
            .field("id", self.manifest.id())
            .field("version", self.manifest.version())
            .field("contributions", &self.contributions)
            .field("mcp_contributions", &self.mcp_contributions)
            .field("has_package_root", &self.package_root.is_some())
            .finish()
    }
}

/// Closed safe failure returned by one domain registry adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HostActivationFailure {
    /// The contribution's primary document is invalid for this registry.
    #[error("invalid declarative definition")]
    InvalidDefinition,
    /// Its exact public name already has a live owner.
    #[error("declarative contribution name is already registered")]
    Duplicate,
    /// A required live registry or policy is unavailable.
    #[error("declarative contribution host is unavailable")]
    Unavailable,
    /// The host deliberately does not implement this definition shape.
    #[error("declarative definition is unsupported by this host")]
    Unsupported,
}

/// One exact PL03 host registration owned by the activation bridge.
///
/// The bridge consumes this value into a [`Context`] effect immediately after
/// a host method succeeds. Taking `self: Box<Self>` makes withdrawal exactly
/// once by construction and prevents a successful host from omitting an
/// explicit disposer from the activation contract.
pub trait DeclarativeContributionRegistration: Send {
    /// Remove the exact registration this value owns.
    fn withdraw(self: Box<Self>);
}

/// Host adapters for all six PL03 contribution registries.
///
/// No activation method has a default: a concrete host cannot compile while
/// silently omitting one PL03 kind. Every successful method returns its exact
/// registration; the bridge attaches it to `context`, so K09 rollback and
/// Context shutdown cannot depend on each adapter remembering that step.
pub trait DeclarativeContributionHost: Send + Sync {
    /// Services every activation method may read from `context`.
    fn required_services(&self) -> &'static [ServiceKey];

    /// Broad descriptor families covering every exact inventory row returned
    /// by [`Self::inventory`].
    fn descriptor_families(&self) -> &'static [PluginContributionKind];

    /// Exact rows this contribution will register, when its target registry has
    /// a core inventory namespace. The default is empty for registries whose
    /// exact core kind has not landed yet; the root integration must not claim
    /// those rows under a false namespace.
    fn inventory(&self, _contribution: &DeclarativeContribution) -> Vec<PluginContributionSpec> {
        Vec::new()
    }

    /// Validate and activate one skill.
    fn activate_skill(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;

    /// Validate and activate one human-only command.
    fn activate_command(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;

    /// Validate and activate one agent preset.
    fn activate_agent(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;

    /// Validate and activate one typed lifecycle hook.
    fn activate_hook(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;

    /// Validate and activate one semantic theme.
    fn activate_theme(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;

    /// Validate and activate one declarative provider route.
    fn activate_provider(
        &self,
        context: &Context,
        contribution: &DeclarativeContribution,
    ) -> Result<Box<dyn DeclarativeContributionRegistration>, HostActivationFailure>;
}

/// Build the aggregate core plugin for one complete active package generation.
///
/// Package identities and `(kind, public-name)` claims are unique before a
/// plugin is returned. During composition, declarations publish first and all
/// six host methods run in package/manifest order inside K09's verified
/// transaction.
///
/// # Errors
/// Duplicate active packages or contribution claims reject the generation
/// before any registry changes.
pub fn declarative_activation_plugin(
    packages: Vec<DeclarativePackage>,
    host: Arc<dyn DeclarativeContributionHost>,
) -> Result<Box<dyn Plugin>, DeclarativePluginError> {
    let mut ids = BTreeSet::new();
    let mut claims = BTreeSet::new();
    for package in &packages {
        if !ids.insert(package.manifest.id().clone()) {
            return Err(DeclarativePluginError::DuplicatePackage {
                package: package.manifest.id().clone(),
            });
        }
        for contribution in &package.contributions {
            if !claims.insert((contribution.kind, contribution.public_name.clone())) {
                return Err(DeclarativePluginError::DuplicateContribution {
                    kind: contribution.kind,
                    name: contribution.public_name.clone(),
                });
            }
        }
    }
    Ok(Box::new(DeclarativeActivationPlugin { packages, host }))
}

struct DeclarativeActivationPlugin {
    packages: Vec<DeclarativePackage>,
    host: Arc<dyn DeclarativeContributionHost>,
}

impl Plugin for DeclarativeActivationPlugin {
    fn name(&self) -> &'static str {
        DECLARATIVE_ACTIVATION_PLUGIN_ID
    }

    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor::built_in(
            DECLARATIVE_ACTIVATION_PLUGIN_ID,
            env!("CARGO_PKG_VERSION"),
            self.host.descriptor_families(),
        )
    }

    fn inject(&self) -> &'static [ServiceKey] {
        self.host.required_services()
    }

    fn inventory(&self) -> Vec<PluginContributionSpec> {
        self.packages
            .iter()
            .flat_map(|package| package.contributions.iter())
            .flat_map(|contribution| self.host.inventory(contribution))
            .collect()
    }

    fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
        for contribution in self
            .packages
            .iter()
            .flat_map(|package| package.contributions.iter())
        {
            let outcome = match contribution.kind {
                ContributionKind::Skill => self.host.activate_skill(context, contribution),
                ContributionKind::Command => self.host.activate_command(context, contribution),
                ContributionKind::Agent => self.host.activate_agent(context, contribution),
                ContributionKind::Hook => self.host.activate_hook(context, contribution),
                ContributionKind::Theme => self.host.activate_theme(context, contribution),
                ContributionKind::Provider => self.host.activate_provider(context, contribution),
                ContributionKind::Mcp => Err(HostActivationFailure::Unsupported),
            };
            let registration = outcome.map_err(|failure| {
                CoreError::other(
                    DeclarativePluginError::Host {
                        package: contribution.package_id.clone(),
                        kind: contribution.kind,
                        name: contribution.public_name.clone(),
                        failure,
                    }
                    .to_string(),
                )
            })?;
            context.effect(move || registration.withdraw());
        }
        Ok(())
    }
}

/// Stable activation/load failures with no document bytes or host paths.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeclarativePluginError {
    /// The host cannot enforce the installed package's owner-only read boundary.
    #[error("declarative plugin package security is unsupported on this host")]
    UnsupportedSecurity,
    /// The installed package root could not be opened as the immutable object
    /// the cache receipt named.
    #[error("installed declarative plugin package is unsafe or changed during activation")]
    UnsafePackage,
    /// One package exceeded the fixed PL03 row budget.
    #[error("plugin `{package}` exceeds the {limit}-contribution activation limit")]
    TooManyContributions {
        /// Validated package id.
        package: PluginId,
        /// Fixed row budget.
        limit: usize,
    },
    /// One primary contribution document was missing, unsafe or malformed.
    #[error("plugin `{package}` {kind} contribution `{name}` document is {fault}")]
    Document {
        /// Validated package id.
        package: PluginId,
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated effective public name.
        name: String,
        /// Closed body-free document fault.
        fault: DeclarativeDocumentFault,
    },
    /// Complete loaded text exceeded the fixed generation memory budget.
    #[error("plugin `{package}` exceeds the {limit}-byte declarative generation limit")]
    GenerationTooLarge {
        /// Validated package id.
        package: PluginId,
        /// Fixed aggregate text budget.
        limit: usize,
    },
    /// The active generation contains the same package id twice.
    #[error("active declarative generation contains plugin `{package}` more than once")]
    DuplicatePackage {
        /// Validated duplicate package id.
        package: PluginId,
    },
    /// Two active packages claim the same exact registry row.
    #[error("active declarative generation contains duplicate {kind} contribution `{name}`")]
    DuplicateContribution {
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated effective public name.
        name: String,
    },
    /// A domain host refused one definition during K09 apply.
    #[error("plugin `{package}` {kind} contribution `{name}` failed activation: {failure}")]
    Host {
        /// Validated package id.
        package: PluginId,
        /// Exact contribution registry.
        kind: ContributionKind,
        /// Validated effective public name.
        name: String,
        /// Closed body-free host failure.
        failure: HostActivationFailure,
    },
}

/// Body-free primary document failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DeclarativeDocumentFault {
    /// The declared file could not be opened and read completely.
    #[error("unreadable")]
    Unreadable,
    /// A symlink, hard link, non-file or changing object was observed.
    #[error("unsafe or changed")]
    Unsafe,
    /// The primary document exceeded its fixed byte budget.
    #[error("too large")]
    TooLarge,
    /// Declarative v1 primary documents are UTF-8 text.
    #[error("not UTF-8")]
    NotUtf8,
}

struct PackageReader {
    #[cfg(unix)]
    root: cap_std::fs::Dir,
}

impl PackageReader {
    fn open(root: &std::path::Path) -> Result<Self, DeclarativePluginError> {
        #[cfg(unix)]
        {
            let root = unix::open_absolute_dir_nofollow(root)
                .map_err(|_| DeclarativePluginError::UnsafePackage)?;
            Ok(Self { root })
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            Err(DeclarativePluginError::UnsupportedSecurity)
        }
    }

    fn read(&self, path: &PluginPath) -> Result<String, DeclarativeDocumentFault> {
        #[cfg(unix)]
        {
            unix::read_document(&self.root, path)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(DeclarativeDocumentFault::Unreadable)
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Read as _};
    use std::path::{Component, Path, PathBuf};

    use cap_fs_ext::{DirExt as _, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt as _};
    use cap_std::ambient_authority;
    use cap_std::fs::{Dir, OpenOptions};
    use cap_std::time::SystemTime;

    use super::{DeclarativeDocumentFault, MAX_DOCUMENT_BYTES, PluginPath};

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FileStamp {
        device: u64,
        inode: u64,
        length: u64,
        modified: Option<SystemTime>,
        links: u64,
        regular: bool,
    }

    impl FileStamp {
        fn of(file: &cap_std::fs::File) -> Result<Self, io::Error> {
            let metadata = file.metadata()?;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                modified: metadata.modified().ok(),
                links: metadata.nlink(),
                regular: metadata.is_file(),
            })
        }
    }

    pub(super) fn open_absolute_dir_nofollow(path: &Path) -> Result<Dir, io::Error> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "package root is not absolute",
            ));
        }
        let mut volume_root = PathBuf::new();
        let mut relative = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => volume_root.push(prefix.as_os_str()),
                Component::RootDir => volume_root.push(component.as_os_str()),
                Component::Normal(name) => relative.push(name),
                Component::CurDir | Component::ParentDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "package root contains traversal",
                    ));
                }
            }
        }
        let root = Dir::open_ambient_dir(&volume_root, ambient_authority())?;
        open_relative_dir(&root, &relative)
    }

    fn open_relative_dir(authority: &Dir, relative: &Path) -> Result<Dir, io::Error> {
        let mut directory = authority.try_clone()?;
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "package path contains traversal",
                ));
            };
            directory = directory.open_dir_nofollow(name)?;
        }
        Ok(directory)
    }

    pub(super) fn read_document(
        root: &Dir,
        path: &PluginPath,
    ) -> Result<String, DeclarativeDocumentFault> {
        let path = Path::new(path.as_str());
        let parent = path.parent().ok_or(DeclarativeDocumentFault::Unreadable)?;
        let name = path
            .file_name()
            .ok_or(DeclarativeDocumentFault::Unreadable)?;
        let directory =
            open_relative_dir(root, parent).map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = directory
            .open_with(name, &options)
            .map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        let before = FileStamp::of(&file).map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        if !before.regular || before.links != 1 {
            return Err(DeclarativeDocumentFault::Unsafe);
        }
        if before.length > MAX_DOCUMENT_BYTES {
            return Err(DeclarativeDocumentFault::TooLarge);
        }
        let capacity =
            usize::try_from(before.length).map_err(|_| DeclarativeDocumentFault::TooLarge)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.by_ref()
            .take(MAX_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        if u64::try_from(bytes.len()).ok() != Some(before.length) {
            return Err(DeclarativeDocumentFault::Unsafe);
        }
        let after = FileStamp::of(&file).map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        let current = directory
            .open_with(name, &options)
            .map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        let current = FileStamp::of(&current).map_err(|_| DeclarativeDocumentFault::Unreadable)?;
        if before != after || before != current {
            return Err(DeclarativeDocumentFault::Unsafe);
        }
        String::from_utf8(bytes).map_err(|_| DeclarativeDocumentFault::NotUtf8)
    }
}
