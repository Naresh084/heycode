//! Atomic paginated tool generations.
//!
//! One generation is a complete `tools/list` page walk plus the token-owned
//! rows it registers. The walk is built while the previous generation is still
//! live; only a complete, unraced candidate replaces it. A list change, a
//! conflicting concurrent refresh or any mid-walk failure leaves exactly one
//! correct generation registered — never a partial one.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures::FutureExt as _;
use heycode_tools::{OwnedToolRegistration, Tool, ToolCtx, ToolError, ToolRegistry};
use tokio_util::sync::CancellationToken;

use crate::channel::{
    McpChannelError, McpRequestChannel, McpServerHandshake, McpSiblingContributions,
};
use crate::registry::{
    McpConnectionGeneration, McpConnectionPublisher, McpGenerationRetention, McpServerDefinition,
    McpServerId, McpToolPolicy,
};
use crate::{
    McpClientEventRouter, McpToolAdmission, McpToolAnnotations, McpToolApprovalDecision,
    McpToolApprovalHandler, McpToolApprovalRequest, resolve_mcp_tool_admission,
};

const MAX_CURSOR_BYTES: usize = 4 * 1024;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 16 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 128 * 1024;

/// One tool advertised by a server.
#[derive(Debug, Clone)]
pub struct McpToolDef {
    /// Tool name local to the server.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// JSON Schema for arguments.
    pub input_schema: serde_json::Value,
    /// Optional JSON Schema for `structuredContent`, retained so MCP12 can say
    /// whether a result conforms. Absent means the tool declared none, which is
    /// why a result with no structured content is unremarkable.
    ///
    /// <https://modelcontextprotocol.io/specification/2025-11-25/server/tools#output-schema>
    pub output_schema: Option<serde_json::Value>,
    /// Validated advisory server annotations. They never decide authority.
    pub annotations: McpToolAnnotations,
}

/// Monotonic marker that a server's tool list changed.
///
/// Transports mark it when a `notifications/tools/list_changed` arrives. A walk
/// that spans a mark is torn and must never be published.
#[derive(Debug, Clone, Default)]
pub struct McpListChangeWatch(Arc<AtomicU64>);

impl McpListChangeWatch {
    /// Build a watch at epoch zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the server's list changed.
    pub fn mark_changed(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Current change epoch; equality across a walk proves it was not torn.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Explicit bounds applied to one paginated listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpToolListLimits {
    max_pages: NonZeroU32,
    max_page_tools: NonZeroU32,
    max_tools: NonZeroU32,
}

impl McpToolListLimits {
    /// Replace the page budget for one walk.
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: NonZeroU32) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Replace the per-page row budget.
    #[must_use]
    pub const fn with_max_page_tools(mut self, max_page_tools: NonZeroU32) -> Self {
        self.max_page_tools = max_page_tools;
        self
    }

    /// Replace the total tool budget for one generation.
    #[must_use]
    pub const fn with_max_tools(mut self, max_tools: NonZeroU32) -> Self {
        self.max_tools = max_tools;
        self
    }
}

impl Default for McpToolListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroU32::new(64).unwrap_or(NonZeroU32::MIN),
            max_page_tools: NonZeroU32::new(512).unwrap_or(NonZeroU32::MIN),
            max_tools: NonZeroU32::new(2_048).unwrap_or(NonZeroU32::MIN),
        }
    }
}

struct LiveGeneration {
    definitions: Vec<McpToolDef>,
    channel: Arc<dyn McpRequestChannel>,
    registrations: Vec<OwnedToolRegistration>,
}

/// The single lifecycle owner of one server's tool generations.
///
/// Dropping the owner removes every registered row, even from a separately
/// held [`ToolRegistry`].
pub struct McpToolGenerationOwner {
    server: McpServerId,
    tools: Arc<ToolRegistry>,
    publisher: McpConnectionPublisher,
    watch: McpListChangeWatch,
    limits: McpToolListLimits,
    live: tokio::sync::Mutex<Option<LiveGeneration>>,
    /// Committed row names, readable without the async swap lane.
    names: std::sync::Mutex<Vec<String>>,
    /// Always present: every owner is built from an exact definition, so an
    /// allowlist or a deny row filters publication regardless of which host
    /// composed the connection. Only the approval *broker* is optional.
    policy: ResolvedToolPolicy,
}

