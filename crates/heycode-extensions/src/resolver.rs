//! PL07 deterministic dependency, conflict and platform resolution.
//!
//! Resolution is a pure pre-mutation boundary over already validated manifest
//! values. One explicit [`PlatformTarget`] is carried into the successful
//! graph; no `cfg!` or ambient host inference can turn a manifest validated for
//! another target into current-platform support. Required and present-optional
//! dependencies are version-checked with SemVer precedence, conflicts are
//! symmetric once both packages are selected, and a stable dependency-first
//! order is produced regardless of caller or manifest-list order.
//!
//! The opaque [`ResolvedPluginGraph`] is consumed by the existing cache,
//! lifecycle and declarative-activation boundaries. Their resolved variants
//! verify exact manifests before publication, state persistence or host
//! callbacks. Lower PL01/PL02/PL03 APIs remain available for their own tests and
//! explicitly unmanaged embedding; a product generation must use the resolved
//! path and PL08 managed admission together.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use heycode_core::Plugin;
use thiserror::Error;

use crate::{
    DeclarativeContributionHost, DeclarativePackage, DeclarativePluginError, InstalledPlugin,
    PackageCacheError, PlatformTarget, PluginId, PluginInstallCache, PluginManifest, PluginVersion,
    declarative_activation_plugin,
};

/// Pure resolver configured for one exact current platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginGraphResolver {
    platform: PlatformTarget,
}

impl PluginGraphResolver {
    /// Bind resolution to exact current platform evidence supplied by the host.
    #[must_use]
    pub const fn new(platform: PlatformTarget) -> Self {
        Self { platform }
    }

    /// Resolve one complete active plugin generation.
    ///
    /// The successful order is dependency-first. Ties are plugin-id ordered,
    /// and all diagnostics use the same stable id/dependency ordering, so
    /// caller input order cannot change either success or failure.
    ///
    /// # Errors
    /// Duplicate identities, an unsupported current platform, active
    /// conflicts, missing required dependencies, incompatible present
    /// dependency versions, or cycles.
    pub fn resolve(
        &self,
        manifests: impl IntoIterator<Item = PluginManifest>,
    ) -> Result<ResolvedPluginGraph, PluginResolutionError> {
        let mut grouped = BTreeMap::<PluginId, Vec<PluginManifest>>::new();
        for manifest in manifests {
            grouped
                .entry(manifest.id().clone())
                .or_default()
                .push(manifest);
        }

        for (plugin, group) in &grouped {
            if group.len() > 1 {
                let mut versions = group
                    .iter()
                    .map(|manifest| manifest.version().clone())
                    .collect::<Vec<_>>();
                versions.sort_by(compare_versions);
                return Err(PluginResolutionError::DuplicatePlugin(Box::new(
                    DuplicatePluginDiagnostic {
                        plugin: plugin.clone(),
                        versions,
                    },
                )));
            }
        }

        let mut manifests = BTreeMap::new();
        for (plugin, mut group) in grouped {
            let Some(manifest) = group.pop() else {
                continue;
            };
            manifests.insert(plugin, manifest);
        }

        for manifest in manifests.values() {
            if !manifest.platforms().contains(&self.platform) {
                let mut supported = manifest.platforms().to_vec();
                supported.sort_unstable();
                return Err(PluginResolutionError::UnsupportedPlatform(Box::new(
                    UnsupportedPlatformDiagnostic {
                        plugin: manifest.id().clone(),
                        version: manifest.version().clone(),
                        current: self.platform,
                        supported,
                    },
                )));
            }
        }

        let mut conflicts = BTreeSet::new();
        for manifest in manifests.values() {
            let mut declared = manifest.conflicts().to_vec();
            declared.sort();
            for conflict in declared {
                if manifests.contains_key(&conflict) {
                    conflicts.insert(canonical_pair(manifest.id(), &conflict));
                }
            }
        }
        if let Some((first, second)) = conflicts.into_iter().next() {
            return Err(PluginResolutionError::Conflict { first, second });
        }

        let mut dependencies = BTreeMap::<PluginId, Vec<PluginId>>::new();
        for manifest in manifests.values() {
            let mut declared = manifest.dependencies().iter().collect::<Vec<_>>();
            declared.sort_by(|left, right| left.id.cmp(&right.id));
            let mut present = Vec::new();
            for dependency in declared {
                let Some(found) = manifests.get(&dependency.id) else {
                    if dependency.optional {
                        continue;
                    }
                    return Err(PluginResolutionError::MissingDependency {
                        plugin: manifest.id().clone(),
                        dependency: dependency.id.clone(),
                    });
                };
                let below_minimum =
                    found.version().precedence_cmp(&dependency.minimum_version) == Ordering::Less;
                let reaches_maximum =
                    dependency
                        .maximum_version_exclusive
                        .as_ref()
                        .is_some_and(|maximum| {
                            found.version().precedence_cmp(maximum) != Ordering::Less
                        });
                if below_minimum || reaches_maximum {
                    return Err(PluginResolutionError::IncompatibleDependencyVersion(
                        Box::new(IncompatibleDependencyVersionDiagnostic {
                            plugin: manifest.id().clone(),
                            dependency: dependency.id.clone(),
                            found: found.version().clone(),
                            minimum: dependency.minimum_version.clone(),
                            maximum_exclusive: dependency.maximum_version_exclusive.clone(),
                            optional: dependency.optional,
                        }),
                    ));
                }
                present.push(dependency.id.clone());
            }
            dependencies.insert(manifest.id().clone(), present);
        }

        let ordered_ids = topological_order(&manifests, &dependencies)?;
        let mut ordered = Vec::with_capacity(ordered_ids.len());
        for plugin in ordered_ids {
            let Some(manifest) = manifests.get(&plugin).cloned() else {
                return Err(PluginResolutionError::MissingGenerationPlugin { plugin });
            };
            ordered.push(manifest);
        }
        let by_id = ordered
            .iter()
            .enumerate()
            .map(|(index, manifest)| (manifest.id().clone(), index))
            .collect();
        Ok(ResolvedPluginGraph {
            platform: self.platform,
            ordered,
            by_id,
        })
    }
}

