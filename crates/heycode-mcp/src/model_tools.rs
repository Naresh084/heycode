//! Model-facing access to the MCP resource and connection registries.
//!
//! Transport owners bind one resource registry after the corresponding MCP
//! generation is fully established. The three tools below therefore observe
//! exactly the same atomic listings, read channel and lifecycle state as the
//! human-facing MCP surfaces; they do not create a second client or infer
//! readiness from configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::ToolSpec;
use heycode_tools::{OwnedToolRegistration, Tool, ToolCtx, ToolEffect, ToolError, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::resources::{McpResourceBody, McpResourceRegistry};
use crate::{McpConnectionState, McpRegistry, McpServerId};

pub(crate) const MODEL_RESOURCE_TOOL_NAMES: [&str; 3] = [
    "list_mcp_resources",
    "read_mcp_resource",
    "wait_for_mcp_servers",
];

const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 100;
const MAX_MODEL_TEXT_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_URI_BYTES: usize = 2 * 1024;
const DEFAULT_WAIT_MS: u64 = 30_000;
const MAX_WAIT_MS: u64 = 30_000;
const POLL_INTERVAL_MS: u64 = 25;

struct BoundResourceRegistry {
    token: Arc<()>,
    registry: Arc<McpResourceRegistry>,
    exposed: bool,
}

/// Session-local join between the public connection registry and each live
/// connection's resource registry.
pub(crate) struct ModelResourceHub {
    registry: McpRegistry,
    resources: Arc<Mutex<BTreeMap<String, BoundResourceRegistry>>>,
    registrations: Mutex<Vec<SharedToolRegistration>>,
}

impl ModelResourceHub {
    pub(crate) fn new(registry: McpRegistry) -> Arc<Self> {
        Arc::new(Self {
            registry,
            resources: Arc::new(Mutex::new(BTreeMap::new())),
            registrations: Mutex::new(Vec::new()),
        })
    }

    /// One registration set per tool registry, leased by every transport owner.
    /// The registration mutex serializes the last release with a new acquire,
    /// so an old owner's Drop cannot remove a replacement generation.
    pub(crate) fn acquire_tools(
        self: &Arc<Self>,
        tools: &Arc<ToolRegistry>,
    ) -> Result<ModelResourceToolOwner, heycode_tools::RegisterError> {
        let mut registrations = self
            .registrations
            .lock()
            .map_err(|_| heycode_tools::RegisterError::RegistryUnavailable)?;
        if let Some(shared) = registrations.iter_mut().find(|shared| {
            shared
                .tools
                .upgrade()
                .is_some_and(|registered| Arc::ptr_eq(&registered, tools))
        }) {
            shared.owners = shared
                .owners
                .checked_add(1)
                .ok_or(heycode_tools::RegisterError::RegistryUnavailable)?;
            return Ok(ModelResourceToolOwner {
                hub: Arc::clone(self),
                token: Arc::clone(&shared.token),
            });
        }
        let owned = self.register_tools(tools)?;
        let token = Arc::new(());
        registrations.push(SharedToolRegistration {
            tools: Arc::downgrade(tools),
            token: Arc::clone(&token),
            owners: 1,
            owned,
        });
        Ok(ModelResourceToolOwner {
            hub: Arc::clone(self),
            token,
        })
    }

    pub(crate) fn register_tools(
        self: &Arc<Self>,
        tools: &ToolRegistry,
    ) -> Result<Vec<OwnedToolRegistration>, heycode_tools::RegisterError> {
        [
            Arc::new(ListMcpResources {
                hub: Arc::clone(self),
            }) as Arc<dyn Tool>,
            Arc::new(ReadMcpResource {
                hub: Arc::clone(self),
            }),
            Arc::new(WaitForMcpServers {
                hub: Arc::clone(self),
            }),
        ]
        .into_iter()
        .map(|tool| tools.register_owned(tool))
        .collect()
    }

    pub(crate) fn bind(
        self: &Arc<Self>,
        server: String,
        registry: Arc<McpResourceRegistry>,
        exposed: bool,
    ) -> Result<ModelResourceBinding, &'static str> {
        let token = Arc::new(());
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| "MCP resource access registry is unavailable")?;
        if resources.contains_key(&server) {
            return Err("MCP resource access already has a live server binding");
        }
        resources.insert(
            server.clone(),
            BoundResourceRegistry {
                token: Arc::clone(&token),
                registry,
                exposed,
            },
        );
        Ok(ModelResourceBinding {
            resources: Arc::downgrade(&self.resources),
            server,
            token,
        })
    }

    fn bound(&self, server: &str) -> Result<Option<(Arc<McpResourceRegistry>, bool)>, ToolError> {
        let resources = self
            .resources
            .lock()
            .map_err(|_| ToolError::new("MCP resource access registry is unavailable"))?;
        Ok(resources
            .get(server)
            .map(|row| (Arc::clone(&row.registry), row.exposed)))
    }

    fn snapshot(&self) -> Result<Arc<crate::McpSnapshot>, ToolError> {
        self.registry
            .snapshot()
            .map_err(|_| ToolError::new("MCP connection registry is unavailable"))
    }
}