struct ResolvedToolPolicy {
    policy: McpToolPolicy,
    approval: Option<Arc<dyn McpToolApprovalHandler>>,
    client_events: Option<McpClientEventRouter>,
    lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
}

impl McpToolGenerationOwner {
    /// Bind one server's registry publisher to the tool registry it feeds.
    ///
    /// The definition's tool policy is enforced at publication exactly as it is
    /// under [`Self::new_with_policy`]. Without an approval broker a `Prompt`
    /// admission proceeds to the host's own tool-approval gate rather than
    /// failing, because MCP approval is a second gate and not the only one;
    /// `Deny` and `Hidden` are refused here either way.
    #[must_use]
    pub fn new(
        definition: &McpServerDefinition,
        tools: Arc<ToolRegistry>,
        publisher: McpConnectionPublisher,
        watch: McpListChangeWatch,
        limits: McpToolListLimits,
    ) -> Self {
        Self {
            server: definition.id().clone(),
            tools,
            publisher,
            watch,
            limits,
            live: tokio::sync::Mutex::new(None),
            names: std::sync::Mutex::new(Vec::new()),
            policy: ResolvedToolPolicy {
                policy: definition.tool_policy().clone(),
                approval: None,
                client_events: None,
                lifecycle_hooks: None,
            },
        }
    }

    /// Bind the exact definition policy and action-time approval broker.
    ///
    /// Unlike [`Self::new`], this constructor enforces MCP13 at publication and
    /// action time: allowlist/deny rows filter the atomic candidate, explicit
    /// Allow bypasses only this MCP prompt (never higher host guards), and
    /// Prompt invokes `approval` before any server request.
    #[must_use]
    pub fn new_with_policy(
        definition: Arc<McpServerDefinition>,
        tools: Arc<ToolRegistry>,
        publisher: McpConnectionPublisher,
        watch: McpListChangeWatch,
        limits: McpToolListLimits,
        approval: Arc<dyn McpToolApprovalHandler>,
        client_events: Option<McpClientEventRouter>,
    ) -> Self {
        Self::new_with_product_policy(
            definition,
            tools,
            publisher,
            watch,
            limits,
            approval,
            client_events,
            None,
        )
    }

