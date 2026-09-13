//! U13 plugin panel: one terminal surface over the PL06 lifecycle.
//!
//! The panel owns no lifecycle logic. Every action it offers is one of the six
//! [`PluginOperation`] variants and is executed by [`PluginLifecycle`], which
//! implements each exactly once. `PluginOperation` is deliberately not
//! `#[non_exhaustive]`, so the matches in [`build_actions`] and
//! [`PluginPanelIntent::operation`] stop compiling the moment a seventh
//! operation is added.
//!
//! `list` is deliberately **not** in that set — a read is not a lifecycle
//! transition — so the panel's refresh is a query, not an action, and the
//! exhaustive matches below cover exactly six.
//!
//! Provenance and permissions are not lifecycle state: they come from PL01's
//! manifest through PL02's cache. The panel takes them as an already-projected
//! [`PluginPackageIndex`], so it stays a pure function of its inputs and the
//! filesystem work belongs to whoever composed the cache.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyModifiers};

use heycode_extensions::lifecycle::{
    LifecycleError, PluginLifecycle, PluginOperation, PluginState,
};
use heycode_extensions::{
    AuthenticationPolicy, PluginId, PluginManifest, PluginPermission, PluginSourceKind,
    PluginVersion, UpdateChannel,
};

#[path = "plugin_panel_render.rs"]
pub mod render;

/// The four capabilities the acceptance criterion names, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PluginPanelSection {
    /// Where the package came from and what verifies it.
    Provenance,
    /// What the manifest asks the host to grant.
    Permissions,
    /// Whether the plugin participates in composition.
    Enable,
    /// Active version, rollback target and what the cache holds.
    Update,
}

impl PluginPanelSection {
    /// Every section, in the order the acceptance criterion names them.
    pub const ALL: [Self; 4] = [
        Self::Provenance,
        Self::Permissions,
        Self::Enable,
        Self::Update,
    ];

    /// Stable tab label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Provenance => "provenance",
            Self::Permissions => "permissions",
            Self::Enable => "enable",
            Self::Update => "update",
        }
    }

    /// Next section in tab order, wrapping.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Provenance => Self::Permissions,
            Self::Permissions => Self::Enable,
            Self::Enable => Self::Update,
            Self::Update => Self::Provenance,
        }
    }

    /// Previous section in tab order, wrapping.
    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Provenance => Self::Update,
            Self::Permissions => Self::Provenance,
            Self::Enable => Self::Permissions,
            Self::Update => Self::Enable,
        }
    }
}

/// Whether a section states live facts, and if not, why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSectionState {
    /// The lines below are facts read from the package or its state.
    Live,
    /// The package declares nothing of this kind, and we verified that.
    NotDeclared,
    /// Nothing was looked up, so nothing is claimed.
    NoEvidence,
}

/// One section's availability plus its safe rendered lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSectionView {
    /// Which capability this describes.
    pub section: PluginPanelSection,
    /// Whether the lines are facts or an explicit absence.
    pub state: PluginSectionState,
    /// Safe display lines; never empty.
    pub lines: Vec<String>,
}

/// Where a package came from, projected from PL01's manifest.
///
/// The locator is rendered whole. PL01's validator rejects `?`, `#` and any
/// `@` in a URL authority before a manifest is ever admitted, so a locator
/// cannot carry a query string or embedded credentials. Re-eliding here would
/// duplicate a boundary the layer below already owns, and a redundant check is
/// a check no test can distinguish (GOTCHAS #160).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProvenance {
    /// Origin kind the package asserts.
    pub kind: &'static str,
    /// Validated, credential-free source locator.
    pub locator: String,
    /// Immutable or named revision, when declared.
    pub revision: Option<String>,
    /// Declared content checksum, when present.
    pub checksum: Option<String>,
    /// Whether a detached signature is attached.
    pub signed: bool,
    /// Requested update channel.
    pub update_channel: &'static str,
}

/// What a package asks the host to grant, projected from PL01's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPermissionRequest {
    /// Exactly the permissions the manifest requests, in manifest order.
    ///
    /// An empty vector means the manifest requests none — which is a different
    /// statement from the absent [`PluginPackageFacts::permissions`], and the
    /// difference runs in the dangerous direction: rendering "none" for a
    /// package that was never inspected understates what it may do.
    pub permissions: Vec<PluginPermission>,
    /// Whether the package requires, may use, or declares no credentials.
    pub authentication: &'static str,
    /// Non-secret credential references the package binds.
    pub credential_references: Vec<String>,
}

/// Package-cache facts for one exact id/version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPackageFacts {
    /// Verified canonical tree hash.
    pub content_hash: String,
    /// Number of contribution declarations in the manifest.
    pub contribution_count: usize,
    /// Whether the manifest asks to be enabled by default.
    pub default_enabled: bool,
    /// Manifest-derived origin, absent when no manifest was resolved.
    pub provenance: Option<PluginProvenance>,
    /// Manifest-derived permission request, absent when no manifest was
    /// resolved. Absent never means "requests nothing".
    pub permissions: Option<PluginPermissionRequest>,
}

/// Already-projected package-cache facts for the rows on screen.
///
/// Built by whoever owns the cache, so the panel performs no filesystem work
/// and stays testable without one. A caller that ran only `inspect()` supplies
/// summaries; a caller that also resolved manifests supplies provenance and
/// permissions too, and the panel reports honestly on whichever it received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginPackageIndex {
    facts: BTreeMap<(String, String), PluginPackageFacts>,
    versions: BTreeMap<String, Vec<PluginVersion>>,
}

impl PluginPackageIndex {
    /// An index holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one verified package the cache holds.
    ///
    /// Summary level, mapped by the caller from one PL02 `CachedPluginSummary`
    /// row (`id`, `version`, `content_hash.as_str()`, `contribution_count`,
    /// `default_enabled`). Taking the projected fields rather than the cache
    /// type keeps this crate off PL02's receipt struct and lets the projection
    /// be exercised without a filesystem.
    ///
    /// Provenance and permissions stay absent until a manifest is resolved.
    pub fn add_package(
        &mut self,
        id: &PluginId,
        version: &PluginVersion,
        content_hash: impl Into<String>,
        contribution_count: usize,
        default_enabled: bool,
    ) {
        self.versions
            .entry(id.as_str().to_owned())
            .or_default()
            .push(version.clone());
        self.facts.insert(
            (id.as_str().to_owned(), version.as_str().to_owned()),
            PluginPackageFacts {
                content_hash: content_hash.into(),
                contribution_count,
                default_enabled,
                provenance: None,
                permissions: None,
            },
        );
    }

