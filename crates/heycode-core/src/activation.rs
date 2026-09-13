//! Verified plugin activation transactions and typed activation health.
//!
//! Activating one plugin is a transaction over the [`Context`]: its declared
//! rows, its services, its dynamic rows and its effects either all publish or
//! none do. The transaction is **verified, not trusted** — a rollback that
//! leaves anything behind fails loud as [`CoreError::BrokenActivation`] naming
//! the plugin and the residue, exactly as a partially applied plugin would.

use crate::{AppliedPlugin, Context, CoreError, PluginContribution, PluginScope};

/// Where in one plugin's activation the failure happened.
///
/// The stage separates "was refused before it could register anything" from
/// "registered and then failed", which the rollback contract treats
/// differently.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStage {
    /// Identity, uniqueness and inject verification, before any registration.
    Admission,
    /// Declaring the plugin's static [`crate::Plugin::inventory`] rows.
    Declaration,
    /// The plugin's own [`crate::Plugin::apply`].
    Apply,
    /// Recording the descriptor after a successful apply.
    Commit,
    /// Rollback left state the failed activation had registered.
    Rollback,
}

impl ActivationStage {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::Declaration => "declaration",
            Self::Apply => "apply",
            Self::Commit => "commit",
            Self::Rollback => "rollback",
        }
    }
}

impl std::fmt::Display for ActivationStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why one plugin's activation failed. The offending plugin is named by the
/// [`PluginActivation`] row that carries this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationFailure {
    /// Stage that refused the plugin.
    pub stage: ActivationStage,
    /// Safe rendering of the original failure.
    pub message: String,
}

/// Health of one requested plugin.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginActivationOutcome {
    /// Applied successfully; every contribution it registered is published.
    Activated,
    /// Reached and refused; nothing it registered is published.
    Failed(ActivationFailure),
    /// Never reached, because an earlier plugin aborted composition.
    NotAttempted,
}

impl PluginActivationOutcome {
    /// True only for a plugin whose contributions are live.
    #[must_use]
    pub const fn is_activated(&self) -> bool {
        matches!(self, Self::Activated)
    }

    /// The failure detail when this plugin ran and was refused.
    #[must_use]
    pub const fn failure(&self) -> Option<&ActivationFailure> {
        match self {
            Self::Failed(failure) => Some(failure),
            _ => None,
        }
    }
}

/// One requested plugin's activation health.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginActivation {
    /// Runtime plugin id.
    pub plugin: &'static str,
    /// Activation scope the plugin was requested at.
    pub scope: PluginScope,
    /// Typed health of this row.
    pub outcome: PluginActivationOutcome,
}

/// Activation health for every requested plugin, in requested order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivationReport {
    activations: Vec<PluginActivation>,
}

impl ActivationReport {
    pub(crate) fn from_rows(activations: Vec<PluginActivation>) -> Self {
        Self { activations }
    }

    /// Every requested plugin in requested order.
    #[must_use]
    pub fn activations(&self) -> &[PluginActivation] {
        &self.activations
    }

    /// True only when every requested plugin activated.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.activations
            .iter()
            .all(|row| row.outcome.is_activated())
    }

    /// The single plugin that ran and was refused, when one exists.
    #[must_use]
    pub fn failure(&self) -> Option<&PluginActivation> {
        self.activations
            .iter()
            .find(|row| row.outcome.failure().is_some())
    }

    /// Health of one requested plugin by id.
    #[must_use]
    pub fn outcome(&self, plugin: &str) -> Option<&PluginActivationOutcome> {
        self.activations
            .iter()
            .find(|row| row.plugin == plugin)
            .map(|row| &row.outcome)
    }

    /// Body-free diagnostic projection safe for doctor JSON/human output.
    #[must_use]
    pub fn diagnostic(&self) -> ActivationDiagnosticReport {
        ActivationDiagnosticReport {
            complete: true,
            healthy: self.healthy(),
            diagnostic_code: None,
            suppressed: Vec::new(),
            plugins: self
                .activations
                .iter()
                .map(|row| {
                    let (state, stage) = match &row.outcome {
                        PluginActivationOutcome::Activated => {
                            (ActivationDiagnosticState::Activated, None)
                        }
                        PluginActivationOutcome::Failed(failure) => (
                            ActivationDiagnosticState::Failed,
                            Some(failure.stage.as_str().to_owned()),
                        ),
                        PluginActivationOutcome::NotAttempted => {
                            (ActivationDiagnosticState::NotAttempted, None)
                        }
                    };
                    ActivationDiagnosticPlugin {
                        plugin: row.plugin.to_owned(),
                        scope: row.scope.as_str().to_owned(),
                        state,
                        stage,
                    }
                })
                .collect(),
        }
    }
}