    /// Bind exact MCP11/MCP13 policy plus O09 lifecycle hooks.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_product_policy(
        definition: Arc<McpServerDefinition>,
        tools: Arc<ToolRegistry>,
        publisher: McpConnectionPublisher,
        watch: McpListChangeWatch,
        limits: McpToolListLimits,
        approval: Arc<dyn McpToolApprovalHandler>,
        client_events: Option<McpClientEventRouter>,
        lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
    ) -> Self {
        Self {
            server: definition.id().clone(),
            tools,
            publisher,
            watch,
            limits,
            live: tokio::sync::Mutex::new(None),
            names: std::sync::Mutex::new(Vec::new()),
            policy: ResolvedToolPolicy {
                policy: definition.tool_policy().clone(),
                approval: Some(approval),
                client_events,
                lifecycle_hooks,
            },
        }
    }

    /// Walk the complete paginated tool list and atomically replace the live
    /// generation.
    ///
    /// The walk runs while the previous rows stay live. Only a complete,
    /// unraced candidate swaps them; a failed swap restores the previous rows,
    /// and the registry generation advances exactly once, at publication.
    ///
    /// `siblings` carries what the resource and prompt walks of the same
    /// connection found. This owner publishes the connection's single
    /// generation, so it cannot honestly report those families as zero on their
    /// behalf; an advertised listing that was not walked is refused here.
    ///
    /// # Errors
    /// Transport/timeout/cancellation, a JSON-RPC error mid-walk, a protocol or
    /// budget violation, an advertised sibling listing that was never walked,
    /// and a conflict from a list change, a racing owner or a contested tool
    /// name.
    pub async fn refresh(
        &self,
        channel: Arc<dyn McpRequestChannel>,
        handshake: &McpServerHandshake,
        siblings: McpSiblingContributions,
        cancellation: &CancellationToken,
    ) -> Result<Arc<McpConnectionGeneration>, McpChannelError> {
        let mut live = self.live.lock().await;
        match self
            .build_and_swap(&mut live, channel, handshake, siblings, cancellation)
            .await
        {
            Ok(generation) => Ok(generation),
            Err((error, retention)) => {
                let _published = self.publisher.report_failure(
                    error.failure_code(),
                    crate::unix_time_ms(),
                    retention,
                );
                Err(error)
            }
        }
    }

    /// Retire the live generation, removing every registered row.
    ///
    /// Used when a bounded reconnect exhausts: the registry stops claiming a
    /// generation, so the rows it accounted for must not stay model-visible.
    /// Taking the swap lane means this can never tear an in-flight refresh.
    pub(crate) async fn retire(&self) {
        let mut live = self.live.lock().await;
        *live = None;
        self.commit_names(&live);
    }

    /// Committed tool row names in registration order.
    ///
    /// Reads the last committed generation, so it never blocks on an in-flight
    /// refresh and never observes a partially assembled candidate.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.names
            .lock()
            .map(|names| names.clone())
            .unwrap_or_default()
    }

    fn commit_names(&self, live: &Option<LiveGeneration>) {
        let committed = live.as_ref().map_or_else(Vec::new, |generation| {
            generation
                .registrations
                .iter()
                .map(|registration| registration.name().to_owned())
                .collect()
        });
        if let Ok(mut names) = self.names.lock() {
            *names = committed;
        }
    }

    async fn build_and_swap(
        &self,
        live: &mut Option<LiveGeneration>,
        channel: Arc<dyn McpRequestChannel>,
        handshake: &McpServerHandshake,
        siblings: McpSiblingContributions,
        cancellation: &CancellationToken,
    ) -> Result<Arc<McpConnectionGeneration>, (McpChannelError, McpGenerationRetention)> {
        let epoch = self.watch.epoch();
        let definitions = walk_tool_list(channel.as_ref(), self.limits, cancellation)
            .await
            .map_err(keep_last_good)?;
        let definitions = self.apply_policy(definitions);
        if cancellation.is_cancelled() {
            return Err(keep_last_good(McpChannelError::Cancelled));
        }
        if self.watch.epoch() != epoch {
            return Err(keep_last_good(McpChannelError::Conflict));
        }
        let count = u32::try_from(definitions.len()).map_err(|_| {
            keep_last_good(McpChannelError::protocol(
                "tool count exceeds the supported range",
            ))
        })?;
        let candidate = handshake
            .candidate(count, siblings, crate::unix_time_ms())
            .map_err(keep_last_good)?;

        // The registry rejects duplicate names, so the previous rows must be
        // released before the candidate can claim them. Everything needed to
        // rebuild them is retained until the swap commits.
        let previous = live
            .take()
            .map(|generation| (generation.definitions, generation.channel));
        let registrations = match self.register_all(&definitions, &channel) {
            Ok(registrations) => registrations,
            Err(error) => return Err(self.restore(live, previous, error)),
        };
        let generation = match self.publisher.publish_generation(candidate) {
            Ok(generation) => generation,
            Err(_) => {
                drop(registrations);
                self.commit_names(live);
                return Err((McpChannelError::Conflict, McpGenerationRetention::Remove));
            }
        };
        *live = Some(LiveGeneration {
            definitions,
            channel,
            registrations,
        });
        self.commit_names(live);
        Ok(generation)
    }

    fn restore(
        &self,
        live: &mut Option<LiveGeneration>,
        previous: Option<(Vec<McpToolDef>, Arc<dyn McpRequestChannel>)>,
        error: McpChannelError,
    ) -> (McpChannelError, McpGenerationRetention) {
        let Some((definitions, channel)) = previous else {
            return keep_last_good(error);
        };
        match self.register_all(&definitions, &channel) {
            Ok(registrations) => {
                *live = Some(LiveGeneration {
                    definitions,
                    channel,
                    registrations,
                });
                self.commit_names(live);
                keep_last_good(error)
            }
            // The previous rows are gone and cannot be rebuilt: the registry
            // must stop claiming a generation this owner no longer serves.
            Err(_) => {
                self.commit_names(live);
                (error, McpGenerationRetention::Remove)
            }
        }
    }

    fn register_all(
        &self,
        definitions: &[McpToolDef],
        channel: &Arc<dyn McpRequestChannel>,
    ) -> Result<Vec<OwnedToolRegistration>, McpChannelError> {
        let mut registrations = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let tool = qualified_tool(&self.server, Arc::clone(channel), definition, &self.policy);
            registrations.push(
                self.tools
                    .register_owned(tool)
                    .map_err(|_| McpChannelError::Conflict)?,
            );
        }
        Ok(registrations)
    }

    fn apply_policy(&self, definitions: Vec<McpToolDef>) -> Vec<McpToolDef> {
        let resolved = &self.policy;
        definitions
            .into_iter()
            .filter(|definition| {
                matches!(
                    resolve_mcp_tool_admission(
                        &resolved.policy,
                        &definition.name,
                        &definition.annotations,
                    ),
                    McpToolAdmission::Prompt | McpToolAdmission::Allow
                )
            })
            .collect()
    }
}

