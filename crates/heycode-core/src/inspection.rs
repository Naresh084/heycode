//! Side-effect-free composition graph inspection.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{PluginSource, ScopedPlugin};

/// One exact static contribution in a dry plugin report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositionContributionReport {
    /// Stable exact namespace id.
    pub kind: String,
    /// Named row.
    pub name: String,
}

/// One selected plugin and its declared dependency/contribution shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositionPluginReport {
    /// Runtime plugin id.
    pub id: String,
    /// Implementation version.
    pub version: String,
    /// Implementation provenance.
    pub source: String,
    /// Winning activation scope.
    pub scope: String,
    /// Required prior service keys.
    pub injects: Vec<String>,
    /// Declared service keys.
    pub provides: Vec<String>,
    /// Declared exact static rows (dynamic rows require activation).
    pub contributions: Vec<CompositionContributionReport>,
    /// `ready` or `blocked` under this dry graph.
    pub state: String,
}

/// Safe composition diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositionDiagnostic {
    /// Stable machine code.
    pub code: String,
    /// Offending plugin when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// Safe human detail.
    pub message: String,
    /// Stable related service/plugin/contribution ids.
    pub related: Vec<String>,
}

/// Complete dry composition report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositionReport {
    /// True only when no graph/identity/collision diagnostic exists.
    pub healthy: bool,
    /// Selected plugin rows in requested order.
    pub plugins: Vec<CompositionPluginReport>,
    /// Deterministic diagnostics in discovery order.
    pub diagnostics: Vec<CompositionDiagnostic>,
}

/// Two-phase `doctor --composition` report: zero-apply graph inspection plus
/// an isolated production activation probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositionDoctorReport {
    /// Stable report schema.
    pub schema_version: u8,
    /// True only when graph and activation phases are both healthy.
    pub healthy: bool,
    /// Side-effect-free descriptor/dependency/collision phase.
    pub graph: CompositionReport,
    /// Isolated activation phase; absent when the graph could not resolve.
    pub activation: Option<crate::ActivationDiagnosticReport>,
}

impl CompositionDoctorReport {
    /// Join the two authoritative phases.
    #[must_use]
    pub fn new(
        graph: CompositionReport,
        activation: Option<crate::ActivationDiagnosticReport>,
    ) -> Self {
        let healthy = graph.healthy
            && activation
                .as_ref()
                .is_some_and(|activation| activation.healthy);
        Self {
            schema_version: 1,
            healthy,
            graph,
            activation,
        }
    }

    /// Render the same safe facts carried by JSON.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut rendered = self.graph.render_human();
        rendered.push('\n');
        match &self.activation {
            Some(activation) => rendered.push_str(&activation.render_human()),
            None => rendered.push_str("activation: not_run"),
        }
        rendered
    }
}

impl CompositionReport {
    /// Render a compact human report without configuration values or secrets.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut lines = vec![format!(
            "composition: {}",
            if self.healthy { "healthy" } else { "invalid" }
        )];
        for plugin in &self.plugins {
            lines.push(format!(
                "[{}] {}@{} source={} scope={}",
                plugin.state, plugin.id, plugin.version, plugin.source, plugin.scope
            ));
            if !plugin.injects.is_empty() {
                lines.push(format!("  injects: {}", plugin.injects.join(", ")));
            }
            if !plugin.provides.is_empty() {
                lines.push(format!("  provides: {}", plugin.provides.join(", ")));
            }
            for row in &plugin.contributions {
                lines.push(format!("  {}: {}", row.kind, row.name));
            }
        }
        for diagnostic in &self.diagnostics {
            let plugin = diagnostic
                .plugin
                .as_deref()
                .map_or(String::new(), |plugin| format!(" plugin={plugin}"));
            lines.push(format!(
                "error {}{plugin}: {} [{}]",
                diagnostic.code,
                diagnostic.message,
                diagnostic.related.join(", ")
            ));
        }
        lines.join("\n")
    }
}