struct SharedToolRegistration {
    tools: Weak<ToolRegistry>,
    token: Arc<()>,
    owners: usize,
    owned: Vec<OwnedToolRegistration>,
}

/// Context-owned lease on the shared resource tools. Connection bindings keep
/// their own tokens; releasing one plugin never hides another plugin's servers.
pub(crate) struct ModelResourceToolOwner {
    hub: Arc<ModelResourceHub>,
    token: Arc<()>,
}

impl ModelResourceToolOwner {
    pub(crate) fn hub(&self) -> Arc<ModelResourceHub> {
        Arc::clone(&self.hub)
    }
}

impl Drop for ModelResourceToolOwner {
    fn drop(&mut self) {
        let Ok(mut registrations) = self.hub.registrations.lock() else {
            return;
        };
        let Some(index) = registrations
            .iter()
            .position(|shared| Arc::ptr_eq(&shared.token, &self.token))
        else {
            return;
        };
        let shared = &mut registrations[index];
        shared.owners -= 1;
        if shared.owners == 0 {
            // Retire while holding the acquisition lock, before another owner
            // can attempt to register this same tool set again.
            shared.owned.clear();
            registrations.remove(index);
        }
    }
}

/// Token-matched lifetime for one live resource binding.
pub(crate) struct ModelResourceBinding {
    resources: Weak<Mutex<BTreeMap<String, BoundResourceRegistry>>>,
    server: String,
    token: Arc<()>,
}

impl Drop for ModelResourceBinding {
    fn drop(&mut self) {
        let Some(resources) = self.resources.upgrade() else {
            return;
        };
        let Ok(mut resources) = resources.lock() else {
            return;
        };
        let remove = resources
            .get(&self.server)
            .is_some_and(|row| Arc::ptr_eq(&row.token, &self.token));
        if remove {
            resources.remove(&self.server);
        }
    }
}

struct ListMcpResources {
    hub: Arc<ModelResourceHub>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    server: Option<String>,
    #[serde(default = "default_page_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<ListCursor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListCursor {
    kind: String,
    offset: usize,
    #[serde(default)]
    registry_revision: Option<u64>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    resource_generation: Option<u64>,
}

const fn default_page_limit() -> usize {
    DEFAULT_PAGE_LIMIT
}

