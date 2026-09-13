//! Atomic live rescan for the filesystem-owned skill generation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use heycode_agent::{
    Agent, Command, CommandAvailability, CommandDescriptor, CommandRegistry, CommandSource,
    CommandTiming, UiEvent,
};
use heycode_core::{Context, CoreError, CoreResult};

use crate::{
    Skill, SkillAdmission, SkillDiscovery, SkillEntry, SkillRecord, SkillRegistryError, SkillRoot,
    SkillSet, SkillState, active_rows, can_share_canonical, discover_report, row_admission,
    row_owns_identity, validate_skill_name,
};

/// Admission check performed immediately before a rescan begins.
///
/// The default pinned guard is appropriate for a composition whose workspace
/// cannot change. Dynamic workspace hosts use [`WorkspaceSkillReloadGuard`] so
/// `/reload-skills` cannot quietly keep scanning an earlier directory after a
/// `/cd` or worktree transition.
pub trait SkillReloadGuard: Send + Sync {
    /// Whether the owning plugin must inject the workspace-transition service.
    #[doc(hidden)]
    fn requires_workspace(&self) -> bool {
        false
    }

    /// Bind composition-owned authority after dependency resolution.
    ///
    /// # Errors
    /// A missing or conflicting required service fails composition.
    #[doc(hidden)]
    fn bind_context(&self, _context: &Context) -> CoreResult<()> {
        Ok(())
    }

    /// Validate that the configured roots still describe the active authority.
    ///
    /// # Errors
    /// A changed or unavailable authority refuses the rescan before discovery.
    fn validate(&self) -> Result<(), SkillReloadError>;
}

/// Guard for a process whose configured skill roots remain pinned by design.
#[derive(Debug, Default)]
pub struct PinnedSkillReloadGuard;

impl SkillReloadGuard for PinnedSkillReloadGuard {
    fn validate(&self) -> Result<(), SkillReloadError> {
        Ok(())
    }
}

/// Refuses rescans after the dynamic workspace leaves the directory whose
/// project skill roots were bound at composition.
pub struct WorkspaceSkillReloadGuard {
    workspace: OnceLock<Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>>,
    expected_cwd: PathBuf,
}

impl WorkspaceSkillReloadGuard {
    /// Bind to one already-canonical project directory.
    ///
    /// # Errors
    /// A relative, missing, inaccessible or non-directory path is refused.
    pub fn new(
        workspace: Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>,
        expected_cwd: impl AsRef<Path>,
    ) -> Result<Self, SkillReloadError> {
        let expected_cwd = std::fs::canonicalize(expected_cwd.as_ref())
            .map_err(|_| SkillReloadError::ScopeUnavailable)?;
        if !expected_cwd.is_dir() {
            return Err(SkillReloadError::ScopeUnavailable);
        }
        let slot = OnceLock::new();
        let _ = slot.set(workspace);
        Ok(Self {
            workspace: slot,
            expected_cwd,
        })
    }

    /// Defer workspace service binding until plugin composition while fixing
    /// the exact project directory whose roots were authorized.
    ///
    /// # Errors
    /// A relative, missing, inaccessible or non-directory path is refused.
    pub fn deferred(expected_cwd: impl AsRef<Path>) -> Result<Self, SkillReloadError> {
        let expected_cwd = std::fs::canonicalize(expected_cwd.as_ref())
            .map_err(|_| SkillReloadError::ScopeUnavailable)?;
        if !expected_cwd.is_dir() {
            return Err(SkillReloadError::ScopeUnavailable);
        }
        Ok(Self {
            workspace: OnceLock::new(),
            expected_cwd,
        })
    }
}

impl SkillReloadGuard for WorkspaceSkillReloadGuard {
    fn requires_workspace(&self) -> bool {
        true
    }