    /// Attach manifest-derived provenance and permissions for one package.
    ///
    /// A manifest for a version the index has no summary for is ignored: the
    /// cache's verified listing is the authority on what is installed.
    pub fn add_manifest(&mut self, manifest: &PluginManifest) {
        let key = (
            manifest.id().as_str().to_owned(),
            manifest.version().as_str().to_owned(),
        );
        let Some(facts) = self.facts.get_mut(&key) else {
            return;
        };
        let source = manifest.source();
        facts.provenance = Some(PluginProvenance {
            kind: source_kind_word(source.kind),
            locator: source.locator.clone(),
            revision: source.revision.clone(),
            checksum: source.checksum.clone(),
            signed: source.signature.is_some(),
            update_channel: update_channel_word(source.update_channel),
        });
        let authentication = manifest.authentication();
        facts.permissions = Some(PluginPermissionRequest {
            permissions: manifest.permissions().to_vec(),
            authentication: authentication_word(authentication.policy),
            credential_references: authentication
                .credential_references
                .iter()
                .map(|requirement| format!("{} ({})", requirement.reference, requirement.kind))
                .collect(),
        });
    }

    /// Versions the cache holds for `id`.
    #[must_use]
    pub fn versions(&self, id: &str) -> &[PluginVersion] {
        self.versions.get(id).map_or(&[], Vec::as_slice)
    }

    /// Facts for one exact id/version.
    #[must_use]
    pub fn facts(&self, id: &str, version: &str) -> Option<&PluginPackageFacts> {
        self.facts.get(&(id.to_owned(), version.to_owned()))
    }
}

/// Whether `rollback` can run now, and why not when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackState {
    /// A remembered version is still in the cache and can be returned to.
    Available(PluginVersion),
    /// No update has happened, so there is nothing to return to.
    NoPreviousVersion,
    /// A version is remembered but the cache no longer holds it.
    ///
    /// `PluginLifecycle::rollback` refuses this case. The panel computes it up
    /// front so the operator is not told only after choosing.
    PreviousVersionPruned(PluginVersion),
    /// No package cache was supplied, so retention could not be checked.
    CacheUnknown(PluginVersion),
}

impl RollbackState {
    /// Whether the panel may offer rollback.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

/// One projected plugin row: lifecycle state plus what the package says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPanelRow {
    /// Validated plugin identity.
    pub id: PluginId,
    /// The version that activates.
    pub active: PluginVersion,
    /// Whether the plugin participates in composition.
    pub enabled: bool,
    /// Whether and how `rollback` can run.
    pub rollback: RollbackState,
    /// Every version the cache holds, newest precedence first.
    pub cached_versions: Vec<PluginVersion>,
    /// Package facts for the active version, absent when nothing was looked up.
    pub facts: Option<PluginPackageFacts>,
    /// Whether a package cache was supplied at all.
    pub cache_consulted: bool,
}

/// One offered lifecycle action, derived from the closed operation set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPanelAction {
    /// Which of the six operations this action runs.
    pub operation: PluginOperation,
    /// Human label; `enable`/`disable` read as their effect.
    pub label: &'static str,
    /// Present when the panel must not offer this action.
    pub unavailable_reason: Option<String>,
}

impl PluginPanelAction {
    /// Whether this action can be run now.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.unavailable_reason.is_none()
    }
}

/// A fully specified panel intent, ready for [`dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPanelIntent {
    /// Record a newly cached version as active and enabled.
    Install {
        /// Plugin identity.
        id: PluginId,
        /// Version to activate.
        version: PluginVersion,
    },
    /// Turn one plugin on or off without touching versions.
    SetEnabled {
        /// Plugin identity.
        id: PluginId,
        /// Whether to activate.
        enabled: bool,
    },
    /// Move to another cached version, remembering the current one.
    Update {
        /// Plugin identity.
        id: PluginId,
        /// Version to move to.
        version: PluginVersion,
    },
    /// Return to the remembered previous version.
    Rollback {
        /// Plugin identity.
        id: PluginId,
    },
    /// Forget a plugin entirely.
    Remove {
        /// Plugin identity.
        id: PluginId,
    },
}

impl PluginPanelIntent {
    /// Which operation this intent invokes.
    ///
    /// `enable` and `disable` share one intent carrying a flag, so this is
    /// where the two words rejoin the closed operation set — the same seam the
    /// CLI uses.
    #[must_use]
    pub const fn operation(&self) -> PluginOperation {
        match self {
            Self::Install { .. } => PluginOperation::Install,
            Self::SetEnabled { enabled: true, .. } => PluginOperation::Enable,
            Self::SetEnabled { enabled: false, .. } => PluginOperation::Disable,
            Self::Update { .. } => PluginOperation::Update,
            Self::Rollback { .. } => PluginOperation::Rollback,
            Self::Remove { .. } => PluginOperation::Remove,
        }
    }
}

/// What one dispatched operation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPanelOutcome {
    /// The lifecycle state was re-read. `list` is a query, not an operation.
    Listed(Vec<PluginState>),
    /// A rollback committed and returned the version now active.
    RolledBack {
        /// Plugin identity.
        id: String,
        /// The version now active.
        restored: PluginVersion,
    },
    /// A mutating operation committed. The panel re-lists before showing rows,
    /// so no row is published from an operation that might still roll back.
    Committed(String),
}

/// What one key press asked the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginPanelKeyOutcome {
    /// The view updated itself; nothing else to do.
    Handled,
    /// Run this intent against the lifecycle layer.
    Run(PluginPanelIntent),
    /// Close the panel.
    Close,
}

/// Semantic emphasis for one rendered line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginPanelTone {
    /// Section or column heading.
    Heading,
    /// Ordinary content.
    Body,
    /// Secondary detail and key hints.
    Dim,
    /// A confirmed-good fact.
    Good,
    /// An absence, an unknown, or a state needing operator attention.
    Warn,
    /// A failure.
    Bad,
    /// The row or action under the cursor.
    Selected,
}