#[async_trait]
impl Tool for ListMcpResources {
    fn prerequisite_status(&self) -> heycode_tools::ToolPrerequisiteStatus {
        heycode_tools::ToolPrerequisiteStatus {
            configured: Some(true),
            detail: "MCP server discovery remains available even when no servers are configured; individual resource listings still require an established exposed server".into(),
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MODEL_RESOURCE_TOOL_NAMES[0].into(),
            description: "List configured MCP servers, or list one server's committed resources. Omit server first to inspect readiness, then pass its exact id. Follow continuation objects without editing them.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "description": "Exact MCP server id. Omit to list server summaries."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_PAGE_LIMIT, "default": DEFAULT_PAGE_LIMIT},
                    "cursor": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string", "enum": ["servers", "resources"]},
                            "offset": {"type": "integer", "minimum": 0},
                            "registry_revision": {"type": "integer", "minimum": 0},
                            "server": {"type": "string"},
                            "resource_generation": {"type": "integer", "minimum": 0}
                        },
                        "required": ["kind", "offset"],
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::mcp())
    }

    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        let args: ListArgs = parse_args(args)?;
        validate_limit(args.limit)?;
        match args.server {
            Some(server) => {
                let server = validate_server_id(server)?;
                self.list_resources(&server, args.limit, args.cursor)
            }
            None => self.list_servers(args.limit, args.cursor),
        }
    }
}

impl ListMcpResources {
    fn list_servers(&self, limit: usize, cursor: Option<ListCursor>) -> Result<Value, ToolError> {
        let snapshot = self.hub.snapshot()?;
        let offset = match cursor {
            None => 0,
            Some(cursor) => {
                if cursor.kind != "servers"
                    || cursor.server.is_some()
                    || cursor.resource_generation.is_some()
                    || cursor.registry_revision != Some(snapshot.revision())
                {
                    return Err(ToolError::new(
                        "MCP server continuation is stale or does not match this listing; restart without cursor",
                    ));
                }
                cursor.offset
            }
        };
        if offset > snapshot.servers().len() {
            return Err(ToolError::new(
                "MCP server continuation offset is outside the current listing",
            ));
        }
        let end = offset.saturating_add(limit).min(snapshot.servers().len());
        let mut rows = Vec::with_capacity(end.saturating_sub(offset));
        for server in &snapshot.servers()[offset..end] {
            let id = server.definition().id().as_str();
            let bound = self.hub.bound(id)?;
            let (resource_generation, resource_count) = bound
                .as_ref()
                .and_then(|(registry, exposed)| {
                    exposed
                        .then(|| registry.inspect().generation().cloned())
                        .flatten()
                })
                .map_or((None, None), |generation| {
                    (
                        Some(generation.number()),
                        Some(generation.resources().len()),
                    )
                });
            rows.push(json!({
                "server": id,
                "state": connection_state_name(server.state()),
                "authentication": serde_json::to_value(server.authentication()).unwrap_or(Value::Null),
                "resources_exposed": server.definition().exposure().resources,
                "resource_generation": resource_generation,
                "resource_count": resource_count,
            }));
        }
        let continuation = (end < snapshot.servers().len()).then(|| {
            json!({
                "kind": "servers",
                "offset": end,
                "registry_revision": snapshot.revision(),
            })
        });
        Ok(json!({
            "registry_revision": snapshot.revision(),
            "servers": rows,
            "returned": rows.len(),
            "total": snapshot.servers().len(),
            "continuation": continuation,
        }))
    }

