//! PL11 curated, value-minimized plugin inspection projection.
//!
//! This module does not register a model tool. It defines the QSEC01-
//! independent report shape a later product Consumer may expose through the
//! ordinary durable tool-result path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use thiserror::Error;

use crate::lifecycle::PluginState;
use crate::{ContributionKind, PluginId, PluginManifest, PluginPermission, PluginVersion};

/// Maximum complete rows in one model-facing inspection report.
pub const MAX_CURATED_PLUGIN_ROWS: usize = 256;

/// Maximum UTF-8 bytes in the fixed model-facing rendering.
pub const MAX_CURATED_PLUGIN_REPORT_BYTES: usize = 64 * 1024;

/// Closed implementation class; no executable or host path is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginExecutionKind {
    /// Immutable documents adapted by product registries.
    Declarative,
    /// PL09 native out-of-process protocol.
    NativeProcess,
    /// PL10 Component Model/WIT host.
    WasiComponent,
}

impl PluginExecutionKind {
    /// Stable report label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declarative => "declarative",
            Self::NativeProcess => "native-process",
            Self::WasiComponent => "wasi-component",
        }
    }
}

/// Closed generation health without process/session/error detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginGenerationState {
    /// No live contribution generation.
    Inactive,
    /// Complete generation is currently callable.
    Active,
    /// A prior generation was withdrawn.
    Retired,
    /// The caller has no authoritative runtime observation.
    Unknown,
}

impl PluginGenerationState {
    /// Stable report label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Unknown => "unknown",
        }
    }
}

/// Count for one closed contribution registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginContributionCount {
    /// Exact manifest contribution kind.
    pub kind: ContributionKind,
    /// Positive number of rows of this kind.
    pub count: usize,
}

/// Value-minimized package facts prepared by an activation owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInspectionPackage {
    id: PluginId,
    version: PluginVersion,
    execution: PluginExecutionKind,
    generation: PluginGenerationState,
    contributions: Vec<PluginContributionCount>,
    requested_permissions: Vec<PluginPermission>,
    granted_permissions: Vec<PluginPermission>,
}

impl PluginInspectionPackage {
    /// Copy only closed facts from a validated manifest and effective grants.
    ///
    /// Descriptions, public contribution names, document paths/bodies, source
    /// locators, digests, authentication references and platform paths have no
    /// field in this type.
    ///
    /// # Errors
    /// Duplicate or unrequested grants are refused.
    pub fn new(
        manifest: &PluginManifest,
        execution: PluginExecutionKind,
        generation: PluginGenerationState,
        granted_permissions: impl IntoIterator<Item = PluginPermission>,
    ) -> Result<Self, PluginInspectorError> {
        let mut requested_permissions = manifest.permissions().to_vec();
        requested_permissions.sort_unstable();
        let mut grants = BTreeSet::new();
        for permission in granted_permissions {
            if !requested_permissions.contains(&permission) {
                return Err(PluginInspectorError::UnrequestedGrant(permission));
            }
            if !grants.insert(permission) {
                return Err(PluginInspectorError::DuplicateGrant(permission));
            }
        }
        let mut counts = BTreeMap::<ContributionKind, usize>::new();
        for contribution in manifest.contributions() {
            let count = counts.entry(contribution.kind()).or_default();
            *count = count.saturating_add(1);
        }
        let contributions = counts
            .into_iter()
            .map(|(kind, count)| PluginContributionCount { kind, count })
            .collect();
        Ok(Self {
            id: manifest.id().clone(),
            version: manifest.version().clone(),
            execution,
            generation,
            contributions,
            requested_permissions,
            granted_permissions: grants.into_iter().collect(),
        })
    }
}

/// One joined lifecycle/package row safe for model-facing rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedPluginRow {
    id: PluginId,
    version: PluginVersion,
    enabled: bool,
    rollback_available: bool,
    execution: PluginExecutionKind,
    generation: PluginGenerationState,
    contributions: Vec<PluginContributionCount>,
    requested_permissions: Vec<PluginPermission>,
    granted_permissions: Vec<PluginPermission>,
}

impl CuratedPluginRow {
    /// Validated marketplace/plugin identity.
    #[must_use]
    pub const fn id(&self) -> &PluginId {
        &self.id
    }

    /// Exact active SemVer.
    #[must_use]
    pub const fn version(&self) -> &PluginVersion {
        &self.version
    }

    /// Whether lifecycle state selects the package for composition.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether one previous version exists; its value is intentionally absent.
    #[must_use]
    pub const fn rollback_available(&self) -> bool {
        self.rollback_available
    }

    /// Closed implementation class.
    #[must_use]
    pub const fn execution(&self) -> PluginExecutionKind {
        self.execution
    }

    /// Closed generation health.
    #[must_use]
    pub const fn generation(&self) -> PluginGenerationState {
        self.generation
    }

    /// Counts by closed contribution registry.
    #[must_use]
    pub fn contributions(&self) -> &[PluginContributionCount] {
        &self.contributions
    }

    /// Manifest-requested closed permission names.
    #[must_use]
    pub fn requested_permissions(&self) -> &[PluginPermission] {
        &self.requested_permissions
    }

    /// Effective granted closed permission names.
    #[must_use]
    pub fn granted_permissions(&self) -> &[PluginPermission] {
        &self.granted_permissions
    }
}