/// One exact dependency-valid, conflict-free, platform-compatible generation.
#[derive(Clone)]
pub struct ResolvedPluginGraph {
    platform: PlatformTarget,
    ordered: Vec<PluginManifest>,
    by_id: BTreeMap<PluginId, usize>,
}

impl ResolvedPluginGraph {
    /// Exact current platform this generation was proven for.
    #[must_use]
    pub const fn platform(&self) -> PlatformTarget {
        self.platform
    }

    /// Complete dependency-first generation.
    #[must_use]
    pub fn manifests(&self) -> &[PluginManifest] {
        &self.ordered
    }

    /// Number of active package identities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// Whether the generation contains no packages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Exact manifest selected for one identity.
    #[must_use]
    pub fn manifest(&self, plugin: &PluginId) -> Option<&PluginManifest> {
        self.by_id
            .get(plugin)
            .and_then(|index| self.ordered.get(*index))
    }

    /// Require a candidate manifest to equal the resolved generation row.
    ///
    /// # Errors
    /// Unexpected identity or any same-id manifest/version/dependency drift.
    pub fn verify_manifest(&self, manifest: &PluginManifest) -> Result<(), PluginResolutionError> {
        let Some(expected) = self.manifest(manifest.id()) else {
            return Err(PluginResolutionError::UnexpectedGenerationPlugin {
                plugin: manifest.id().clone(),
            });
        };
        if expected != manifest {
            return Err(PluginResolutionError::MismatchedGenerationManifest {
                plugin: manifest.id().clone(),
            });
        }
        Ok(())
    }

    pub(crate) fn order_values<T>(
        &self,
        values: Vec<T>,
        manifest_of: impl Fn(&T) -> &PluginManifest,
    ) -> Result<Vec<T>, PluginResolutionError> {
        let mut candidates = BTreeMap::<PluginId, T>::new();
        for value in values {
            let manifest = manifest_of(&value);
            let plugin = manifest.id().clone();
            if candidates.insert(plugin.clone(), value).is_some() {
                return Err(PluginResolutionError::DuplicateGenerationPlugin { plugin });
            }
        }
        for (plugin, value) in &candidates {
            let manifest = manifest_of(value);
            let Some(expected) = self.manifest(plugin) else {
                return Err(PluginResolutionError::UnexpectedGenerationPlugin {
                    plugin: plugin.clone(),
                });
            };
            if expected != manifest {
                return Err(PluginResolutionError::MismatchedGenerationManifest {
                    plugin: plugin.clone(),
                });
            }
        }
        let mut ordered = Vec::with_capacity(self.ordered.len());
        for manifest in &self.ordered {
            let Some(value) = candidates.remove(manifest.id()) else {
                return Err(PluginResolutionError::MissingGenerationPlugin {
                    plugin: manifest.id().clone(),
                });
            };
            ordered.push(value);
        }
        Ok(ordered)
    }
}