const fn keep_last_good(error: McpChannelError) -> (McpChannelError, McpGenerationRetention) {
    (error, McpGenerationRetention::KeepLastGood)
}

struct ToolPage {
    tools: Vec<McpToolDef>,
    next_cursor: Option<String>,
}

async fn walk_tool_list(
    channel: &dyn McpRequestChannel,
    limits: McpToolListLimits,
    cancellation: &CancellationToken,
) -> Result<Vec<McpToolDef>, McpChannelError> {
    let mut tools: Vec<McpToolDef> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut pages: u32 = 0;
    loop {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        pages = pages.saturating_add(1);
        if pages > limits.max_pages.get() {
            return Err(McpChannelError::protocol(
                "tool list exceeds the configured page budget",
            ));
        }
        let params = match &cursor {
            Some(cursor) => serde_json::json!({"cursor": cursor}),
            None => serde_json::json!({}),
        };
        let result = channel.call("tools/list", params, cancellation).await?;
        let page = parse_tool_page(&result, limits)?;
        if tools.len().saturating_add(page.tools.len()) > limits.max_tools.get() as usize {
            return Err(McpChannelError::protocol(
                "tool list exceeds the configured tool budget",
            ));
        }
        tools.extend(page.tools);
        // Absence or null ends the walk. An empty string is a valid cursor and
        // means more results follow.
        let Some(next) = page.next_cursor else {
            return Ok(tools);
        };
        if !visited.insert(next.clone()) {
            return Err(McpChannelError::protocol(
                "tool list repeated a pagination cursor",
            ));
        }
        cursor = Some(next);
    }
}

fn parse_tool_page(
    result: &serde_json::Value,
    limits: McpToolListLimits,
) -> Result<ToolPage, McpChannelError> {
    let result = result.as_object().ok_or(McpChannelError::protocol(
        "tools/list result must be an object",
    ))?;
    let rows = result
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or(McpChannelError::protocol(
            "tools/list result must contain a tools array",
        ))?;
    if rows.len() > limits.max_page_tools.get() as usize {
        return Err(McpChannelError::protocol(
            "tools/list page exceeds the configured page-size budget",
        ));
    }
    let mut tools = Vec::with_capacity(rows.len());
    for row in rows {
        tools.push(parse_tool(row)?);
    }
    let next_cursor = match result.get("nextCursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(cursor)) if cursor.len() <= MAX_CURSOR_BYTES => {
            Some(cursor.clone())
        }
        Some(_) => {
            return Err(McpChannelError::protocol(
                "nextCursor must be absent, null or a bounded opaque string",
            ));
        }
    };
    Ok(ToolPage { tools, next_cursor })
}