/// Complete bounded read-only inspection report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedPluginReport {
    rows: Vec<CuratedPluginRow>,
}

impl CuratedPluginReport {
    /// Sorted complete rows.
    #[must_use]
    pub fn rows(&self) -> &[CuratedPluginRow] {
        &self.rows
    }

    /// Render fixed model-facing text from closed facts only.
    #[must_use]
    pub fn render_model_text(&self) -> String {
        let mut output = format!(
            "plugin-inspection schema=1 count={} read-only; descriptions, bodies, paths, locators, digests, errors, settings and credential references omitted\n",
            self.rows.len()
        );
        for row in &self.rows {
            let _ = write!(
                output,
                "- {} {} {} execution={} generation={} rollback={} contributions=",
                row.id,
                row.version,
                if row.enabled { "enabled" } else { "disabled" },
                row.execution.as_str(),
                row.generation.as_str(),
                if row.rollback_available { "yes" } else { "no" },
            );
            render_counts(&mut output, &row.contributions);
            output.push_str(" requested=");
            render_permissions(&mut output, &row.requested_permissions);
            output.push_str(" granted=");
            render_permissions(&mut output, &row.granted_permissions);
            output.push('\n');
        }
        output
    }
}

/// Pure joiner for lifecycle state and value-minimized package facts.
#[derive(Debug, Default)]
pub struct CuratedPluginInspector;

impl CuratedPluginInspector {
    /// Build one complete, sorted report.
    ///
    /// Every lifecycle row must have one exact-version package view, and no
    /// extra view may be supplied. Missing/colliding/drifted inputs fail rather
    /// than becoming a partial or inferred report.
    ///
    /// # Errors
    /// Bounds, duplicates, missing rows, extra rows or version drift fail loud.
    pub fn build<S, P>(states: S, packages: P) -> Result<CuratedPluginReport, PluginInspectorError>
    where
        S: IntoIterator<Item = PluginState>,
        P: IntoIterator<Item = PluginInspectionPackage>,
    {
        let mut states = states.into_iter().collect::<Vec<_>>();
        let package_rows = packages.into_iter().collect::<Vec<_>>();
        if states.len() > MAX_CURATED_PLUGIN_ROWS || package_rows.len() > MAX_CURATED_PLUGIN_ROWS {
            return Err(PluginInspectorError::TooManyRows);
        }
        let mut package_map = BTreeMap::new();
        for package in package_rows {
            if package_map.insert(package.id.clone(), package).is_some() {
                return Err(PluginInspectorError::DuplicatePackage);
            }
        }
        states.sort_by(|left, right| left.id.cmp(&right.id));
        if states.windows(2).any(|rows| rows[0].id == rows[1].id) {
            return Err(PluginInspectorError::DuplicateState);
        }
        let mut rows = Vec::with_capacity(states.len());
        for state in states {
            let Some(package) = package_map.remove(&state.id) else {
                return Err(PluginInspectorError::MissingPackage);
            };
            if state.active != package.version {
                return Err(PluginInspectorError::VersionMismatch);
            }
            rows.push(CuratedPluginRow {
                id: state.id,
                version: state.active,
                enabled: state.enabled,
                rollback_available: state.previous.is_some(),
                execution: package.execution,
                generation: package.generation,
                contributions: package.contributions,
                requested_permissions: package.requested_permissions,
                granted_permissions: package.granted_permissions,
            });
        }
        if !package_map.is_empty() {
            return Err(PluginInspectorError::UnexpectedPackage);
        }
        let report = CuratedPluginReport { rows };
        if report.render_model_text().len() > MAX_CURATED_PLUGIN_REPORT_BYTES {
            return Err(PluginInspectorError::ReportTooLarge);
        }
        Ok(report)
    }
}

/// Closed failures for curated report construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PluginInspectorError {
    /// Input exceeded the complete report row ceiling.
    #[error("plugin inspection contains too many rows")]
    TooManyRows,
    /// Closed facts still exceeded the rendered byte ceiling.
    #[error("plugin inspection report is too large")]
    ReportTooLarge,
    /// Two package views claimed one id.
    #[error("plugin inspection contains a duplicate package")]
    DuplicatePackage,
    /// Lifecycle state contains one id twice.
    #[error("plugin inspection contains duplicate lifecycle state")]
    DuplicateState,
    /// Lifecycle state has no matching package view.
    #[error("plugin inspection package view is missing")]
    MissingPackage,
    /// A package view has no lifecycle state.
    #[error("plugin inspection contains an unexpected package view")]
    UnexpectedPackage,
    /// Package view and active lifecycle version disagree.
    #[error("plugin inspection package version does not match lifecycle state")]
    VersionMismatch,
    /// Effective grant was not requested by the manifest.
    #[error("plugin inspection contains unrequested grant {0}")]
    UnrequestedGrant(PluginPermission),
    /// Effective grant appeared twice.
    #[error("plugin inspection contains duplicate grant {0}")]
    DuplicateGrant(PluginPermission),
}

fn render_counts(output: &mut String, rows: &[PluginContributionCount]) {
    if rows.is_empty() {
        output.push_str("none");
        return;
    }
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        let _ = write!(output, "{}={}", row.kind.as_str(), row.count);
    }
}

fn render_permissions(output: &mut String, permissions: &[PluginPermission]) {
    if permissions.is_empty() {
        output.push_str("none");
        return;
    }
    for (index, permission) in permissions.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(permission.as_str());
    }
}