/// One rendered panel line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPanelLine {
    /// Safe display text.
    pub text: String,
    /// Semantic emphasis.
    pub tone: PluginPanelTone,
}

impl PluginPanelLine {
    fn new(tone: PluginPanelTone, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

const NO_SELECTION: &str = "select a plugin first";

/// Plugin rows kept on the card at once; the rest are windowed, not clipped.
const VISIBLE_PLUGIN_ROWS: usize = 8;

const fn source_kind_word(kind: PluginSourceKind) -> &'static str {
    match kind {
        PluginSourceKind::Local => "local directory",
        PluginSourceKind::Git => "git",
        PluginSourceKind::Https => "https archive",
        PluginSourceKind::Registry => "registry",
        PluginSourceKind::Marketplace => "marketplace",
    }
}

const fn update_channel_word(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Stable => "stable",
        UpdateChannel::Preview => "preview",
        UpdateChannel::Pinned => "pinned",
    }
}

const fn authentication_word(policy: AuthenticationPolicy) -> &'static str {
    match policy {
        AuthenticationPolicy::None => "no credentials declared",
        AuthenticationPolicy::Optional => "credentials optional",
        AuthenticationPolicy::Required => "credentials required",
    }
}

/// Project lifecycle state and package facts into panel rows.
///
/// `packages` is absent in a world that manages lifecycle without a readable
/// cache; a row then carries no package facts rather than implying the package
/// declares nothing.
#[must_use]
pub fn build_rows(
    states: &[PluginState],
    packages: Option<&PluginPackageIndex>,
) -> Vec<PluginPanelRow> {
    states
        .iter()
        .map(|state| {
            let id = state.id.as_str();
            let mut cached_versions = packages
                .map(|index| index.versions(id).to_vec())
                .unwrap_or_default();
            cached_versions.sort_by(|left, right| {
                right
                    .precedence_cmp(left)
                    .then_with(|| right.as_str().cmp(left.as_str()))
            });
            let rollback = match (state.previous.as_ref(), packages) {
                (None, _) => RollbackState::NoPreviousVersion,
                (Some(previous), None) => RollbackState::CacheUnknown(previous.clone()),
                (Some(previous), Some(_)) if cached_versions.contains(previous) => {
                    RollbackState::Available(previous.clone())
                }
                (Some(previous), Some(_)) => RollbackState::PreviousVersionPruned(previous.clone()),
            };
            PluginPanelRow {
                id: state.id.clone(),
                active: state.active.clone(),
                enabled: state.enabled,
                rollback,
                facts: packages.and_then(|index| index.facts(id, state.active.as_str()).cloned()),
                cache_consulted: packages.is_some(),
                cached_versions,
            }
        })
        .collect()
}

/// Build the action list for one selection.
///
/// The match on [`PluginOperation`] carries no wildcard arm: a seventh
/// operation fails to compile here until this panel handles it. `list` is
/// absent by design — it is a query, and the refresh affordance is not an
/// action.
#[must_use]
pub fn build_actions(row: Option<&PluginPanelRow>) -> Vec<PluginPanelAction> {
    PluginOperation::ALL
        .into_iter()
        .map(|operation| {
            let (label, unavailable_reason) = match operation {
                // `install` adds a package the panel does not yet list, so a
                // selection is irrelevant to it.
                PluginOperation::Install => ("Install…", None),
                PluginOperation::Enable => (
                    "Enable",
                    match row {
                        None => Some(NO_SELECTION.to_owned()),
                        Some(row) if row.enabled => {
                            Some(format!("`{}` is already enabled", row.id.as_str()))
                        }
                        Some(_) => None,
                    },
                ),
                PluginOperation::Disable => (
                    "Disable",
                    match row {
                        None => Some(NO_SELECTION.to_owned()),
                        Some(row) if row.enabled => None,
                        Some(row) => Some(format!("`{}` is already disabled", row.id.as_str())),
                    },
                ),
                PluginOperation::Update => (
                    "Update…",
                    row.map_or(Some(NO_SELECTION.to_owned()), |_| None),
                ),
                PluginOperation::Rollback => ("Roll back", rollback_refusal(row)),
                PluginOperation::Remove => (
                    "Remove",
                    row.map_or(Some(NO_SELECTION.to_owned()), |_| None),
                ),
            };
            PluginPanelAction {
                operation,
                label,
                unavailable_reason,
            }
        })
        .collect()
}

/// Why rollback is not offered for this selection.
///
/// `PluginLifecycle::rollback` refuses a pruned target; stating it here means
/// the operator learns before choosing rather than after being denied.
fn rollback_refusal(row: Option<&PluginPanelRow>) -> Option<String> {
    let Some(row) = row else {
        return Some(NO_SELECTION.to_owned());
    };
    match &row.rollback {
        RollbackState::Available(_) => None,
        RollbackState::NoPreviousVersion => {
            Some(format!("`{}` has no previous version", row.id.as_str()))
        }
        RollbackState::PreviousVersionPruned(version) => Some(format!(
            "version {version} is no longer in the package cache"
        )),
        RollbackState::CacheUnknown(version) => Some(format!(
            "no package cache to confirm version {version} is still installed"
        )),
    }
}

/// Render one section for one selection.
#[must_use]
pub fn section_view(
    section: PluginPanelSection,
    row: Option<&PluginPanelRow>,
) -> PluginSectionView {
    let Some(row) = row else {
        return PluginSectionView {
            section,
            state: PluginSectionState::NoEvidence,
            lines: vec![format!(
                "select a plugin to inspect its {}",
                section.title()
            )],
        };
    };
    match section {
        PluginPanelSection::Provenance => provenance_section(row),
        PluginPanelSection::Permissions => permissions_section(row),
        PluginPanelSection::Enable => enable_section(row),
        PluginPanelSection::Update => update_section(row),
    }
}

