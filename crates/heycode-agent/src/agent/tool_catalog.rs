//! Read-only tool inventory. Configuration, request preparation and successful
//! execution are deliberately separate evidence planes.
use super::Agent;
use heycode_core::ContributionKind;
use heycode_session::SessionEventKind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// One registered implementation and its observed availability stages.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCatalogRow {
    /// Canonical client name or native implementation id.
    pub name: String,
    /// Client or provider-native implementation family.
    pub kind: String,
    /// Exact inventory owners; empty means unattributed, never inferred.
    pub owners: Vec<String>,
    /// Compatibility names accepted without duplicate model schemas.
    pub aliases: Vec<String>,
    /// Registered callable schema, absent for a provider-hosted implementation.
    pub schema: Option<heycode_core::ToolSpec>,
    /// Side-effect-free setup evidence from the owning implementation.
    pub prerequisite: heycode_tools::ToolPrerequisiteStatus,
    /// Whether the active route selected this implementation, where known.
    pub route_selected: Option<bool>,
    /// Current policy classification. Exact eligibility depends on call arguments.
    pub permission: String,
    /// Presence in the latest prepared request on the current route, not wire proof.
    pub prepared: Option<bool>,
    /// Successful completed client calls in this session, separate from setup.
    pub successful_calls: Option<usize>,
    /// Action of the last successful call (status does not prove other operations).
    pub last_successful_action: Option<String>,
    /// Human-readable limitations explaining unknown or unavailable stages.
    pub reason: String,
}

/// Live registration and bounded session evidence for `/tools` and other surfaces.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCatalogSnapshot {
    /// Selected inference provider.
    pub provider: String,
    /// Selected model.
    pub model: String,
    /// Number of registered client implementations.
    pub registered_client_count: usize,
    /// Number of registered native candidates (including incompatible candidates).
    pub registered_native_count: usize,
    /// Client schema count in the last matching prepared request, if recorded.
    pub prepared_client_count: Option<usize>,
    /// No unconditional enabled count is inferred from schemas or successful calls.
    pub enabled_count: Option<usize>,
    /// Limitations of the snapshot and route resolution failures.
    pub evidence: String,
    /// Rows ordered by kind and canonical name.
    pub tools: Vec<ToolCatalogRow>,
}