/// Body-free activation state for one requested plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDiagnosticState {
    /// Plugin applied and committed.
    Activated,
    /// Plugin was reached and refused at `stage`.
    Failed,
    /// An earlier failure prevented this plugin from running.
    NotAttempted,
}

impl ActivationDiagnosticState {
    /// Stable human/wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::Failed => "failed",
            Self::NotAttempted => "not_attempted",
        }
    }
}

/// One requested plugin in a safe activation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ActivationDiagnosticPlugin {
    /// Plugin id.
    pub plugin: String,
    /// Effective activation scope.
    pub scope: String,
    /// Closed outcome.
    pub state: ActivationDiagnosticState,
    /// Failure stage when state is failed; raw failure text is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// Safe activation phase for composition doctor output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ActivationDiagnosticReport {
    /// True when the activation probe ran to a terminal report.
    pub complete: bool,
    /// True only when complete and every row activated.
    pub healthy: bool,
    /// Fixed reason when the isolated probe could not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
    /// External actions deliberately replaced by inert isolated inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<String>,
    /// Requested rows in activation order.
    pub plugins: Vec<ActivationDiagnosticPlugin>,
}

impl ActivationDiagnosticReport {
    /// Fixed unavailable result for an isolation/world-resolution failure.
    #[must_use]
    pub fn unavailable(code: &'static str) -> Self {
        Self {
            complete: false,
            healthy: false,
            diagnostic_code: Some(code.to_owned()),
            suppressed: Vec::new(),
            plugins: Vec::new(),
        }
    }

    /// Record one fixed external action omitted by the isolated probe.
    #[must_use]
    pub fn with_suppressed(mut self, action: &'static str) -> Self {
        self.suppressed.push(action.to_owned());
        self
    }

    /// Render the same body-free facts carried by JSON.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut lines = vec![format!(
            "activation: {}",
            if !self.complete {
                "unavailable"
            } else if self.healthy {
                "healthy"
            } else {
                "failed"
            }
        )];
        if let Some(code) = &self.diagnostic_code {
            lines.push(format!("error {code}"));
        }
        for suppressed in &self.suppressed {
            lines.push(format!("suppressed: {suppressed}"));
        }
        for row in &self.plugins {
            let stage = row
                .stage
                .as_deref()
                .map_or(String::new(), |stage| format!(" stage={stage}"));
            lines.push(format!(
                "[{}] {} scope={}{}",
                row.state.as_str(),
                row.plugin,
                row.scope,
                stage
            ));
        }
        lines.join("\n")
    }
}

/// One complete composition attempt.
///
/// `report` is always populated; `context` is `Ok` only when every requested
/// plugin activated. On failure the whole context has already been shut down
/// and the original error is returned unchanged, except when rollback itself
/// left residue — see [`CoreError::BrokenActivation`].
#[must_use]
pub struct ComposedActivation {
    /// Per-plugin activation health.
    pub report: ActivationReport,
    /// The live context, or the failure that aborted composition.
    pub context: crate::CoreResult<Context>,
}

/// Everything about a [`Context`] a plugin activation can change.
///
/// Captured before an activation and compared after its rollback: equality is
/// the definition of "this plugin published nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextFingerprint {
    services: Vec<(&'static str, &'static str)>,
    contributions: Vec<PluginContribution>,
    recorded: Vec<AppliedPlugin>,
    plugins: Vec<&'static str>,
    descriptors: Vec<crate::PluginDescriptor>,
    scopes: Vec<PluginScope>,
    effects: usize,
    listeners: usize,
}