fn provenance_section(row: &PluginPanelRow) -> PluginSectionView {
    let Some(facts) = row.facts.as_ref() else {
        return PluginSectionView {
            section: PluginPanelSection::Provenance,
            state: PluginSectionState::NoEvidence,
            lines: vec![
                "no package cache evidence for the active version".to_owned(),
                "origin cannot be shown without the installed manifest".to_owned(),
            ],
        };
    };
    let mut lines = vec![
        format!("content: {}", facts.content_hash),
        format!("contributions declared: {}", facts.contribution_count),
    ];
    let Some(provenance) = facts.provenance.as_ref() else {
        return PluginSectionView {
            section: PluginPanelSection::Provenance,
            state: PluginSectionState::NoEvidence,
            lines: {
                lines.push("manifest not resolved, so the origin is unknown".to_owned());
                lines
            },
        };
    };
    lines.push(format!(
        "origin: {} {}",
        provenance.kind, provenance.locator
    ));
    lines.push(format!("update channel: {}", provenance.update_channel));
    lines.push(match provenance.revision.as_deref() {
        Some(revision) => format!("revision: {revision}"),
        None => "revision: none declared".to_owned(),
    });
    lines.push(match provenance.checksum.as_deref() {
        Some(checksum) => format!("checksum: {checksum}"),
        None => "checksum: none declared".to_owned(),
    });
    lines.push(if provenance.signed {
        "signature: attached (PL05 owns verification)".to_owned()
    } else {
        "signature: none attached".to_owned()
    });
    PluginSectionView {
        section: PluginPanelSection::Provenance,
        state: PluginSectionState::Live,
        lines,
    }
}

fn permissions_section(row: &PluginPanelRow) -> PluginSectionView {
    // The dangerous direction: an unresolved manifest must never render as an
    // empty permission list. "Requests nothing" and "we did not look" differ,
    // and only one of them is safe to believe.
    let Some(request) = row
        .facts
        .as_ref()
        .and_then(|facts| facts.permissions.as_ref())
    else {
        return PluginSectionView {
            section: PluginPanelSection::Permissions,
            state: PluginSectionState::NoEvidence,
            lines: vec![
                "requested permissions are unknown for this build".to_owned(),
                "the installed manifest was not resolved — this is not the same as none".to_owned(),
            ],
        };
    };
    if request.permissions.is_empty() {
        let mut lines = vec!["the manifest requests no host permissions".to_owned()];
        lines.push(request.authentication.to_owned());
        return PluginSectionView {
            section: PluginPanelSection::Permissions,
            state: PluginSectionState::NotDeclared,
            lines,
        };
    }
    let mut lines = request
        .permissions
        .iter()
        .map(|permission| format!("requests {}", permission.as_str()))
        .collect::<Vec<_>>();
    lines.push(request.authentication.to_owned());
    for reference in &request.credential_references {
        lines.push(format!("credential reference: {reference}"));
    }
    PluginSectionView {
        section: PluginPanelSection::Permissions,
        state: PluginSectionState::Live,
        lines,
    }
}

fn enable_section(row: &PluginPanelRow) -> PluginSectionView {
    let mut lines = vec![format!(
        "state: {}",
        if row.enabled { "enabled" } else { "disabled" }
    )];
    match row.facts.as_ref() {
        // The manifest's request and the host's decision are different facts,
        // and a panel that showed only one could not explain the other.
        Some(facts) => lines.push(format!(
            "manifest asks to be {} by default",
            if facts.default_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )),
        None => lines.push("manifest default-enable request is unknown".to_owned()),
    }
    PluginSectionView {
        section: PluginPanelSection::Enable,
        state: PluginSectionState::Live,
        lines,
    }
}