impl fmt::Debug for ResolvedPluginGraph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let packages = self
            .ordered
            .iter()
            .map(|manifest| (manifest.id(), manifest.version()))
            .collect::<Vec<_>>();
        formatter
            .debug_struct("ResolvedPluginGraph")
            .field("platform", &self.platform)
            .field("packages", &packages)
            .finish()
    }
}

/// Canonical duplicate-identity diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatePluginDiagnostic {
    /// Duplicated identity.
    pub plugin: PluginId,
    /// Canonically sorted exact versions, including duplicates.
    pub versions: Vec<PluginVersion>,
}

/// Current-platform incompatibility diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedPlatformDiagnostic {
    /// Plugin identity.
    pub plugin: PluginId,
    /// Selected version.
    pub version: PluginVersion,
    /// Exact current platform supplied to the resolver.
    pub current: PlatformTarget,
    /// Manifest-declared supported targets, sorted.
    pub supported: Vec<PlatformTarget>,
}

/// Present dependency version-range diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleDependencyVersionDiagnostic {
    /// Dependent plugin.
    pub plugin: PluginId,
    /// Dependency identity.
    pub dependency: PluginId,
    /// Selected dependency version.
    pub found: PluginVersion,
    /// Inclusive lower bound.
    pub minimum: PluginVersion,
    /// Optional exclusive upper bound.
    pub maximum_exclusive: Option<PluginVersion>,
    /// Whether absence would have been allowed.
    pub optional: bool,
}

/// Stable graph and generation-consumption failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginResolutionError {
    /// More than one manifest selected the same plugin identity.
    DuplicatePlugin(Box<DuplicatePluginDiagnostic>),
    /// One manifest does not support the host-supplied current platform.
    UnsupportedPlatform(Box<UnsupportedPlatformDiagnostic>),
    /// Two selected plugins conflict.
    Conflict {
        /// Lexicographically first identity.
        first: PluginId,
        /// Lexicographically second identity.
        second: PluginId,
    },
    /// Required dependency is absent.
    MissingDependency {
        /// Dependent plugin.
        plugin: PluginId,
        /// Missing dependency.
        dependency: PluginId,
    },
    /// A present required or optional dependency is outside its SemVer range.
    IncompatibleDependencyVersion(Box<IncompatibleDependencyVersionDiagnostic>),
    /// Selected dependencies contain a directed cycle.
    DependencyCycle {
        /// Stable dependency path with the first identity repeated at the end.
        path: Vec<PluginId>,
    },
    /// A cache/activation candidate was not in the resolved generation.
    UnexpectedGenerationPlugin {
        /// Unexpected identity.
        plugin: PluginId,
    },
    /// A resolved generation package was absent from activation input.
    MissingGenerationPlugin {
        /// Missing identity.
        plugin: PluginId,
    },
    /// More than one activation input supplied the same resolved identity.
    DuplicateGenerationPlugin {
        /// Duplicated identity.
        plugin: PluginId,
    },
    /// Same-id candidate metadata differs from the resolved manifest.
    MismatchedGenerationManifest {
        /// Drifted identity.
        plugin: PluginId,
    },
}