    fn bind_context(&self, context: &Context) -> CoreResult<()> {
        let handle = context
            .get::<heycode_agent::workspace_transition::WorkspaceTransitionHandle>(
                heycode_agent::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
            )
            .ok_or_else(|| CoreError::other("skills workspace authority missing"))?;
        if let Some(bound) = self.workspace.get() {
            if Arc::ptr_eq(bound, &handle.0) {
                return Ok(());
            }
            return Err(CoreError::other(
                "skills reload guard was bound to a different workspace authority",
            ));
        }
        self.workspace
            .set(handle.0.clone())
            .map_err(|_| CoreError::other("skills workspace authority could not be bound"))
    }

    fn validate(&self) -> Result<(), SkillReloadError> {
        let snapshot = self
            .workspace
            .get()
            .ok_or(SkillReloadError::ScopeUnavailable)?
            .snapshot()
            .map_err(|_| SkillReloadError::ScopeUnavailable)?;
        if snapshot.cwd == self.expected_cwd && snapshot.worktree.is_none() {
            Ok(())
        } else {
            Err(SkillReloadError::ScopeChanged)
        }
    }
}

/// A refused rescan never changes the live skill generation.
#[derive(Debug, thiserror::Error)]
pub enum SkillReloadError {
    /// The service was constructed from an in-memory snapshot only.
    #[error("no skill discovery roots are configured")]
    NoRoots,
    /// Dynamic workspace authority no longer matches the bound project roots.
    #[error("workspace changed since skill roots were bound; recompose before reloading skills")]
    ScopeChanged,
    /// Workspace authority could not be revalidated.
    #[error("workspace authority is unavailable; existing skills were retained")]
    ScopeUnavailable,
    /// One or more roots did not produce a stable complete snapshot.
    #[error("skill rescan failed; existing skills were retained: {summary}")]
    Discovery {
        /// Deterministically ordered, source-attributed safe diagnostics.
        diagnostics: Vec<String>,
        /// Single-line rendering of `diagnostics` for the error display.
        summary: String,
    },
    /// A newly discovered name conflicts with a live declarative contribution.
    #[error(
        "skill rescan conflicts with live contribution `{name}`; existing skills were retained"
    )]
    ContributionConflict {
        /// Contested validated skill name.
        name: String,
    },
    /// Registry synchronization failed before publication.
    #[error("skill registry is unavailable; existing skills were retained")]
    RegistryUnavailable,
    /// A candidate report violated the internal name/source invariant.
    #[error("skill rescan produced an invalid candidate; existing skills were retained")]
    InvalidCandidate,
}

impl SkillReloadError {
    /// Safe root diagnostics, empty for non-discovery failures.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        match self {
            Self::Discovery { diagnostics, .. } => diagnostics,
            _ => &[],
        }
    }
}

/// Exact effect of one successful rescan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillReloadOutcome {
    /// Registry generation now serving readers. Unchanged scans do not advance it.
    pub generation: u64,
    /// Newly admitted filesystem-owned names.
    pub added: usize,
    /// Withdrawn filesystem-owned names.
    pub removed: usize,
    /// Same-name rows whose body, metadata or source changed.
    pub updated: usize,
    /// Candidate directories skipped for deterministic user-data reasons.
    pub skipped: usize,
    /// Whether the model-visible skills catalog bytes may differ next request.
    pub prompt_changed: bool,
}

impl SkillReloadOutcome {
    /// Bounded, deterministic text used by the slash command and tests.
    #[must_use]
    pub fn render(&self) -> String {
        let prompt = if self.prompt_changed {
            "model catalog changed; the next request receives the new prompt prefix"
        } else {
            "model catalog unchanged; no prompt-cache invalidation is claimed"
        };
        format!(
            "skills generation {}: {} added, {} removed, {} updated, {} skipped; {prompt}",
            self.generation, self.added, self.removed, self.updated, self.skipped
        )
    }
}

impl SkillSet {
    pub(crate) fn from_discovery_with_reload(
        discovery: SkillDiscovery,
        roots: Vec<SkillRoot>,
        reload_guard: Arc<dyn SkillReloadGuard>,
    ) -> Result<Self, SkillRegistryError> {
        Self::from_discovery_with_reload_and_preferences(
            discovery,
            roots,
            reload_guard,
            crate::preferences::detached_preferences(),
            None,
        )
    }