    fn list_resources(
        &self,
        server: &str,
        limit: usize,
        cursor: Option<ListCursor>,
    ) -> Result<Value, ToolError> {
        let snapshot = self.hub.snapshot()?;
        let server_row = find_server(&snapshot, server)?;
        if !server_row.definition().exposure().resources {
            return Err(ToolError::new(format!(
                "MCP server `{server}` does not expose resources to model tools"
            )));
        }
        let (registry, exposed) = self.hub.bound(server)?.ok_or_else(|| {
            ToolError::new(format!(
                "MCP server `{server}` has no established resource connection (state: {})",
                connection_state_name(server_row.state())
            ))
        })?;
        if !exposed {
            return Err(ToolError::new(format!(
                "MCP server `{server}` does not expose resources to model tools"
            )));
        }
        let inspection = registry.inspect();
        let generation = inspection.generation().ok_or_else(|| {
            ToolError::new(format!(
                "MCP server `{server}` did not publish a resource listing"
            ))
        })?;
        let offset = match cursor {
            None => 0,
            Some(cursor) => {
                if cursor.kind != "resources"
                    || cursor.server.as_deref() != Some(server)
                    || cursor.resource_generation != Some(generation.number())
                    || cursor.registry_revision.is_some()
                {
                    return Err(ToolError::new(
                        "MCP resource continuation is stale or does not match this server; restart without cursor",
                    ));
                }
                cursor.offset
            }
        };
        if offset > generation.resources().len() {
            return Err(ToolError::new(
                "MCP resource continuation offset is outside the current listing",
            ));
        }
        let end = offset
            .saturating_add(limit)
            .min(generation.resources().len());
        let rows: Vec<_> = generation.resources()[offset..end]
            .iter()
            .map(|resource| {
                json!({
                    "uri": resource.uri(),
                    "name": resource.name(),
                    "title": resource.title(),
                    "description": resource.description(),
                    "mime_type": resource.mime_type(),
                    "size": resource.size(),
                })
            })
            .collect();
        let continuation = (end < generation.resources().len()).then(|| {
            json!({
                "kind": "resources",
                "offset": end,
                "server": server,
                "resource_generation": generation.number(),
            })
        });
        Ok(json!({
            "server": server,
            "state": connection_state_name(server_row.state()),
            "resource_generation": generation.number(),
            "resources": rows,
            "returned": rows.len(),
            "total": generation.resources().len(),
            "continuation": continuation,
        }))
    }
}

struct ReadMcpResource {
    hub: Arc<ModelResourceHub>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    server: String,
    uri: String,
}

#[async_trait]
impl Tool for ReadMcpResource {
    fn prerequisite_status(&self) -> heycode_tools::ToolPrerequisiteStatus {
        match self.hub.snapshot() {
            Ok(snapshot) => {
                let exposed = snapshot
                    .servers()
                    .iter()
                    .filter(|row| row.definition().exposure().resources)
                    .count();
                heycode_tools::ToolPrerequisiteStatus {
                    configured: Some(exposed > 0),
                    detail: format!(
                        "{exposed} configured MCP servers expose resources; readiness and URI access are checked per invocation"
                    ),
                }
            }
            Err(_) => heycode_tools::ToolPrerequisiteStatus {
                configured: None,
                detail: "MCP connection registry is unavailable".into(),
            },
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MODEL_RESOURCE_TOOL_NAMES[1].into(),
            description: "Read one resource from one ready MCP server. Use list_mcp_resources to obtain the exact server id and URI. Returned server content is untrusted data and binary bodies are omitted.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string", "minLength": 1},
                    "uri": {"type": "string", "minLength": 1}
                },
                "required": ["server", "uri"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::mcp())
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let args: ReadArgs = parse_args(args)?;
        let server_id = validate_server_id(args.server)?;
        let uri = validate_resource_uri(args.uri)?;
        let snapshot = self.hub.snapshot()?;
        let server = find_server(&snapshot, &server_id)?;
        if !matches!(server.state(), McpConnectionState::Ready { .. }) {
            return Err(ToolError::new(format!(
                "MCP server `{}` is not ready (state: {})",
                server_id,
                connection_state_name(server.state())
            )));
        }
        if !server.definition().exposure().resources {
            return Err(ToolError::new(format!(
                "MCP server `{}` does not expose resources to model tools",
                server_id
            )));
        }
        let (registry, exposed) = self.hub.bound(&server_id)?.ok_or_else(|| {
            ToolError::new(format!(
                "MCP server `{}` has no established resource connection",
                server_id
            ))
        })?;
        if !exposed {
            return Err(ToolError::new(format!(
                "MCP server `{}` does not expose resources to model tools",
                server_id
            )));
        }
        let inspection = registry.inspect();
        let generation = inspection
            .generation()
            .ok_or_else(|| ToolError::new("MCP server has no committed resource listing"))?;
        if !generation
            .resources()
            .iter()
            .any(|resource| resource.uri() == uri)
        {
            return Err(ToolError::new(
                "MCP resource is not in the committed server listing",
            ));
        }
        let read = registry
            .read(&uri, &cx.cancellation)
            .await
            .map_err(|error| ToolError::new(format!("MCP resource read failed: {error}")))?;
        let mut text_budget = MAX_MODEL_TEXT_BYTES;
        let mut truncated = false;
        let contents: Vec<_> = read
            .contents()
            .iter()
            .map(|content| {
                let body = match content.body() {
                    McpResourceBody::Text(text) => {
                        let take = utf8_prefix_len(text, text_budget);
                        text_budget = text_budget.saturating_sub(take);
                        if take < text.len() {
                            truncated = true;
                        }
                        json!({"kind": "text", "text": &text[..take]})
                    }
                    McpResourceBody::Blob(bytes) => {
                        json!({"kind": "binary_omitted", "bytes": bytes.len()})
                    }
                };
                json!({
                    "uri": content.uri(),
                    "mime_type": content.mime_type(),
                    "body": body,
                })
            })
            .collect();
        Ok(json!({
            "server": server_id,
            "uri": read.uri(),
            "contents": contents,
            "body_bytes": read.body_bytes(),
            "text_preview_limit_bytes": MAX_MODEL_TEXT_BYTES,
            "truncated": truncated,
        }))
    }
}