impl fmt::Display for PluginResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePlugin(diagnostic) => write!(
                formatter,
                "plugin graph selects `{plugin}` {} times",
                diagnostic.versions.len(),
                plugin = diagnostic.plugin,
            ),
            Self::UnsupportedPlatform(diagnostic) => write!(
                formatter,
                "plugin `{plugin}` version `{version}` does not support current platform {}/{}",
                diagnostic.current.os(),
                diagnostic.current.architecture(),
                plugin = diagnostic.plugin,
                version = diagnostic.version,
            ),
            Self::Conflict { first, second } => {
                write!(formatter, "plugins `{first}` and `{second}` conflict")
            }
            Self::MissingDependency { plugin, dependency } => write!(
                formatter,
                "plugin `{plugin}` requires missing dependency `{dependency}`"
            ),
            Self::IncompatibleDependencyVersion(diagnostic) => {
                write!(
                    formatter,
                    "plugin `{plugin}` requires `{dependency}` >= `{minimum}`",
                    plugin = diagnostic.plugin,
                    dependency = diagnostic.dependency,
                    minimum = diagnostic.minimum,
                )?;
                if let Some(maximum) = &diagnostic.maximum_exclusive {
                    write!(formatter, " and < `{maximum}`")?;
                }
                write!(formatter, "; found `{}`", diagnostic.found)
            }
            Self::DependencyCycle { path } => {
                let rendered = path
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                write!(formatter, "plugin dependency cycle: {rendered}")
            }
            Self::UnexpectedGenerationPlugin { plugin } => {
                write!(
                    formatter,
                    "plugin `{plugin}` is not in the resolved generation"
                )
            }
            Self::MissingGenerationPlugin { plugin } => {
                write!(
                    formatter,
                    "resolved plugin `{plugin}` is missing from the generation"
                )
            }
            Self::DuplicateGenerationPlugin { plugin } => write!(
                formatter,
                "resolved plugin `{plugin}` appears more than once in the generation"
            ),
            Self::MismatchedGenerationManifest { plugin } => write!(
                formatter,
                "plugin `{plugin}` differs from its resolved manifest"
            ),
        }
    }
}

impl std::error::Error for PluginResolutionError {}