/// Inspect a resolved scoped plugin vector without invoking any `apply` or
/// disposer. Dynamic contributions and implementation-internal failures are
/// intentionally outside this dry graph and belong to activation health.
#[must_use]
pub fn inspect_composition(plugins: &[ScopedPlugin]) -> CompositionReport {
    let mut reports = Vec::with_capacity(plugins.len());
    let mut diagnostics = Vec::new();
    let mut plugin_ids = BTreeSet::new();
    let mut services: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    let mut exact: BTreeMap<(crate::ContributionKind, String), &'static str> = BTreeMap::new();

    for scoped in plugins {
        let plugin = scoped.plugin();
        let descriptor = plugin.descriptor();
        let injects: Vec<_> = plugin
            .inject()
            .iter()
            .map(|key| key.as_str().to_owned())
            .collect();
        let provides: Vec<_> = plugin
            .provides()
            .iter()
            .map(|key| key.as_str().to_owned())
            .collect();
        let contributions: Vec<_> = plugin
            .inventory()
            .iter()
            .map(|row| CompositionContributionReport {
                kind: row.kind.as_str().to_owned(),
                name: row.name.clone(),
            })
            .collect();
        let mut blocked = false;

        if descriptor.id != plugin.name() {
            blocked = true;
            diagnostics.push(diagnostic(
                "descriptor_id_mismatch",
                plugin.name(),
                "plugin name and descriptor id differ",
                vec![descriptor.id.to_owned()],
            ));
        }
        if !plugin_ids.insert(plugin.name()) {
            blocked = true;
            diagnostics.push(diagnostic(
                "duplicate_plugin",
                plugin.name(),
                "plugin id appears more than once",
                vec![plugin.name().to_owned()],
            ));
        }
        let missing: Vec<_> = plugin
            .inject()
            .iter()
            .filter(|key| !services.contains_key(key.as_str()))
            .map(|key| key.as_str().to_owned())
            .collect();
        if !missing.is_empty() {
            blocked = true;
            diagnostics.push(diagnostic(
                "missing_dependency",
                plugin.name(),
                "required services are not available before this row",
                missing,
            ));
        }
        let mut local_provides = BTreeSet::new();
        for key in plugin.provides() {
            if !local_provides.insert(key.as_str()) {
                blocked = true;
                diagnostics.push(diagnostic(
                    "duplicate_provide",
                    plugin.name(),
                    "plugin declares one service more than once",
                    vec![key.as_str().to_owned()],
                ));
            } else if let Some(existing) = services.get(key.as_str()) {
                blocked = true;
                diagnostics.push(diagnostic(
                    "service_collision",
                    plugin.name(),
                    "service key already has an earlier owner",
                    vec![key.as_str().to_owned(), (*existing).to_owned()],
                ));
            }
        }
        let mut local_exact = BTreeSet::new();
        for row in plugin.inventory() {
            let identity = (row.kind, row.name.clone());
            if row.name.is_empty()
                || row.name.trim() != row.name
                || row.name.len() > 256
                || row.name.chars().any(char::is_control)
            {
                blocked = true;
                diagnostics.push(diagnostic(
                    "invalid_contribution",
                    plugin.name(),
                    "exact contribution name is invalid",
                    vec![row.kind.as_str().to_owned()],
                ));
                continue;
            }
            if descriptor.source != PluginSource::Unclassified
                && !descriptor
                    .contributions
                    .contains(&row.kind.descriptor_family())
            {
                blocked = true;
                diagnostics.push(diagnostic(
                    "contribution_family_mismatch",
                    plugin.name(),
                    "exact contribution is outside the descriptor family",
                    vec![row.kind.as_str().to_owned(), row.name],
                ));
                continue;
            }
            if !local_exact.insert(identity.clone()) {
                blocked = true;
                diagnostics.push(diagnostic(
                    "duplicate_contribution",
                    plugin.name(),
                    "plugin declares one exact row more than once",
                    vec![row.kind.as_str().to_owned(), row.name],
                ));
            } else if let Some(existing) = exact.get(&identity) {
                blocked = true;
                diagnostics.push(diagnostic(
                    "contribution_collision",
                    plugin.name(),
                    "exact contribution already has an earlier owner",
                    vec![
                        row.kind.as_str().to_owned(),
                        row.name,
                        (*existing).to_owned(),
                    ],
                ));
            }
        }

        if !blocked {
            for key in plugin.provides() {
                services.insert(key.as_str(), plugin.name());
            }
            for row in plugin.inventory() {
                exact.insert((row.kind, row.name), plugin.name());
            }
        }
        reports.push(CompositionPluginReport {
            id: plugin.name().to_owned(),
            version: descriptor.version.to_owned(),
            source: descriptor.source.as_str().to_owned(),
            scope: scoped.scope().as_str().to_owned(),
            injects,
            provides,
            contributions,
            state: if blocked { "blocked" } else { "ready" }.to_owned(),
        });
    }
    CompositionReport {
        healthy: diagnostics.is_empty(),
        plugins: reports,
        diagnostics,
    }
}

fn diagnostic(
    code: &str,
    plugin: &str,
    message: &str,
    related: Vec<String>,
) -> CompositionDiagnostic {
    CompositionDiagnostic {
        code: code.to_owned(),
        plugin: Some(plugin.to_owned()),
        message: message.to_owned(),
        related,
    }
}