fn validate_resource_uri(uri: String) -> Result<String, ToolError> {
    if uri.is_empty()
        || uri.len() > MAX_RESOURCE_URI_BYTES
        || uri.trim() != uri
        || uri.chars().any(char::is_control)
    {
        return Err(ToolError::new("MCP resource URI is invalid"));
    }
    Ok(uri)
}

struct WaitForMcpServers {
    hub: Arc<ModelResourceHub>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    #[serde(default)]
    servers: Option<Vec<String>>,
    #[serde(default = "default_wait_ms")]
    timeout_ms: u64,
}

const fn default_wait_ms() -> u64 {
    DEFAULT_WAIT_MS
}

#[async_trait]
impl Tool for WaitForMcpServers {
    fn prerequisite_status(&self) -> heycode_tools::ToolPrerequisiteStatus {
        heycode_tools::ToolPrerequisiteStatus {
            configured: Some(true),
            detail: "MCP readiness waiting remains available with an explicit no_servers outcome when no servers are configured".into(),
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: MODEL_RESOURCE_TOOL_NAMES[2].into(),
            description: "Wait up to 30 seconds for selected MCP servers to become ready or reach a non-ready settled state. Omit servers to wait for every configured server. Authentication and failure states are never reported as ready.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "servers": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1},
                        "maxItems": 100,
                        "uniqueItems": true
                    },
                    "timeout_ms": {"type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS, "default": DEFAULT_WAIT_MS}
                },
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let args: WaitArgs = parse_args(args)?;
        if args.timeout_ms > MAX_WAIT_MS {
            return Err(ToolError::new("timeout_ms must be between 0 and 30000"));
        }
        let initial = self.hub.snapshot()?;
        let selected: BTreeSet<String> = match args.servers {
            Some(servers) => {
                if servers.len() > 100 {
                    return Err(ToolError::new("servers must contain at most 100 ids"));
                }
                servers
                    .into_iter()
                    .map(validate_server_id)
                    .collect::<Result<BTreeSet<_>, _>>()?
            }
            None => initial
                .servers()
                .iter()
                .map(|row| row.definition().id().as_str().to_owned())
                .collect(),
        };
        let known: BTreeSet<&str> = initial
            .servers()
            .iter()
            .map(|row| row.definition().id().as_str())
            .collect();
        let unknown: Vec<_> = selected
            .iter()
            .filter(|name| !known.contains(name.as_str()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(ToolError::new(format!(
                "Unknown MCP server ids: {}",
                unknown.join(", ")
            )));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_millis(args.timeout_ms);
        loop {
            let snapshot = self.hub.snapshot()?;
            let outcome = wait_outcome(&snapshot, &selected);
            if outcome != "pending" {
                return Ok(wait_value(outcome, &snapshot, &selected));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(wait_value("timed_out", &snapshot, &selected));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let delay = remaining.min(Duration::from_millis(POLL_INTERVAL_MS));
            tokio::select! {
                () = cx.cancellation.cancelled() => {
                    return Err(ToolError::new("MCP readiness wait was cancelled"));
                }
                () = tokio::time::sleep(delay) => {}
            }
        }
    }
}

fn wait_outcome(snapshot: &crate::McpSnapshot, selected: &BTreeSet<String>) -> &'static str {
    if selected.is_empty() {
        return "no_servers";
    }
    let mut all_ready = true;
    let mut pending = false;
    let mut non_ready_settled = false;
    let mut matched = 0_usize;
    for row in snapshot
        .servers()
        .iter()
        .filter(|row| selected.contains(row.definition().id().as_str()))
    {
        matched = matched.saturating_add(1);
        match row.state() {
            McpConnectionState::Ready { .. } => {}
            McpConnectionState::Starting { .. } | McpConnectionState::Reconnecting { .. } => {
                all_ready = false;
                pending = true;
            }
            McpConnectionState::Disabled
            | McpConnectionState::Inactive
            | McpConnectionState::AuthenticationRequired { .. }
            | McpConnectionState::Degraded { .. }
            | McpConnectionState::Failed { .. } => {
                all_ready = false;
                non_ready_settled = true;
            }
        }
    }
    if matched != selected.len() || non_ready_settled {
        return "settled";
    }
    if all_ready {
        "all_ready"
    } else if pending {
        "pending"
    } else {
        "settled"
    }
}

fn wait_value(outcome: &str, snapshot: &crate::McpSnapshot, selected: &BTreeSet<String>) -> Value {
    let states: Vec<_> = snapshot
        .servers()
        .iter()
        .filter(|row| selected.contains(row.definition().id().as_str()))
        .map(|row| {
            json!({
                "server": row.definition().id().as_str(),
                "state": connection_state_name(row.state()),
                "authentication": serde_json::to_value(row.authentication()).unwrap_or(Value::Null),
                "ready": matches!(row.state(), McpConnectionState::Ready { .. }),
            })
        })
        .collect();
    json!({
        "outcome": outcome,
        "all_ready": outcome == "all_ready",
        "registry_revision": snapshot.revision(),
        "servers": states,
    })
}

fn find_server<'a>(
    snapshot: &'a crate::McpSnapshot,
    server: &str,
) -> Result<&'a crate::McpServerSnapshot, ToolError> {
    snapshot
        .servers()
        .iter()
        .find(|row| row.definition().id().as_str() == server)
        .ok_or_else(|| ToolError::new(format!("Unknown MCP server id `{server}`")))
}