fn parse_tool(row: &serde_json::Value) -> Result<McpToolDef, McpChannelError> {
    let row = row.as_object().ok_or(McpChannelError::protocol(
        "tool definition must be an object",
    ))?;
    let name =
        row.get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpChannelError::protocol(
                "tool definition must carry a string name",
            ))?;
    if !valid_segment(name) {
        return Err(McpChannelError::protocol(
            "tool name must be 1..=64 ASCII alphanumeric, `_` or `-` bytes",
        ));
    }
    let description = row
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if description.len() > MAX_TOOL_DESCRIPTION_BYTES {
        return Err(McpChannelError::protocol(
            "tool description exceeds the 16384-byte bound",
        ));
    }
    let input_schema = match row.get("inputSchema") {
        None => serde_json::json!({"type": "object"}),
        Some(schema) if schema.is_object() => schema.clone(),
        Some(_) => {
            return Err(McpChannelError::protocol(
                "tool inputSchema must be a JSON object",
            ));
        }
    };
    if serde_json::to_vec(&input_schema).map_or(usize::MAX, |bytes| bytes.len())
        > MAX_TOOL_SCHEMA_BYTES
    {
        return Err(McpChannelError::protocol(
            "tool inputSchema exceeds the 131072-byte bound",
        ));
    }
    let output_schema = match row.get("outputSchema") {
        None => None,
        Some(schema) if schema.is_object() => Some(schema.clone()),
        Some(_) => {
            return Err(McpChannelError::protocol(
                "tool outputSchema must be absent or a JSON object",
            ));
        }
    };
    if output_schema.as_ref().map_or(0, |schema| {
        serde_json::to_vec(schema).map_or(usize::MAX, |bytes| bytes.len())
    }) > MAX_TOOL_SCHEMA_BYTES
    {
        return Err(McpChannelError::protocol(
            "tool outputSchema exceeds the 131072-byte bound",
        ));
    }
    let annotations = row.get("annotations").map_or_else(
        || Ok(McpToolAnnotations::default()),
        McpToolAnnotations::parse,
    )?;
    Ok(McpToolDef {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        output_schema,
        annotations,
    })
}

/// Validate a server/tool name segment: safe inside `mcp__x__y`.
pub(crate) fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, '_' | '-'))
}

fn qualified_tool(
    server: &McpServerId,
    channel: Arc<dyn McpRequestChannel>,
    definition: &McpToolDef,
    policy: &ResolvedToolPolicy,
) -> Arc<dyn Tool> {
    let admission =
        resolve_mcp_tool_admission(&policy.policy, &definition.name, &definition.annotations);
    Arc::new(McpQualifiedTool {
        qualified: format!("mcp__{}__{}", server.as_str(), definition.name),
        description: definition.description.clone(),
        schema: definition.input_schema.clone(),
        output_schema: definition.output_schema.clone(),
        remote_name: definition.name.clone(),
        server: server.clone(),
        channel,
        admission,
        approval: policy.approval.clone(),
        client_events: policy.client_events.clone(),
        lifecycle_hooks: policy.lifecycle_hooks.clone(),
    })
}

struct McpQualifiedTool {
    qualified: String,
    description: String,
    schema: serde_json::Value,
    output_schema: Option<serde_json::Value>,
    remote_name: String,
    server: McpServerId,
    channel: Arc<dyn McpRequestChannel>,
    admission: McpToolAdmission,
    approval: Option<Arc<dyn McpToolApprovalHandler>>,
    client_events: Option<McpClientEventRouter>,
    lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
}