/// Cache publication failure after graph resolution.
#[derive(Debug, Error)]
pub enum ResolvedPluginInstallError {
    /// Candidate was absent from or drifted from the resolved generation.
    #[error(transparent)]
    Resolution(Box<PluginResolutionError>),
    /// PL02 source/cache validation or immutable publication failed.
    #[error(transparent)]
    Cache(#[from] PackageCacheError),
}

/// Declarative plugin construction failure after graph resolution.
#[derive(Debug, Error)]
pub enum ResolvedPluginActivationError {
    /// Package set was incomplete, duplicated, unexpected or drifted.
    #[error(transparent)]
    Resolution(Box<PluginResolutionError>),
    /// Existing PL03 package/collision admission failed.
    #[error(transparent)]
    Declarative(Box<DeclarativePluginError>),
}

impl From<PluginResolutionError> for ResolvedPluginInstallError {
    fn from(error: PluginResolutionError) -> Self {
        Self::Resolution(Box::new(error))
    }
}

impl From<PluginResolutionError> for ResolvedPluginActivationError {
    fn from(error: PluginResolutionError) -> Self {
        Self::Resolution(Box::new(error))
    }
}

impl From<DeclarativePluginError> for ResolvedPluginActivationError {
    fn from(error: DeclarativePluginError) -> Self {
        Self::Declarative(Box::new(error))
    }
}

impl PluginInstallCache {
    /// Install one source only when its complete manifest equals a resolved row.
    ///
    /// The source tree is frozen before comparison, and those exact bytes enter
    /// PL02 commit. Resolution refusal therefore creates no install lock,
    /// staging directory, content object or id/version reference.
    ///
    /// This is the lower PL02 graph seam. Product marketplace installation
    /// additionally uses PL08 managed policy; it must not substitute this
    /// method for managed admission.
    ///
    /// # Errors
    /// Source/cache failure or generation absence/drift before publication.
    pub fn install_resolved_directory(
        &self,
        graph: &ResolvedPluginGraph,
        source: impl AsRef<Path>,
    ) -> Result<InstalledPlugin, ResolvedPluginInstallError> {
        let prepared = self.prepare_directory(source.as_ref())?;
        graph.verify_manifest(&prepared.manifest)?;
        Ok(self.commit_prepared(prepared)?)
    }
}

/// Construct PL03 activation only from one exact complete resolved generation.
///
/// Inputs are reordered into dependency-first order before the existing PL03
/// transaction is built. Any missing, duplicate, unexpected or drifted package
/// fails before the first domain-host callback.
///
/// # Errors
/// Generation mismatch or existing PL03 package/contribution collision.
pub fn resolved_declarative_activation_plugin(
    graph: &ResolvedPluginGraph,
    packages: Vec<DeclarativePackage>,
    host: Arc<dyn DeclarativeContributionHost>,
) -> Result<Box<dyn Plugin>, ResolvedPluginActivationError> {
    let ordered = graph.order_values(packages, |package| package.manifest())?;
    Ok(declarative_activation_plugin(ordered, host)?)
}

fn topological_order(
    manifests: &BTreeMap<PluginId, PluginManifest>,
    dependencies: &BTreeMap<PluginId, Vec<PluginId>>,
) -> Result<Vec<PluginId>, PluginResolutionError> {
    let mut indegree = manifests
        .keys()
        .map(|plugin| (plugin.clone(), dependencies.get(plugin).map_or(0, Vec::len)))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = manifests
        .keys()
        .map(|plugin| (plugin.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for (plugin, required) in dependencies {
        for dependency in required {
            if let Some(rows) = dependents.get_mut(dependency) {
                rows.insert(plugin.clone());
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(plugin, count)| (*count == 0).then_some(plugin.clone()))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(manifests.len());
    while let Some(plugin) = ready.pop_first() {
        ordered.push(plugin.clone());
        if let Some(rows) = dependents.get(&plugin) {
            for dependent in rows {
                let Some(count) = indegree.get_mut(dependent) else {
                    continue;
                };
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(dependent.clone());
                }
            }
        }
    }
    if ordered.len() == manifests.len() {
        return Ok(ordered);
    }
    Err(PluginResolutionError::DependencyCycle {
        path: extract_cycle(&indegree, dependencies),
    })
}

fn extract_cycle(
    indegree: &BTreeMap<PluginId, usize>,
    dependencies: &BTreeMap<PluginId, Vec<PluginId>>,
) -> Vec<PluginId> {
    let remaining = indegree
        .iter()
        .filter_map(|(plugin, count)| (*count > 0).then_some(plugin.clone()))
        .collect::<BTreeSet<_>>();
    let Some(mut current) = remaining.first().cloned() else {
        return Vec::new();
    };
    let mut path = Vec::new();
    let mut positions = BTreeMap::new();
    loop {
        if let Some(start) = positions.get(&current).copied() {
            let mut cycle = path[start..].to_vec();
            cycle.push(current);
            return canonical_cycle(cycle);
        }
        positions.insert(current.clone(), path.len());
        path.push(current.clone());
        let next = dependencies
            .get(&current)
            .into_iter()
            .flatten()
            .filter(|dependency| remaining.contains(*dependency))
            .min()
            .cloned();
        let Some(next) = next else {
            return vec![current.clone(), current];
        };
        current = next;
    }
}

fn canonical_cycle(mut cycle: Vec<PluginId>) -> Vec<PluginId> {
    let _ = cycle.pop();
    let Some((start, _)) = cycle.iter().enumerate().min_by_key(|(_, plugin)| *plugin) else {
        return Vec::new();
    };
    let mut canonical = cycle[start..]
        .iter()
        .chain(cycle[..start].iter())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(first) = canonical.first().cloned() {
        canonical.push(first);
    }
    canonical
}

fn canonical_pair(left: &PluginId, right: &PluginId) -> (PluginId, PluginId) {
    if left <= right {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    }
}

fn compare_versions(left: &PluginVersion, right: &PluginVersion) -> Ordering {
    left.precedence_cmp(right)
        .then_with(|| left.as_str().cmp(right.as_str()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use std::mem::size_of;

    use super::{PluginResolutionError, ResolvedPluginActivationError, ResolvedPluginInstallError};

    #[test]
    fn public_operation_errors_remain_small_result_payloads() {
        const MAX_ERROR_BYTES: usize = 128;
        assert!(size_of::<PluginResolutionError>() <= MAX_ERROR_BYTES);
        assert!(size_of::<ResolvedPluginInstallError>() <= MAX_ERROR_BYTES);
        assert!(size_of::<ResolvedPluginActivationError>() <= MAX_ERROR_BYTES);
        assert!(size_of::<crate::ManagedPluginError>() <= MAX_ERROR_BYTES);
    }
}