fn connection_state_name(state: &McpConnectionState) -> &'static str {
    match state {
        McpConnectionState::Disabled => "disabled",
        McpConnectionState::Inactive => "inactive",
        McpConnectionState::Starting { .. } => "starting",
        McpConnectionState::Ready { .. } => "ready",
        McpConnectionState::AuthenticationRequired { .. } => "authentication_required",
        McpConnectionState::Reconnecting { .. } => "reconnecting",
        McpConnectionState::Degraded { .. } => "degraded",
        McpConnectionState::Failed { .. } => "failed",
    }
}

fn validate_limit(limit: usize) -> Result<(), ToolError> {
    if (1..=MAX_PAGE_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(ToolError::new("limit must be between 1 and 100"))
    }
}

fn validate_server_id(server: String) -> Result<String, ToolError> {
    McpServerId::new(server)
        .map(|server| server.as_str().to_owned())
        .map_err(|_| ToolError::new("MCP server id must be a valid 1..=64 byte namespace"))
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|_| ToolError::new("Invalid MCP tool arguments"))
}

fn utf8_prefix_len(text: &str, budget: usize) -> usize {
    let mut end = text.len().min(budget);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use heycode_core::Context;
    use heycode_tools::ToolCtx;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        McpCapabilitySet, McpConnectionProviderId, McpContributionCounts, McpDefinitionScope,
        McpExposurePolicy, McpFailureCode, McpGenerationCandidate, McpGenerationRetention,
        McpServerDefinition, McpStreamableHttpTransport, McpTransportDefinition,
    };

    fn pending_world(
        resources_exposed: bool,
    ) -> (
        Context,
        Context,
        Arc<ToolRegistry>,
        crate::McpConnectionPublisher,
        Vec<OwnedToolRegistration>,
    ) {
        let registry = McpRegistry::new();
        let definition_owner = Context::new();
        let definition = McpServerDefinition::new(
            "pending",
            "Pending fixture",
            McpDefinitionScope::User,
            McpTransportDefinition::StreamableHttp(
                McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new())
                    .unwrap(),
            ),
        )
        .unwrap()
        .with_exposure(McpExposurePolicy {
            resources: resources_exposed,
            prompts: false,
            instructions: false,
        });
        registry
            .register_definition(&definition_owner, definition)
            .unwrap();
        let connection_owner = Context::new();
        let publisher = registry
            .register_connection(
                &connection_owner,
                &McpServerId::new("pending").unwrap(),
                McpConnectionProviderId::new("streamable-http").unwrap(),
                1,
            )
            .unwrap();
        let tools = Arc::new(ToolRegistry::new());
        let hub = ModelResourceHub::new(registry);
        let registrations = hub.register_tools(&tools).unwrap();
        (
            definition_owner,
            connection_owner,
            tools,
            publisher,
            registrations,
        )
    }

    #[test]
    fn shared_tool_leases_survive_one_owner_and_retire_only_the_last_owner() {
        let registry = McpRegistry::new();
        let tools = Arc::new(ToolRegistry::new());
        let first = registry.model_resource_tools(&tools).unwrap();
        let first_hub = first.hub();
        let second = registry.clone().model_resource_tools(&tools).unwrap();
        assert!(Arc::ptr_eq(&first_hub, &second.hub()));
        let retained_tool = tools.get("list_mcp_resources").unwrap();
        drop(first);
        assert!(Arc::ptr_eq(
            &retained_tool,
            &tools.get("list_mcp_resources").unwrap()
        ));
        for name in MODEL_RESOURCE_TOOL_NAMES {
            assert!(tools.get(name).is_some());
        }
        drop(second);
        for name in MODEL_RESOURCE_TOOL_NAMES {
            assert!(tools.get(name).is_none());
        }

        // A retained in-flight tool keeps the hub readable, but does not keep
        // registrations published or prevent a later activation.
        let replacement = registry.model_resource_tools(&tools).unwrap();
        assert!(Arc::ptr_eq(&first_hub, &replacement.hub()));
        let replacement_tool = tools.get("list_mcp_resources").unwrap();
        assert!(!Arc::ptr_eq(&retained_tool, &replacement_tool));
        drop(retained_tool);
        assert!(Arc::ptr_eq(
            &replacement_tool,
            &tools.get("list_mcp_resources").unwrap()
        ));
        drop(replacement);
        for name in MODEL_RESOURCE_TOOL_NAMES {
            assert!(tools.get(name).is_none());
        }
    }

    #[test]
    fn shared_registration_conflicts_roll_back_without_adopting_unrelated_tools() {
        let registry = McpRegistry::new();
        let tools = Arc::new(ToolRegistry::new());
        let foreign = tools
            .register_owned(Arc::new(ReadMcpResource {
                hub: ModelResourceHub::new(McpRegistry::new()),
            }))
            .unwrap();
        assert!(
            matches!(registry.model_resource_tools(&tools), Err(heycode_tools::RegisterError::Duplicate(name)) if name == "read_mcp_resource")
        );
        assert!(
            tools.get("list_mcp_resources").is_none(),
            "partial registration rolled back"
        );
        assert!(tools.get("wait_for_mcp_servers").is_none());
        assert!(
            tools.get("read_mcp_resource").is_some(),
            "unrelated owner's tool survives"
        );
        drop(foreign);
        let owner = registry.model_resource_tools(&tools).unwrap();
        for name in MODEL_RESOURCE_TOOL_NAMES {
            assert!(tools.get(name).is_some());
        }
        drop(owner);
        for name in MODEL_RESOURCE_TOOL_NAMES {
            assert!(tools.get(name).is_none());
        }
    }

    #[tokio::test]
    async fn readiness_wait_distinguishes_timeout_settlement_ready_and_cancellation() {
        assert_eq!(
            wait_outcome(
                &McpRegistry::new().snapshot().unwrap(),
                &BTreeSet::from(["vanished".to_owned()])
            ),
            "settled",
            "a selected server that disappears mid-wait is never reported ready"
        );
        let (mut definition_owner, mut connection_owner, tools, publisher, registrations) =
            pending_world(true);
        let wait = tools.get("wait_for_mcp_servers").unwrap();
        let timed_out = wait
            .run(
                json!({"servers":["pending"],"timeout_ms":0}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        assert_eq!(timed_out["outcome"], "timed_out");
        assert_eq!(timed_out["servers"][0]["state"], "starting");

        let cancellation = CancellationToken::new();
        let waiting = {
            let wait = wait.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                wait.run(
                    json!({"servers":["pending"],"timeout_ms":30000}),
                    &ToolCtx {
                        cwd: std::path::PathBuf::from("."),
                        cancellation,
                    },
                )
                .await
            })
        };
        tokio::task::yield_now().await;
        cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("readiness cancellation must settle")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.message, "MCP readiness wait was cancelled");

        publisher
            .report_failure(McpFailureCode::Transport, 2, McpGenerationRetention::Remove)
            .unwrap();
        let settled = wait
            .run(
                json!({"servers":["pending"],"timeout_ms":30000}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        assert_eq!(settled["outcome"], "settled");
        assert_eq!(settled["all_ready"], false);

        publisher
            .publish_generation(
                McpGenerationCandidate::new(
                    "2025-11-25",
                    "fixture",
                    "1",
                    McpCapabilitySet {
                        resources: true,
                        ..McpCapabilitySet::default()
                    },
                    McpContributionCounts {
                        resources: 0,
                        ..McpContributionCounts::default()
                    },
                    3,
                )
                .unwrap(),
            )
            .unwrap();
        let ready = wait
            .run(
                json!({"servers":["pending"],"timeout_ms":0}),
                &ToolCtx::default(),
            )
            .await
            .unwrap();
        assert_eq!(ready["outcome"], "all_ready");
        assert_eq!(ready["all_ready"], true);

        drop(registrations);
        assert!(tools.get("wait_for_mcp_servers").is_none());
        connection_owner.shutdown();
        definition_owner.shutdown();
    }

    #[tokio::test]
    async fn unexposed_resources_never_reach_listing_or_read_channels() {
        let (mut definition_owner, mut connection_owner, tools, publisher, registrations) =
            pending_world(false);
        publisher
            .publish_generation(
                McpGenerationCandidate::new(
                    "2025-11-25",
                    "fixture",
                    "1",
                    McpCapabilitySet {
                        resources: true,
                        ..McpCapabilitySet::default()
                    },
                    McpContributionCounts {
                        resources: 1,
                        ..McpContributionCounts::default()
                    },
                    3,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            tools
                .get("read_mcp_resource")
                .unwrap()
                .prerequisite_status()
                .configured,
            Some(false)
        );
        for (name, args) in [
            ("list_mcp_resources", json!({"server":"pending","limit":1})),
            (
                "read_mcp_resource",
                json!({"server":"pending","uri":"fixture://private"}),
            ),
        ] {
            let error = tools
                .get(name)
                .unwrap()
                .run(args, &ToolCtx::default())
                .await
                .unwrap_err();
            assert_eq!(
                error.message,
                "MCP server `pending` does not expose resources to model tools"
            );
        }
        drop(registrations);
        connection_owner.shutdown();
        definition_owner.shutdown();
    }
}