impl ContextFingerprint {
    /// Observe the context's complete published state.
    ///
    /// # Errors
    /// [`CoreError::InventoryUnavailable`] when the shared inventory cannot be
    /// read; an unobservable context cannot be verified.
    pub(crate) fn capture(context: &Context) -> Result<Self, CoreError> {
        let snapshot = context.plugin_inventory().snapshot()?;
        let mut services: Vec<(&'static str, &'static str)> = context
            .services()
            .into_iter()
            .map(|(key, owner)| (key.as_str(), owner))
            .collect();
        services.sort_unstable();
        Ok(Self {
            services,
            contributions: snapshot.contributions,
            recorded: snapshot.plugins,
            plugins: context.plugins().to_vec(),
            descriptors: context.plugin_descriptors().to_vec(),
            scopes: context.plugin_scopes().to_vec(),
            effects: context.pending_effects(),
            listeners: context.events.listener_count(),
        })
    }

    /// Number of exact inventory rows observed.
    pub(crate) const fn contribution_count(&self) -> usize {
        self.contributions.len()
    }

    /// Number of pending disposers observed.
    pub(crate) const fn effect_count(&self) -> usize {
        self.effects
    }

    /// Describe what a rollback failed to remove, or `None` when the context
    /// is observably identical to `self`.
    pub(crate) fn residue(&self, after: &Self) -> Option<String> {
        let mut parts = Vec::new();
        let surviving_services: Vec<&str> = after
            .services
            .iter()
            .filter(|row| !self.services.contains(row))
            .map(|(key, _)| *key)
            .collect();
        if !surviving_services.is_empty() {
            parts.push(format!("services [{}]", surviving_services.join(", ")));
        }
        let surviving_rows: Vec<String> = after
            .contributions
            .iter()
            .filter(|row| !self.contributions.contains(row))
            .map(|row| format!("{}:{}", row.kind, row.name))
            .collect();
        if !surviving_rows.is_empty() {
            parts.push(format!("contributions [{}]", surviving_rows.join(", ")));
        }
        if self.recorded != after.recorded || self.plugins != after.plugins {
            parts.push("recorded plugins".to_owned());
        }
        if self.descriptors != after.descriptors || self.scopes != after.scopes {
            parts.push("plugin descriptors".to_owned());
        }
        if after.effects != self.effects {
            parts.push(format!(
                "pending effects {} -> {}",
                self.effects, after.effects
            ));
        }
        if after.listeners != self.listeners {
            parts.push(format!(
                "event listeners {} -> {}",
                self.listeners, after.listeners
            ));
        }
        if parts.is_empty() && self != after {
            parts.push("context state".to_owned());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }
}

/// State one in-flight activation may have to undo.
pub(crate) struct ActivationTransaction {
    pub(crate) before: ContextFingerprint,
    /// Service keys inserted during this activation, in insertion order.
    pub(crate) services: Vec<crate::ServiceKey>,
    pub(crate) contributions: usize,
    pub(crate) effects: usize,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_names_stage_and_not_attempted_rows_without_failure_body() {
        let report = ActivationReport::from_rows(vec![
            PluginActivation {
                plugin: "before",
                scope: PluginScope::BuiltIn,
                outcome: PluginActivationOutcome::Activated,
            },
            PluginActivation {
                plugin: "culprit",
                scope: PluginScope::User,
                outcome: PluginActivationOutcome::Failed(ActivationFailure {
                    stage: ActivationStage::Apply,
                    message: "SECRET-ACTIVATION-BODY-CANARY".to_owned(),
                }),
            },
            PluginActivation {
                plugin: "after",
                scope: PluginScope::Project,
                outcome: PluginActivationOutcome::NotAttempted,
            },
        ]);

        let diagnostic = report.diagnostic();
        let doctor = crate::CompositionDoctorReport::new(
            crate::CompositionReport {
                healthy: true,
                plugins: Vec::new(),
                diagnostics: Vec::new(),
            },
            Some(diagnostic),
        );
        assert!(!doctor.healthy);
        let json = serde_json::to_string(&doctor).unwrap();
        let human = doctor.render_human();
        for rendered in [&json, &human] {
            assert!(rendered.contains("culprit"), "{rendered}");
            assert!(rendered.contains("apply"), "{rendered}");
            assert!(rendered.contains("not_attempted"), "{rendered}");
            assert!(!rendered.contains("SECRET-ACTIVATION-BODY-CANARY"));
        }
    }
}