impl McpQualifiedTool {
    async fn call_result(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<crate::results::McpToolResult, ToolError> {
        if let Some(hooks) = &self.lifecycle_hooks {
            let request = crate::McpLifecycleHookRequest::pre(
                self.server.clone(),
                self.remote_name.clone(),
                self.qualified.clone(),
                args.clone(),
            );
            let report =
                std::panic::AssertUnwindSafe(hooks.run(request, cx.cancellation.child_token()))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| crate::McpLifecycleHookReport::proceed_with_faults(1));
            if report.decision() == crate::McpLifecycleHookDecision::Refuse {
                return Err(ToolError::new("MCP tool call refused by a lifecycle hook"));
            }
        }
        // Publication already filtered `Deny`/`Hidden` out of the generation.
        // Refusing them again here is deliberate defence in depth: a row that
        // survives a racing refresh must still never reach the server.
        match self.admission {
            McpToolAdmission::Hidden | McpToolAdmission::Deny => {
                return Err(ToolError::new("MCP tool call denied by server/tool policy"));
            }
            McpToolAdmission::Prompt => {
                if let Some(approval) = self.approval.as_ref() {
                    if cx.cancellation.is_cancelled() {
                        return Err(ToolError::new("MCP tool approval was cancelled"));
                    }
                    let request = McpToolApprovalRequest::new(
                        self.server.clone(),
                        self.remote_name.clone(),
                        self.qualified.clone(),
                        args.clone(),
                    );
                    let decision = std::panic::AssertUnwindSafe(
                        approval.decide(request, cx.cancellation.clone()),
                    )
                    .catch_unwind()
                    .await
                    .map_err(|_| ToolError::new("MCP tool approval handler failed"))?;
                    if cx.cancellation.is_cancelled() {
                        return Err(ToolError::new("MCP tool approval was cancelled"));
                    }
                    if decision == McpToolApprovalDecision::Deny {
                        return Err(ToolError::new("MCP tool call denied by server/tool policy"));
                    }
                }
            }
            McpToolAdmission::Allow => {}
        }
        let progress = self
            .client_events
            .as_ref()
            .map(McpClientEventRouter::begin_progress);
        let mut params = serde_json::json!({"name": self.remote_name, "arguments": args});
        if let Some(progress) = &progress {
            params["_meta"] = serde_json::json!({
                "progressToken": progress.token().to_json()
            });
        }
        let result = self
            .channel
            .call("tools/call", params, &cx.cancellation)
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        let parsed = crate::results::McpToolResult::parse(&result, self.output_schema.as_ref())
            .map_err(|error| ToolError::new(error.to_string()))?;
        if let Some(hooks) = &self.lifecycle_hooks {
            let request = crate::McpLifecycleHookRequest::post(
                self.server.clone(),
                self.remote_name.clone(),
                self.qualified.clone(),
                parsed.render_for_model(),
                parsed.is_error(),
            );
            let _report =
                std::panic::AssertUnwindSafe(hooks.run(request, cx.cancellation.child_token()))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| crate::McpLifecycleHookReport::proceed_with_faults(1));
        }
        Ok(parsed)
    }
}

#[async_trait]
impl Tool for McpQualifiedTool {
    fn supports_background(&self) -> bool {
        true
    }

    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: self.qualified.clone(),
            description: self.description.clone(),
            parameters: self.schema.clone(),
        }
    }

    /// Every byte of an MCP tool result is authored by whoever runs the server
    /// and is destined for the model's context. WEB05's boundary is the exact
    /// vocabulary for that, and `UntrustedContentSource::Mcp` names this source
    /// rather than borrowing the web's.
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::mcp())
    }

    async fn run(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        let parsed = self.call_result(args, cx).await?;
        if parsed.is_error() {
            let rendered = parsed.render_for_model();
            return Err(ToolError::new(if rendered.is_empty() {
                format!("mcp tool `{}` failed", self.remote_name)
            } else {
                rendered
            }));
        }
        Ok(parsed.ui_value())
    }

    async fn run_output(
        &self,
        args: serde_json::Value,
        cx: &ToolCtx,
    ) -> Result<heycode_tools::ToolOutput, ToolError> {
        self.call_result(args, cx)
            .await?
            .to_tool_output()
            .map_err(|error| ToolError::new(error.to_string()))
    }
}