    pub(crate) fn from_discovery_with_reload_and_preferences(
        discovery: SkillDiscovery,
        roots: Vec<SkillRoot>,
        reload_guard: Arc<dyn SkillReloadGuard>,
        preferences: Arc<std::sync::RwLock<crate::preferences::PreferenceState>>,
        preferences_binding: Option<crate::preferences::PreferencesBinding>,
    ) -> Result<Self, SkillRegistryError> {
        if discovery.skills.len() != discovery.sources.len() {
            return Err(SkillRegistryError::Unavailable);
        }
        let mut entries = Vec::with_capacity(discovery.skills.len());
        for (skill, source) in discovery.skills.into_iter().zip(discovery.sources) {
            validate_skill_name(&skill.name)?;
            if entries
                .iter()
                .any(|row: &SkillEntry| row.record.skill.name == skill.name)
            {
                return Err(SkillRegistryError::Duplicate {
                    name: skill.name.clone(),
                });
            }
            entries.push(SkillEntry {
                record: SkillRecord { skill, source },
                preference_aliases: Vec::new(),
                token: Arc::new(()),
                discovered: true,
            });
        }
        Ok(Self {
            inner: Arc::new(std::sync::Mutex::new(SkillState {
                entries,
                skipped: discovery.skipped,
                revision: 1,
                reload_diagnostics: discovery
                    .issues
                    .into_iter()
                    .map(|issue| format!("{}: {}", issue.root, issue.reason))
                    .collect(),
            })),
            roots: Arc::new(roots),
            reload_guard,
            preferences,
            preferences_binding,
        })
    }

    /// Rescan all originally authorized roots and publish one complete
    /// filesystem-owned generation. Live declarative contributions survive.
    ///
    /// # Errors
    /// Scope drift, unstable roots, contribution collisions and unavailable
    /// registry state all fail before the live rows change.
    pub fn reload(&self) -> Result<SkillReloadOutcome, SkillReloadError> {
        self.reload_readiness()?;
        let discovery = discover_report(&self.roots);
        if !discovery.issues.is_empty() {
            let diagnostics = discovery
                .issues
                .iter()
                .map(|issue| format!("{}: {}", issue.root, issue.reason))
                .collect::<Vec<_>>();
            self.record_reload_diagnostics(diagnostics.clone())?;
            return Err(SkillReloadError::Discovery {
                summary: diagnostics.join("; "),
                diagnostics,
            });
        }
        if discovery.skills.len() != discovery.sources.len()
            || discovery
                .skills
                .iter()
                .any(|skill| validate_skill_name(&skill.name).is_err())
        {
            return Err(SkillReloadError::InvalidCandidate);
        }

        let candidates = discovery
            .skills
            .into_iter()
            .zip(discovery.sources)
            .map(|(skill, source)| SkillRecord { skill, source })
            .collect::<Vec<_>>();
        let mut state = self
            .inner
            .lock()
            .map_err(|_| SkillReloadError::RegistryUnavailable)?;
        for candidate in &candidates {
            if state
                .entries
                .iter()
                .filter(|row| !row.discovered)
                .any(|row| {
                    row_owns_identity(row, &candidate.skill.name)
                        && (row.record.skill.name != candidate.skill.name
                            || !can_share_canonical(row, candidate.source.scope, true))
                })
            {
                state.reload_diagnostics = vec![format!(
                    "{}: conflicts with a live contribution",
                    candidate.skill.name
                )];
                return Err(SkillReloadError::ContributionConflict {
                    name: candidate.skill.name.clone(),
                });
            }
        }

        let preferences = self
            .preferences
            .read()
            .map_err(|_| SkillReloadError::RegistryUnavailable)?;
        let old_prompt = prompt_projection(active_rows(&state.entries).into_iter().map(|row| {
            (
                &row.record.skill,
                row_admission(row, &preferences.admission_overrides),
            )
        }));
        let old = state
            .entries
            .iter()
            .filter(|row| row.discovered)
            .map(|row| (row.record.skill.name.clone(), row.record.clone()))
            .collect::<BTreeMap<_, _>>();
        let new = candidates
            .iter()
            .map(|record| (record.skill.name.clone(), record.clone()))
            .collect::<BTreeMap<_, _>>();
        let added = new.keys().filter(|name| !old.contains_key(*name)).count();
        let removed = old.keys().filter(|name| !new.contains_key(*name)).count();
        let updated = new
            .iter()
            .filter(|(name, record)| old.get(*name).is_some_and(|old| old != *record))
            .count();
        let mut next_entries = candidates
            .iter()
            .cloned()
            .map(|record| SkillEntry {
                record,
                preference_aliases: Vec::new(),
                token: Arc::new(()),
                discovered: true,
            })
            .collect::<Vec<_>>();
        next_entries.extend(state.entries.iter().filter(|row| !row.discovered).cloned());
        let new_prompt = prompt_projection(active_rows(&next_entries).into_iter().map(|row| {
            (
                &row.record.skill,
                row_admission(row, &preferences.admission_overrides),
            )
        }));
        let prompt_changed = old_prompt != new_prompt;
        let skipped_changed = state.skipped != discovery.skipped;
        let changed = added != 0 || removed != 0 || updated != 0 || skipped_changed;

        if changed {
            state.entries = next_entries;
            state.skipped = discovery.skipped;
            state.revision = state.revision.saturating_add(1);
        }
        state.reload_diagnostics.clear();
        Ok(SkillReloadOutcome {
            generation: state.revision,
            added,
            removed,
            updated,
            skipped: state.skipped.len(),
            prompt_changed,
        })
    }