impl Agent {
    /// Inspect registered tools without invoking them, requesting permission, or
    /// scheduling inference. Old route headers never attest the current route.
    ///
    /// # Errors
    /// Inventory, native registry or session locks are unavailable.
    pub fn tool_catalog(
        &self,
        inventory: &heycode_core::PluginInventory,
    ) -> anyhow::Result<ToolCatalogSnapshot> {
        let inventory = inventory.snapshot()?;
        let selection = self.selection();
        let candidates = self.native_tools.implementations()?;
        let routes = self
            .native_tools
            .resolve_for_model(&selection.provider_name, &selection.model);
        let policy = self.approval.kind();
        let permission = format!(
            "{}; eligibility checked for each invocation and its arguments",
            policy.as_str()
        );
        let owners = |name: &str, kind| {
            inventory
                .contributions
                .iter()
                .filter(|row| row.kind == kind && row.name == name)
                .map(|row| row.plugin.to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session unavailable"))?;
        let header = session
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                SessionEventKind::RequestHeader { header, .. } => Some(header.as_ref()),
                _ => None,
            })
            .filter(|header| {
                header.provider == selection.provider_name && header.model == selection.model
            });
        let mut calls = HashMap::new();
        let mut success: BTreeMap<String, (usize, Option<String>)> = BTreeMap::new();
        let mut native_requests = HashMap::new();
        let mut native_calls = HashMap::new();
        let mut native_success: BTreeMap<String, usize> = BTreeMap::new();
        for event in session.events() {
            match &event.kind {
                SessionEventKind::RequestHeader {
                    request_id, header, ..
                } => {
                    native_requests.insert(request_id.clone(), &header.options.native_tool_routes);
                }
                SessionEventKind::ServerToolCall {
                    request_id, call, ..
                } => {
                    if let Some(route) = native_requests.get(request_id).and_then(|routes| {
                        routes
                            .iter()
                            .find(|route| route.logical() == call.logical())
                    }) {
                        native_calls.insert(
                            (request_id.clone(), call.id().clone()),
                            route.implementation().to_owned(),
                        );
                    }
                }
                SessionEventKind::ServerToolResult {
                    request_id, result, ..
                } => {
                    if let Some(name) =
                        native_calls.remove(&(request_id.clone(), result.call_id().clone()))
                        && result.outcome() == heycode_core::ServerToolOutcome::Success
                    {
                        *native_success.entry(name).or_default() += 1;
                    }
                }
                SessionEventKind::ToolCall {
                    call_id,
                    name,
                    args,
                    ..
                } => {
                    let canonical = self
                        .tools
                        .get(name)
                        .map_or_else(|| name.clone(), |tool| tool.spec().name);
                    calls.insert(
                        call_id.clone(),
                        (
                            canonical,
                            args.get("action")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned),
                        ),
                    );
                }
                SessionEventKind::ToolResult {
                    call_id, is_error, ..
                }
                | SessionEventKind::RichToolResult {
                    call_id, is_error, ..
                } => {
                    if let Some((name, action)) = calls.remove(call_id)
                        && !is_error
                    {
                        let entry = success.entry(name).or_default();
                        entry.0 += 1;
                        entry.1 = action;
                    }
                }
                _ => {}
            }
        }
        let mut rows = Vec::new();
        for spec in self.tools.specs() {
            let Some(tool) = self.tools.get(&spec.name) else {
                continue;
            };
            let prerequisite = tool.prerequisite_status();
            let prepared = header.map(|header| {
                header
                    .tools
                    .iter()
                    .any(|candidate| candidate.name == spec.name)
            });
            let (successful_calls, last_successful_action) =
                success.remove(&spec.name).unwrap_or_default();
            let mut reasons = Vec::new();
            if prerequisite.configured == Some(false) {
                reasons
                    .push("Main operation lacks setup; status/setup operations may remain usable");
            }
            if !self.inference_connected() {
                reasons.push("Inference is disconnected");
            }
            if policy == crate::ApprovalPolicyKind::Deny {
                reasons.push("Current permission policy denies calls");
            }
            match prepared {
                Some(false) => {
                    reasons.push("Absent from latest prepared request; may be deferred or filtered")
                }
                None => reasons
                    .push("No latest request evidence matching the active provider and model"),
                Some(true) => {}
            }
            rows.push(ToolCatalogRow {
                owners: owners(&spec.name, ContributionKind::Tool),
                aliases: tool
                    .aliases()
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                name: spec.name.clone(),
                kind: "client".into(),
                schema: Some(spec),
                prerequisite,
                route_selected: None,
                permission: permission.clone(),
                prepared,
                successful_calls: Some(successful_calls),
                last_successful_action,
                reason: reasons.join("; "),
            });
        }
        let registered_client_count = rows.len();
        for candidate in &candidates {
            let route = candidate.route();
            let selected = routes.as_ref().ok().map(|routes| {
                routes
                    .iter()
                    .any(|selected| selected.implementation() == candidate.id())
            });
            let compatible = route
                .provider()
                .is_none_or(|provider| provider == selection.provider_name)
                && candidate
                    .models()
                    .is_none_or(|models| models.contains(&selection.model));
            rows.push(ToolCatalogRow {
                name: candidate.id().into(), kind: format!("native/{:?}", route.kind()).to_lowercase(),
                owners: owners(candidate.id(), ContributionKind::NativeTool), aliases: Vec::new(), schema: None,
                prerequisite: heycode_tools::ToolPrerequisiteStatus { configured: None, detail: "Registration and route selection do not prove provider account entitlement or execution".into() },
                route_selected: selected, permission: permission.clone(),
                prepared: header.map(|header| header.options.native_tool_routes.iter().any(|prepared| prepared.implementation() == candidate.id())),
                successful_calls: Some(native_success.remove(candidate.id()).unwrap_or_default()), last_successful_action: None,
                reason: if !compatible { "Incompatible with the selected provider or model".into() }
                    else if selected == Some(false) { "Not selected by the active native tool policy".into() }
                    else { "Provider success counts require correlated request, server call and successful result events".into() },
            });
        }
        rows.sort_by(|left, right| (&left.kind, &left.name).cmp(&(&right.kind, &right.name)));
        Ok(ToolCatalogSnapshot {
            provider: selection.provider_name,
            model: selection.model,
            registered_client_count,
            registered_native_count: candidates.len(),
            prepared_client_count: header.map(|header| header.tools.len()),
            enabled_count: None,
            evidence: format!(
                "Prepared schemas are historical request metadata, not serialized-wire verification or current permission grants. Successful calls attest only their completed operations in this session. Enabled count remains unknown because permission and prerequisites can depend on arguments.{}",
                routes
                    .err()
                    .map(|error| format!(" Route resolution failed: {error}"))
                    .unwrap_or_default()
            ),
            tools: rows,
        })
    }
}