fn update_section(row: &PluginPanelRow) -> PluginSectionView {
    let mut lines = vec![format!("active: {}", row.active)];
    lines.push(match &row.rollback {
        RollbackState::Available(version) => format!("rollback target: {version}"),
        RollbackState::NoPreviousVersion => {
            "rollback target: none — no update has been applied".to_owned()
        }
        RollbackState::PreviousVersionPruned(version) => {
            format!("rollback target: {version} is no longer in the cache")
        }
        RollbackState::CacheUnknown(version) => {
            format!("rollback target: {version}, retention unverified")
        }
    });
    if row.cache_consulted {
        lines.push(if row.cached_versions.is_empty() {
            "the package cache holds no version of this plugin".to_owned()
        } else {
            format!(
                "cache holds: {}",
                row.cached_versions
                    .iter()
                    .map(PluginVersion::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
    } else {
        lines.push("no package cache was consulted".to_owned());
    }
    PluginSectionView {
        section: PluginPanelSection::Update,
        state: if row.cache_consulted {
            PluginSectionState::Live
        } else {
            PluginSectionState::NoEvidence
        },
        lines,
    }
}

/// Run one intent against the shared PL06 lifecycle layer.
///
/// Nothing is reimplemented here: every arm delegates to [`PluginLifecycle`],
/// which owns the single implementation of each operation.
///
/// # Errors
/// The lifecycle error rendered for a terminal. The text names plugins and
/// versions only.
pub fn dispatch(
    lifecycle: &PluginLifecycle,
    intent: &PluginPanelIntent,
) -> Result<PluginPanelOutcome, String> {
    match intent {
        PluginPanelIntent::Install { id, version } => {
            lifecycle.install(id, version).map_err(render_error)?;
            Ok(PluginPanelOutcome::Committed(format!(
                "installed `{}` {version}",
                id.as_str()
            )))
        }
        PluginPanelIntent::SetEnabled { id, enabled } => {
            lifecycle.set_enabled(id, *enabled).map_err(render_error)?;
            Ok(PluginPanelOutcome::Committed(format!(
                "{} `{}`",
                if *enabled { "enabled" } else { "disabled" },
                id.as_str()
            )))
        }
        PluginPanelIntent::Update { id, version } => {
            lifecycle.update(id, version).map_err(render_error)?;
            Ok(PluginPanelOutcome::Committed(format!(
                "updated `{}` to {version}",
                id.as_str()
            )))
        }
        PluginPanelIntent::Rollback { id } => {
            let restored = lifecycle.rollback(id).map_err(render_error)?;
            Ok(PluginPanelOutcome::RolledBack {
                id: id.as_str().to_owned(),
                restored,
            })
        }
        PluginPanelIntent::Remove { id } => {
            lifecycle.remove(id).map_err(render_error)?;
            Ok(PluginPanelOutcome::Committed(format!(
                "removed `{}`",
                id.as_str()
            )))
        }
    }
}

/// Re-read lifecycle state. A query, not one of the six operations.
///
/// # Errors
/// The lifecycle error rendered for a terminal.
pub fn list(lifecycle: &PluginLifecycle) -> Result<PluginPanelOutcome, String> {
    lifecycle
        .list()
        .map(PluginPanelOutcome::Listed)
        .map_err(render_error)
}

fn render_error(error: LifecycleError) -> String {
    error.to_string()
}

/// Which list the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginPanelFocus {
    /// Claude-style Installed list.
    Plugins,
    /// Selected-plugin details and actions.
    Actions,
}

/// Which field of an install/update form the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginFormField {
    Id,
    Version,
}

struct PluginPanelForm {
    operation: PluginOperation,
    id: String,
    version: String,
    field: PluginFormField,
}

struct PluginPanelConfirm {
    id: PluginId,
    remove: bool,
}

struct PluginPanelNotice {
    text: String,
    ok: bool,
}

/// Panel state: selection, section, in-flight form and last notice.
pub struct PluginPanelView {
    rows: Vec<PluginPanelRow>,
    selected: usize,
    section: PluginPanelSection,
    focus: PluginPanelFocus,
    action_selected: usize,
    section_offset: usize,
    query: String,
    show_details: bool,
    form: Option<PluginPanelForm>,
    confirm: Option<PluginPanelConfirm>,
    notice: Option<PluginPanelNotice>,
}

impl PluginPanelView {
    /// Open the panel over one projection.
    #[must_use]
    pub fn new(rows: Vec<PluginPanelRow>) -> Self {
        Self {
            rows,
            selected: 0,
            section: PluginPanelSection::Provenance,
            focus: PluginPanelFocus::Plugins,
            action_selected: 0,
            section_offset: 0,
            query: String::new(),
            show_details: false,
            form: None,
            confirm: None,
            notice: None,
        }
    }

    /// Replace the rows, keeping the cursor on the same plugin when it survives.
    pub fn set_rows(&mut self, rows: Vec<PluginPanelRow>) {
        let previous = self.selected_row().map(|row| row.id.as_str().to_owned());
        self.rows = rows;
        self.selected = previous
            .and_then(|id| self.rows.iter().position(|row| row.id.as_str() == id))
            .unwrap_or(0);
        self.section_offset = 0;
        self.clamp_action();
    }

    /// Record a successful operation.
    pub fn note_success(&mut self, text: impl Into<String>) {
        self.notice = Some(PluginPanelNotice {
            text: text.into(),
            ok: true,
        });
    }

    /// Record a failed operation.
    pub fn note_failure(&mut self, text: impl Into<String>) {
        self.notice = Some(PluginPanelNotice {
            text: text.into(),
            ok: false,
        });
    }

    /// Projected rows.
    #[must_use]
    pub fn rows(&self) -> &[PluginPanelRow] {
        &self.rows
    }

    /// Highlighted plugin row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Highlighted plugin row.
    #[must_use]
    pub fn selected_row(&self) -> Option<&PluginPanelRow> {
        self.rows.get(self.selected)
    }

    /// Active capability section.
    #[must_use]
    pub const fn section(&self) -> PluginPanelSection {
        self.section
    }

    /// Actions offered for the current selection.
    #[must_use]
    pub fn actions(&self) -> Vec<PluginPanelAction> {
        build_actions(self.selected_row())
    }

    /// Highlighted action index.
    #[must_use]
    pub const fn action_selected(&self) -> usize {
        self.action_selected
    }

    /// Whether an install/update form or a remove confirmation is open.
    #[must_use]
    pub const fn is_prompting(&self) -> bool {
        self.form.is_some() || self.confirm.is_some()
    }

    fn clamp_action(&mut self) {
        let count = PluginOperation::ALL.len();
        if self.action_selected >= count {
            self.action_selected = count.saturating_sub(1);
        }
    }

    /// Rendered lines. Pure: no terminal, no styling beyond a tone token.
    #[must_use]
    pub fn lines(&self) -> Vec<PluginPanelLine> {
        let mut lines = vec![PluginPanelLine::new(
            PluginPanelTone::Heading,
            format!("plugins ({})", self.rows.len()),
        )];
        if self.rows.is_empty() {
            lines.push(PluginPanelLine::new(
                PluginPanelTone::Warn,
                "  none installed — use Install…",
            ));
        }
        // The card is a fixed-height modal, so a long list is windowed here
        // rather than clipped by the renderer: silently losing the key hints
        // and the last notice off the bottom is not an acceptable way to run
        // out of room.
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(VISIBLE_PLUGIN_ROWS);
        let end = start
            .saturating_add(VISIBLE_PLUGIN_ROWS)
            .min(self.rows.len());
        for (offset, row) in self.rows[start..end].iter().enumerate() {
            let index = start + offset;
            let selected = index == self.selected;
            lines.push(PluginPanelLine::new(
                if selected {
                    PluginPanelTone::Selected
                } else {
                    PluginPanelTone::Body
                },
                format!(
                    "{}{:<28} {:<12} {:<9} {}",
                    if selected { "● " } else { "  " },
                    row.id.as_str(),
                    row.active.as_str(),
                    if row.enabled { "enabled" } else { "disabled" },
                    match &row.rollback {
                        RollbackState::Available(version) => format!("rollback -> {version}"),
                        RollbackState::NoPreviousVersion => "no rollback target".to_owned(),
                        RollbackState::PreviousVersionPruned(version) => {
                            format!("rollback {version} pruned")
                        }
                        RollbackState::CacheUnknown(version) => {
                            format!("rollback {version}?")
                        }
                    }
                ),
            ));
        }
        if self.rows.len() > VISIBLE_PLUGIN_ROWS {
            lines.push(PluginPanelLine::new(
                PluginPanelTone::Dim,
                format!(
                    "  showing {}-{} of {}",
                    start.saturating_add(1),
                    end,
                    self.rows.len()
                ),
            ));
        }
        lines.push(PluginPanelLine::new(PluginPanelTone::Dim, ""));
        lines.push(PluginPanelLine::new(
            PluginPanelTone::Heading,
            PluginPanelSection::ALL
                .iter()
                .map(|section| {
                    if *section == self.section {
                        format!("[{}]", section.title())
                    } else {
                        format!(" {} ", section.title())
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        ));
        lines.extend(self.section_lines());
        lines.push(PluginPanelLine::new(PluginPanelTone::Dim, ""));
        lines.extend(self.action_lines());
        if let Some(form) = self.form.as_ref() {
            lines.push(PluginPanelLine::new(PluginPanelTone::Dim, ""));
            lines.extend(form_lines(form));
        }
        if let Some(confirm) = self.confirm.as_ref() {
            lines.push(PluginPanelLine::new(PluginPanelTone::Dim, ""));
            lines.push(PluginPanelLine::new(
                PluginPanelTone::Warn,
                format!(
                    "Remove `{}`? heycode forgets it; the cached package stays.",
                    confirm.id.as_str()
                ),
            ));
            lines.push(PluginPanelLine::new(
                PluginPanelTone::Selected,
                if confirm.remove {
                    "  cancel   [remove]"
                } else {
                    "  [cancel]   remove"
                },
            ));
        }
        if let Some(notice) = self.notice.as_ref() {
            lines.push(PluginPanelLine::new(PluginPanelTone::Dim, ""));
            lines.push(PluginPanelLine::new(
                if notice.ok {
                    PluginPanelTone::Good
                } else {
                    PluginPanelTone::Bad
                },
                notice.text.clone(),
            ));
        }
        lines.push(PluginPanelLine::new(PluginPanelTone::Dim, self.hint()));
        lines
    }

    /// Render a height-bounded visual projection.
    ///
    /// The complete [`Self::lines`] projection remains available to screen
    /// readers. This viewport keeps the selected plugin, current section,
    /// focused action or prompt, notices, and key hint reachable on the actual
    /// terminal surface. Long section facts are paged explicitly with
    /// PageUp/PageDown rather than being silently clipped by the renderer.
    #[must_use]
    pub fn lines_for_height(&self, height: u16) -> Vec<PluginPanelLine> {
        let capacity = usize::from(height);
        if capacity == 0 {
            return Vec::new();
        }
        if capacity == 1 {
            return vec![PluginPanelLine::new(PluginPanelTone::Dim, self.hint())];
        }

        let mut tail = self.visual_tail_lines();
        let hint = PluginPanelLine::new(PluginPanelTone::Dim, self.hint());
        // Extremely short terminals cannot carry six action rows. Keep the
        // focused action and any notice visible; Up/Down still reaches every
        // operation and the complete list remains in accessibility output.
        if self.form.is_none() && self.confirm.is_none() && capacity < 12 {
            let selected = self
                .action_lines()
                .into_iter()
                .nth(self.action_selected)
                .into_iter();
            let notice = self.notice.as_ref().map(|notice| {
                PluginPanelLine::new(
                    if notice.ok {
                        PluginPanelTone::Good
                    } else {
                        PluginPanelTone::Bad
                    },
                    notice.text.clone(),
                )
            });
            tail = selected.chain(notice).collect();
        }

        let fixed_without_plugins = 1_usize
            .saturating_add(1)
            .saturating_add(tail.len())
            .saturating_add(1);
        let mut plugin_limit = 3_usize.min(self.rows.len().max(1));
        while plugin_limit > 1
            && fixed_without_plugins.saturating_add(self.visual_plugin_lines(plugin_limit).len())
                > capacity
        {
            plugin_limit -= 1;
        }
        let plugin_lines = self.visual_plugin_lines(plugin_limit);
        let detail_budget =
            capacity.saturating_sub(fixed_without_plugins.saturating_add(plugin_lines.len()));
        let details = self.visual_section_lines(detail_budget);

        let mut lines = vec![PluginPanelLine::new(
            PluginPanelTone::Heading,
            format!("plugins ({})", self.rows.len()),
        )];
        lines.extend(plugin_lines);
        lines.push(self.section_tabs_line());
        lines.extend(details);
        lines.extend(tail);
        if lines.len() >= capacity {
            lines.truncate(capacity - 1);
        }
        lines.push(hint);
        lines
    }

    fn section_tabs_line(&self) -> PluginPanelLine {
        PluginPanelLine::new(
            PluginPanelTone::Heading,
            PluginPanelSection::ALL
                .iter()
                .map(|section| {
                    if *section == self.section {
                        format!("[{}]", section.title())
                    } else {
                        format!(" {} ", section.title())
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn visual_plugin_lines(&self, limit: usize) -> Vec<PluginPanelLine> {
        if self.rows.is_empty() {
            return vec![PluginPanelLine::new(
                PluginPanelTone::Warn,
                "  none installed — use Install…",
            )];
        }
        let shown = limit.max(1).min(self.rows.len());
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(shown)
            .min(self.rows.len().saturating_sub(shown));
        let end = start.saturating_add(shown).min(self.rows.len());
        let mut lines = self.rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let selected = start + offset == self.selected;
                PluginPanelLine::new(
                    if selected {
                        PluginPanelTone::Selected
                    } else {
                        PluginPanelTone::Body
                    },
                    format!(
                        "{}{:<28} {:<12} {:<9} {}",
                        if selected { "● " } else { "  " },
                        row.id.as_str(),
                        row.active.as_str(),
                        if row.enabled { "enabled" } else { "disabled" },
                        match &row.rollback {
                            RollbackState::Available(version) => format!("rollback -> {version}"),
                            RollbackState::NoPreviousVersion => "no rollback target".to_owned(),
                            RollbackState::PreviousVersionPruned(version) => {
                                format!("rollback {version} pruned")
                            }
                            RollbackState::CacheUnknown(version) => {
                                format!("rollback {version}?")
                            }
                        }
                    ),
                )
            })
            .collect::<Vec<_>>();
        if self.rows.len() > shown {
            lines.push(PluginPanelLine::new(
                PluginPanelTone::Dim,
                format!("  plugins {}-{} of {}", start + 1, end, self.rows.len()),
            ));
        }
        lines
    }

    fn visual_section_lines(&self, budget: usize) -> Vec<PluginPanelLine> {
        if budget == 0 {
            return Vec::new();
        }
        let all = self.section_lines();
        if all.len() <= budget {
            return all;
        }
        if budget == 1 {
            return vec![PluginPanelLine::new(
                PluginPanelTone::Warn,
                format!("details 0 of {} · PgDn", all.len()),
            )];
        }
        let visible = budget - 1;
        let start = self.section_offset.min(all.len().saturating_sub(visible));
        let end = start.saturating_add(visible).min(all.len());
        let mut lines = all[start..end].to_vec();
        lines.push(PluginPanelLine::new(
            PluginPanelTone::Dim,
            format!("details {}-{} of {} · PgUp/PgDn", start + 1, end, all.len()),
        ));
        lines
    }

    fn visual_tail_lines(&self) -> Vec<PluginPanelLine> {
        let mut lines = if let Some(form) = self.form.as_ref() {
            form_lines(form)
        } else if let Some(confirm) = self.confirm.as_ref() {
            vec![
                PluginPanelLine::new(
                    PluginPanelTone::Warn,
                    format!(
                        "Remove `{}`? heycode forgets it; the cached package stays.",
                        confirm.id.as_str()
                    ),
                ),
                PluginPanelLine::new(
                    PluginPanelTone::Selected,
                    if confirm.remove {
                        "  cancel   [remove]"
                    } else {
                        "  [cancel]   remove"
                    },
                ),
            ]
        } else {
            self.action_lines()
        };
        if let Some(notice) = self.notice.as_ref() {
            lines.push(PluginPanelLine::new(
                if notice.ok {
                    PluginPanelTone::Good
                } else {
                    PluginPanelTone::Bad
                },
                notice.text.clone(),
            ));
        }
        lines
    }

    fn section_lines(&self) -> Vec<PluginPanelLine> {
        let view = section_view(self.section, self.selected_row());
        let tone = match view.state {
            PluginSectionState::Live => PluginPanelTone::Body,
            PluginSectionState::NotDeclared => PluginPanelTone::Dim,
            PluginSectionState::NoEvidence => PluginPanelTone::Warn,
        };
        view.lines
            .into_iter()
            .map(|line| PluginPanelLine::new(tone, format!("  {line}")))
            .collect()
    }

    fn action_lines(&self) -> Vec<PluginPanelLine> {
        self.actions()
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let selected =
                    index == self.action_selected && self.focus == PluginPanelFocus::Actions;
                PluginPanelLine::new(
                    if selected {
                        PluginPanelTone::Selected
                    } else if action.is_available() {
                        PluginPanelTone::Body
                    } else {
                        PluginPanelTone::Warn
                    },
                    match action.unavailable_reason.as_deref() {
                        Some(reason) => format!(
                            "{}{} — unavailable: {reason}",
                            if selected { "● " } else { "  " },
                            action.label
                        ),
                        None => format!("{}{}", if selected { "● " } else { "  " }, action.label),
                    },
                )
            })
            .collect()
    }

    const fn hint(&self) -> &'static str {
        if self.confirm.is_some() {
            "←→ choose · enter confirm · esc cancel"
        } else if self.form.is_some() {
            "tab field · enter submit · esc cancel"
        } else if matches!(self.focus, PluginPanelFocus::Actions) {
            "↑↓ action · Tab section · PgUp/Dn detail · Enter · Esc"
        } else {
            "↑↓ plugin · Tab section · PgUp/Dn detail · Enter · Esc"
        }
    }

    /// Apply one key press.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> PluginPanelKeyOutcome {
        if self.confirm.is_some() {
            return self.handle_confirm_key(code);
        }
        if self.form.is_some() {
            return self.handle_form_key(code, modifiers);
        }
        match code {
            KeyCode::Esc if self.focus == PluginPanelFocus::Actions => {
                self.focus = PluginPanelFocus::Plugins;
                self.show_details = false;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Esc => PluginPanelKeyOutcome::Close,
            KeyCode::Tab => {
                self.show_details = true;
                self.section = self.section.next();
                self.section_offset = 0;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::BackTab => {
                self.show_details = true;
                self.section = self.section.previous();
                self.section_offset = 0;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::PageUp => {
                self.section_offset = self.section_offset.saturating_sub(3);
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::PageDown => {
                self.section_offset = self.section_offset.saturating_add(3);
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Up | KeyCode::Down if self.focus == PluginPanelFocus::Actions => {
                let count = PluginOperation::ALL.len();
                self.action_selected = if code == KeyCode::Up {
                    self.action_selected
                        .checked_sub(1)
                        .unwrap_or(count.saturating_sub(1))
                } else {
                    (self.action_selected + 1) % count
                };
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Up | KeyCode::Down => {
                self.move_plugin(code == KeyCode::Down);
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Left => {
                self.focus = PluginPanelFocus::Plugins;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Right | KeyCode::Enter if self.focus == PluginPanelFocus::Plugins => {
                self.focus = PluginPanelFocus::Actions;
                self.action_selected = if self.selected_row().is_some_and(|row| row.enabled) {
                    2
                } else {
                    1
                };
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Char(' ') if self.focus == PluginPanelFocus::Plugins => self
                .selected_row()
                .cloned()
                .map_or(PluginPanelKeyOutcome::Handled, |row| {
                    PluginPanelKeyOutcome::Run(PluginPanelIntent::SetEnabled {
                        id: row.id,
                        enabled: !row.enabled,
                    })
                }),
            KeyCode::Char('i') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.action_selected = 0;
                self.run_selected_action()
            }
            KeyCode::Char('i') if self.focus == PluginPanelFocus::Actions => {
                self.show_details = !self.show_details;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Backspace if self.focus == PluginPanelFocus::Plugins => {
                self.query.pop();
                self.select_first_match();
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Char(character)
                if self.focus == PluginPanelFocus::Plugins
                    && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control()
                    && self.query.len() < 256 =>
            {
                self.query.push(character);
                self.select_first_match();
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Enter => self.run_selected_action(),
            _ => PluginPanelKeyOutcome::Handled,
        }
    }

    fn matching_indices(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                row.id
                    .as_str()
                    .to_lowercase()
                    .contains(&query)
                    .then_some(index)
            })
            .collect()
    }

    fn select_first_match(&mut self) {
        self.selected = self
            .matching_indices()
            .first()
            .copied()
            .unwrap_or(self.rows.len());
        self.section_offset = 0;
    }

    fn move_plugin(&mut self, forward: bool) {
        let matches = self.matching_indices();
        if matches.is_empty() {
            return;
        }
        let position = matches
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let next = if forward {
            (position + 1) % matches.len()
        } else {
            position.checked_sub(1).unwrap_or(matches.len() - 1)
        };
        self.selected = matches[next];
        self.section_offset = 0;
    }

    /// Height of the current Installed list or plugin details view.
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        if self.focus == PluginPanelFocus::Plugins && !self.is_prompting() {
            u16::try_from(self.matching_indices().len().clamp(1, 8) + 8).unwrap_or(16)
        } else {
            20
        }
    }

    fn run_selected_action(&mut self) -> PluginPanelKeyOutcome {
        let actions = self.actions();
        let Some(action) = actions.get(self.action_selected) else {
            return PluginPanelKeyOutcome::Handled;
        };
        if let Some(reason) = action.unavailable_reason.as_deref() {
            self.note_failure(format!("{} unavailable: {reason}", action.label));
            return PluginPanelKeyOutcome::Handled;
        }
        let row = self.selected_row().cloned();
        match action.operation {
            PluginOperation::Install => {
                self.form = Some(PluginPanelForm {
                    operation: PluginOperation::Install,
                    id: String::new(),
                    version: String::new(),
                    field: PluginFormField::Id,
                });
                PluginPanelKeyOutcome::Handled
            }
            PluginOperation::Enable | PluginOperation::Disable => {
                row.map_or(PluginPanelKeyOutcome::Handled, |row| {
                    PluginPanelKeyOutcome::Run(PluginPanelIntent::SetEnabled {
                        enabled: !row.enabled,
                        id: row.id,
                    })
                })
            }
            PluginOperation::Update => {
                if let Some(row) = row {
                    self.form = Some(PluginPanelForm {
                        operation: PluginOperation::Update,
                        id: row.id.as_str().to_owned(),
                        version: String::new(),
                        field: PluginFormField::Version,
                    });
                }
                PluginPanelKeyOutcome::Handled
            }
            PluginOperation::Rollback => row.map_or(PluginPanelKeyOutcome::Handled, |row| {
                PluginPanelKeyOutcome::Run(PluginPanelIntent::Rollback { id: row.id })
            }),
            PluginOperation::Remove => {
                if let Some(row) = row {
                    self.confirm = Some(PluginPanelConfirm {
                        id: row.id,
                        remove: false,
                    });
                }
                PluginPanelKeyOutcome::Handled
            }
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) -> PluginPanelKeyOutcome {
        match code {
            KeyCode::Esc => {
                self.confirm = None;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                if let Some(confirm) = self.confirm.as_mut() {
                    confirm.remove = !confirm.remove;
                }
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Enter => match self.confirm.take() {
                Some(confirm) if confirm.remove => {
                    PluginPanelKeyOutcome::Run(PluginPanelIntent::Remove { id: confirm.id })
                }
                _ => PluginPanelKeyOutcome::Handled,
            },
            _ => PluginPanelKeyOutcome::Handled,
        }
    }

    fn handle_form_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> PluginPanelKeyOutcome {
        match code {
            KeyCode::Esc => {
                self.form = None;
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                if let Some(form) = self.form.as_mut()
                    && form.operation == PluginOperation::Install
                {
                    form.field = match form.field {
                        PluginFormField::Id => PluginFormField::Version,
                        PluginFormField::Version => PluginFormField::Id,
                    };
                }
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Backspace => {
                if let Some(form) = self.form.as_mut() {
                    match form.field {
                        PluginFormField::Id => form.id.pop(),
                        PluginFormField::Version => form.version.pop(),
                    };
                }
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Char(character)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(form) = self.form.as_mut() {
                    let field = match form.field {
                        PluginFormField::Id => &mut form.id,
                        PluginFormField::Version => &mut form.version,
                    };
                    if field.chars().count() < 128 {
                        field.push(character);
                    }
                }
                PluginPanelKeyOutcome::Handled
            }
            KeyCode::Enter => self.submit_form(),
            _ => PluginPanelKeyOutcome::Handled,
        }
    }

    /// Validate the form through the owning types rather than the panel.
    ///
    /// `PluginId` and `PluginVersion` are the validators; a malformed entry is
    /// reported where it was typed instead of becoming a lifecycle error.
    fn submit_form(&mut self) -> PluginPanelKeyOutcome {
        let Some(form) = self.form.as_ref() else {
            return PluginPanelKeyOutcome::Handled;
        };
        let (operation, raw_id, raw_version) =
            (form.operation, form.id.clone(), form.version.clone());
        let id = match PluginId::new(raw_id) {
            Ok(id) => id,
            Err(error) => {
                self.note_failure(format!("invalid plugin id: {error}"));
                return PluginPanelKeyOutcome::Handled;
            }
        };
        let version = match PluginVersion::parse(raw_version) {
            Ok(version) => version,
            Err(error) => {
                self.note_failure(format!("invalid version: {error}"));
                return PluginPanelKeyOutcome::Handled;
            }
        };
        self.form = None;
        PluginPanelKeyOutcome::Run(if operation == PluginOperation::Install {
            PluginPanelIntent::Install { id, version }
        } else {
            PluginPanelIntent::Update { id, version }
        })
    }
}

fn form_lines(form: &PluginPanelForm) -> Vec<PluginPanelLine> {
    let mut lines = vec![PluginPanelLine::new(
        PluginPanelTone::Heading,
        match form.operation {
            PluginOperation::Update => format!("update `{}`", form.id),
            _ => "install plugin".to_owned(),
        },
    )];
    if form.operation == PluginOperation::Install {
        lines.push(field_line(
            "id",
            &form.id,
            form.field == PluginFormField::Id,
        ));
    }
    lines.push(field_line(
        "version",
        &form.version,
        form.field == PluginFormField::Version,
    ));
    lines
}

fn field_line(label: &str, value: &str, selected: bool) -> PluginPanelLine {
    PluginPanelLine::new(
        if selected {
            PluginPanelTone::Selected
        } else {
            PluginPanelTone::Body
        },
        format!(
            "{}{label:<9} {}",
            if selected { "● " } else { "  " },
            if value.is_empty() { "—" } else { value }
        ),
    )
}

/// U03 descriptor for the plugin panel contribution.
///
/// Shared with the plugin so the registered identity and the tested identity
/// cannot drift.
///
/// # Errors
/// Static descriptor validation failure.
pub fn panel_descriptor()
-> Result<heycode_ui::UiContributionDescriptor, heycode_ui::UiRegistryError> {
    heycode_ui::UiContributionDescriptor::new(
        heycode_ui::UiSlot::Panel,
        "plugins",
        "Installed plugins",
        55,
    )
}