    /// Validate dynamic `/reload-skills` availability without scanning files.
    ///
    /// # Errors
    /// Missing roots or changed/unavailable workspace authority are reported
    /// with the same safe reason execution would return.
    pub fn reload_readiness(&self) -> Result<(), SkillReloadError> {
        if self.roots.is_empty() {
            return Err(SkillReloadError::NoRoots);
        }
        self.reload_guard.validate()
    }

    fn record_reload_diagnostics(&self, diagnostics: Vec<String>) -> Result<(), SkillReloadError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| SkillReloadError::RegistryUnavailable)?;
        state.reload_diagnostics = diagnostics;
        Ok(())
    }
}

fn prompt_projection<'a>(
    skills: impl Iterator<Item = (&'a Skill, SkillAdmission)>,
) -> Vec<(String, String)> {
    skills
        .filter(|(_, admission)| admission.model_invocable())
        .map(|(skill, admission)| {
            (
                skill.name.clone(),
                if admission.description_visible() {
                    skill.description.clone()
                } else {
                    String::new()
                },
            )
        })
        .collect()
}

struct ReloadSkillsCommand {
    skills: SkillSet,
    descriptor: CommandDescriptor,
}

#[async_trait]
impl Command for ReloadSkillsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        match self.skills.reload_readiness() {
            Ok(()) => CommandAvailability::available(),
            Err(error) => CommandAvailability::unavailable(error.to_string())
                .unwrap_or_else(|_| CommandAvailability::available()),
        }
    }

    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("usage: /reload-skills");
        }
        let outcome = self
            .skills
            .reload()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent.ui().emit(UiEvent::Info {
            text: outcome.render(),
        });
        Ok(())
    }
}

pub(crate) fn register(
    context: &Context,
    commands: &CommandRegistry,
    skills: SkillSet,
) -> CoreResult<()> {
    let descriptor = CommandDescriptor::new(
        "reload-skills",
        "Rescan authorized skill roots atomically",
        Vec::new(),
        CommandTiming::Immediate,
        CommandSource::from_plugin("skills")
            .map_err(|error| CoreError::other(error.to_string()))?,
    )
    .map_err(|error| CoreError::other(error.to_string()))?;
    commands
        .register_effect(
            context,
            Arc::new(ReloadSkillsCommand { skills, descriptor }),
        )
        .map_err(|error| CoreError::other(error.to_string()))
}
