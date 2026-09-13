//! A08 provider-independent deferred-tool and Code Mode scheduling.
//!
//! A deferred provider selects membership, never executes a tool. The Agent
//! applies that selection to both model-facing [`heycode_core::ToolSpec`] rows and
//! N01 [`heycode_core::NativeToolRoute`] rows before dispatch. Any tool call the
//! model later emits still crosses A06 approval, execution and ordered durable
//! commit.
//!
//! Code Mode has the same boundary in the other direction: a provider may
//! materialize explicit ordinary tool-call stream chunks, but this module has
//! no tool registry or execution handle. Feeding those chunks through the
//! Agent is the only path that can run them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

use heycode_core::{NativeToolImplementationKind, NativeToolRoute, ToolSpec};
use tokio_util::sync::CancellationToken;

const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_SELECTIONS: usize = 256;
const MAX_CATALOG_ENTRIES: usize = 16_384;
const MAX_CODE_MODE_CALLS: usize = 256;

/// Validated identity of one deferred-tool selection provider.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeferredToolProviderId(String);

impl DeferredToolProviderId {
    /// Validate one lowercase kebab-case provider id.
    ///
    /// # Errors
    /// Empty, oversized, non-kebab-case, or control-bearing ids fail.
    pub fn new(value: impl Into<String>) -> Result<Self, DeferredToolError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_PROVIDER_ID_BYTES
            || value.starts_with('-')
            || value.ends_with('-')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(DeferredToolError::InvalidProviderId);
        }
        Ok(Self(value))
    }

    /// Stable provider id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How one manifest row would be reached if selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredToolKind {
    /// Ordinary client tool executed by A06.
    Client,
    /// Provider-native capability selected by N01.
    ProviderNative,
    /// MCP-backed local capability selected by N01.
    Mcp,
}

impl DeferredToolKind {
    /// Whether this row is executed by the inference provider.
    #[must_use]
    pub const fn is_provider_native(self) -> bool {
        matches!(self, Self::ProviderNative)
    }
}

/// One bounded catalog row handed to a deferred selection provider.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredToolEntry {
    name: String,
    description: String,
    parameters: Option<serde_json::Value>,
    kind: DeferredToolKind,
}

impl DeferredToolEntry {
    /// Stable logical/model-facing name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Model-facing description when a client schema exists.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Parameter schema when this catalog row has a client tool declaration.
    #[must_use]
    pub const fn parameters(&self) -> Option<&serde_json::Value> {
        self.parameters.as_ref()
    }

    /// Route family selected by N01.
    #[must_use]
    pub const fn kind(&self) -> DeferredToolKind {
        self.kind
    }
}

/// Current request plus the complete local catalog a provider may search.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredToolRequest {
    provider: String,
    model: String,
    query: String,
    entries: Arc<Vec<DeferredToolEntry>>,
}

impl DeferredToolRequest {
    pub(crate) fn new(
        provider: String,
        model: String,
        query: String,
        entries: Arc<Vec<DeferredToolEntry>>,
    ) -> Self {
        Self {
            provider,
            model,
            query,
            entries,
        }
    }

    /// Inference provider route being prepared.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Canonical or configured model id being prepared.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Latest durable user text used as the selection query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Complete searchable catalog; this stays local and is not automatically
    /// inserted into the main model request.
    #[must_use]
    pub fn entries(&self) -> &[DeferredToolEntry] {
        &self.entries
    }
}

/// Provider-selected logical names to expose on this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredToolSelection {
    names: Vec<String>,
}

impl DeferredToolSelection {
    /// Validate a bounded unique selection.
    ///
    /// # Errors
    /// Unsafe names, duplicates, or more than 256 rows fail before a request
    /// can be changed.
    pub fn new<I, S>(names: I) -> Result<Self, DeferredToolError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names = names.into_iter().map(Into::into).collect::<Vec<_>>();
        if names.len() > MAX_SELECTIONS {
            return Err(DeferredToolError::SelectionTooLarge);
        }
        let mut unique = BTreeSet::new();
        for name in &names {
            if !safe_name(name) {
                return Err(DeferredToolError::InvalidToolName);
            }
            if !unique.insert(name.as_str()) {
                return Err(DeferredToolError::DuplicateSelection);
            }
        }
        Ok(Self { names })
    }

    /// Selected names in provider order.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    fn set(&self) -> BTreeSet<&str> {
        self.names.iter().map(String::as_str).collect()
    }
}

/// Async provider that searches/chooses tools but cannot execute them.
#[async_trait::async_trait]
pub trait DeferredToolProvider: Send + Sync {
    /// Stable implementation id.
    fn id(&self) -> &DeferredToolProviderId;

    /// Select the logical names this request may expose.
    ///
    /// # Errors
    /// Provider failure or cancellation refuses request preparation; no
    /// unfiltered fallback is inferred.
    async fn select(
        &self,
        request: DeferredToolRequest,
        cancellation: CancellationToken,
    ) -> Result<DeferredToolSelection, DeferredToolError>;
}

/// Deterministic local selector used by the default product composition.
///
/// Small catalogs pass through unchanged. Large catalogs rank query overlap in
/// name/description, then use original catalog order as the stable tie-breaker.
/// This provider selects membership only and owns no execution handle.
pub struct LexicalDeferredToolProvider {
    id: DeferredToolProviderId,
    max_selected: usize,
}

impl LexicalDeferredToolProvider {
    /// Build one selector with an explicit 1..=256 result ceiling.
    ///
    /// # Errors
    /// Zero or above-protocol ceilings fail before composition.
    pub fn new(max_selected: usize) -> Result<Self, DeferredToolError> {
        if max_selected == 0 || max_selected > MAX_SELECTIONS {
            return Err(DeferredToolError::InvalidSelectionLimit);
        }
        Ok(Self {
            id: DeferredToolProviderId::new("lexical-local")?,
            max_selected,
        })
    }

    /// Maximum rows selected from a large catalog.
    #[must_use]
    pub const fn max_selected(&self) -> usize {
        self.max_selected
    }
}

#[async_trait::async_trait]
impl DeferredToolProvider for LexicalDeferredToolProvider {
    fn id(&self) -> &DeferredToolProviderId {
        &self.id
    }

    async fn select(
        &self,
        request: DeferredToolRequest,
        cancellation: CancellationToken,
    ) -> Result<DeferredToolSelection, DeferredToolError> {
        if cancellation.is_cancelled() {
            return Err(DeferredToolError::Cancelled);
        }
        if request.entries.len() <= self.max_selected {
            return DeferredToolSelection::new(
                request.entries.iter().map(|entry| entry.name.clone()),
            );
        }
        let query = lexical_terms(&request.query);
        let mut ranked = request
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (lexical_score(entry, &query), index))
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        let mut selected = ranked
            .into_iter()
            .take(self.max_selected)
            .map(|(_, index)| index)
            .collect::<Vec<_>>();
        selected.sort_unstable();
        if cancellation.is_cancelled() {
            return Err(DeferredToolError::Cancelled);
        }
        DeferredToolSelection::new(
            selected
                .into_iter()
                .map(|index| request.entries[index].name.clone()),
        )
    }
}

fn lexical_terms(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn lexical_score(entry: &DeferredToolEntry, query: &BTreeSet<String>) -> usize {
    let name = entry.name.to_lowercase();
    let description = entry.description.to_lowercase();
    query.iter().fold(0usize, |score, term| {
        let name_score = if name == term.as_str() {
            1_000
        } else if name.contains(term) {
            100
        } else {
            0
        };
        let description_score = if description
            .split(|character: char| !character.is_alphanumeric())
            .any(|word| word == term.as_str())
        {
            10
        } else if description.contains(term) {
            1
        } else {
            0
        };
        score
            .saturating_add(name_score)
            .saturating_add(description_score)
    })
}

/// Complete local catalog before deferred selection.
#[derive(Debug, Clone)]
pub struct DeferredToolCatalog {
    tool_specs: Vec<ToolSpec>,
    native_routes: Vec<NativeToolRoute>,
    entries: Arc<Vec<DeferredToolEntry>>,
    full_schema_bytes: usize,
    full_schema_nodes: usize,
}

impl DeferredToolCatalog {
    /// Build a deterministic union of client schemas and N01 route rows.
    ///
    /// Provider-native route evidence overrides the family label of a same-name
    /// client schema without removing that schema; `request_draft` performs the
    /// existing provider-native substitution after selection.
    ///
    /// # Errors
    /// Duplicate/invalid client names, non-object schemas, duplicate N01
    /// logicals, or more than 16,384 union rows fail.
    pub fn new(
        tool_specs: Vec<ToolSpec>,
        native_routes: Vec<NativeToolRoute>,
    ) -> Result<Self, DeferredToolError> {
        let mut positions = BTreeMap::new();
        let mut entries = Vec::new();
        for tool in &tool_specs {
            if !safe_name(&tool.name) {
                return Err(DeferredToolError::InvalidToolName);
            }
            if !tool.parameters.is_object() {
                return Err(DeferredToolError::InvalidToolSchema);
            }
            if positions.insert(tool.name.clone(), entries.len()).is_some() {
                return Err(DeferredToolError::DuplicateCatalogName);
            }
            entries.push(DeferredToolEntry {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: Some(tool.parameters.clone()),
                kind: DeferredToolKind::Client,
            });
        }
        let mut route_names = BTreeSet::new();
        for route in &native_routes {
            if !safe_name(route.logical()) || !route_names.insert(route.logical()) {
                return Err(DeferredToolError::DuplicateNativeRoute);
            }
            let kind = match route.kind() {
                NativeToolImplementationKind::Provider => DeferredToolKind::ProviderNative,
                NativeToolImplementationKind::Client => DeferredToolKind::Client,
                NativeToolImplementationKind::Mcp => DeferredToolKind::Mcp,
            };
            if let Some(index) = positions.get(route.logical()).copied() {
                entries[index].kind = kind;
            } else {
                positions.insert(route.logical().to_owned(), entries.len());
                entries.push(DeferredToolEntry {
                    name: route.logical().to_owned(),
                    description: String::new(),
                    parameters: None,
                    kind,
                });
            }
        }
        if entries.len() > MAX_CATALOG_ENTRIES {
            return Err(DeferredToolError::CatalogTooLarge);
        }
        let full_schema_bytes = serialized_tool_bytes(&tool_specs)?;
        let full_schema_nodes = tool_specs.iter().map(tool_schema_nodes).sum();
        Ok(Self {
            tool_specs,
            native_routes,
            entries: Arc::new(entries),
            full_schema_bytes,
            full_schema_nodes,
        })
    }

    /// Complete searchable manifest.
    #[must_use]
    pub fn entries(&self) -> &[DeferredToolEntry] {
        &self.entries
    }

    pub(crate) fn shared_entries(&self) -> Arc<Vec<DeferredToolEntry>> {
        self.entries.clone()
    }

    /// Apply provider-selected membership while preserving catalog order.
    ///
    /// # Errors
    /// A selected name absent from the current union fails instead of becoming
    /// an unavailable/hidden execution later.
    pub fn apply(
        &self,
        selection: &DeferredToolSelection,
    ) -> Result<DeferredToolPlan, DeferredToolError> {
        let selected = selection.set();
        if selected
            .iter()
            .any(|name| !self.entries.iter().any(|entry| entry.name == *name))
        {
            return Err(DeferredToolError::UnknownSelection);
        }
        let tool_specs = self
            .tool_specs
            .iter()
            .filter(|tool| selected.contains(tool.name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let native_routes = self
            .native_routes
            .iter()
            .filter(|route| selected.contains(route.logical()))
            .cloned()
            .collect::<Vec<_>>();
        let selected_schema_bytes = serialized_tool_bytes(&tool_specs)?;
        let selected_schema_nodes = tool_specs.iter().map(tool_schema_nodes).sum();
        let selected_entries = self
            .entries
            .iter()
            .filter(|entry| selected.contains(entry.name.as_str()))
            .count();
        Ok(DeferredToolPlan {
            tool_specs,
            native_routes,
            metrics: DeferredToolMetrics {
                catalog_entries: self.entries.len(),
                selected_entries,
                full_schema_bytes: self.full_schema_bytes,
                selected_schema_bytes,
                full_schema_nodes: self.full_schema_nodes,
                selected_schema_nodes,
                native_routes: self.native_routes.len(),
                selected_native_routes: self
                    .native_routes
                    .iter()
                    .filter(|route| selected.contains(route.logical()))
                    .count(),
            },
        })
    }
}

/// Filtered tool declaration and N01 route set for one request.
#[derive(Debug, Clone)]
pub struct DeferredToolPlan {
    tool_specs: Vec<ToolSpec>,
    native_routes: Vec<NativeToolRoute>,
    metrics: DeferredToolMetrics,
}

impl DeferredToolPlan {
    /// Model-facing client schemas that survived selection.
    #[must_use]
    pub fn tool_specs(&self) -> &[ToolSpec] {
        &self.tool_specs
    }

    /// N01 routes that survived the same selection.
    #[must_use]
    pub fn native_routes(&self) -> &[NativeToolRoute] {
        &self.native_routes
    }

    /// Deterministic context/work proxy for this selection.
    #[must_use]
    pub const fn metrics(&self) -> &DeferredToolMetrics {
        &self.metrics
    }

    pub(crate) fn into_parts(self) -> (Vec<ToolSpec>, Vec<NativeToolRoute>, DeferredToolMetrics) {
        (self.tool_specs, self.native_routes, self.metrics)
    }
}

/// Deterministic large-catalog context and schema-work measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredToolMetrics {
    catalog_entries: usize,
    selected_entries: usize,
    full_schema_bytes: usize,
    selected_schema_bytes: usize,
    full_schema_nodes: usize,
    selected_schema_nodes: usize,
    native_routes: usize,
    selected_native_routes: usize,
}

macro_rules! metric_getter {
    ($name:ident, $field:ident, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub const fn $name(&self) -> usize {
            self.$field
        }
    };
}

impl DeferredToolMetrics {
    metric_getter!(
        catalog_entries,
        catalog_entries,
        "Rows in the complete union."
    );
    metric_getter!(
        selected_entries,
        selected_entries,
        "Rows selected for this request."
    );
    metric_getter!(
        full_schema_bytes,
        full_schema_bytes,
        "Serialized bytes of every client schema."
    );
    metric_getter!(
        selected_schema_bytes,
        selected_schema_bytes,
        "Serialized bytes of selected client schemas."
    );
    metric_getter!(
        full_schema_nodes,
        full_schema_nodes,
        "JSON-schema nodes in the complete client catalog."
    );
    metric_getter!(
        selected_schema_nodes,
        selected_schema_nodes,
        "JSON-schema nodes in selected client schemas."
    );
    metric_getter!(native_routes, native_routes, "N01 routes before selection.");
    metric_getter!(
        selected_native_routes,
        selected_native_routes,
        "N01 routes after selection."
    );
}

/// One explicit tool call produced by a Code Mode provider.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeModeCall {
    id: heycode_core::CallId,
    name: String,
    arguments: serde_json::Value,
}

impl CodeModeCall {
    /// Validate one explicit ordinary tool call.
    ///
    /// # Errors
    /// Unsafe identity/name or non-object arguments fail before stream
    /// materialization.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Result<Self, DeferredToolError> {
        let id = id.into();
        let name = name.into();
        if !safe_name(&id) {
            return Err(DeferredToolError::InvalidCallId);
        }
        if !safe_name(&name) {
            return Err(DeferredToolError::InvalidToolName);
        }
        if !arguments.is_object() {
            return Err(DeferredToolError::InvalidCallArguments);
        }
        Ok(Self {
            id: heycode_core::CallId::from_raw(id),
            name,
            arguments,
        })
    }

    /// Provider call identity.
    #[must_use]
    pub fn id(&self) -> &heycode_core::CallId {
        &self.id
    }

    /// Selected tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Exact object arguments.
    #[must_use]
    pub const fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
}

/// Code Mode output represented only as ordinary provider tool-call chunks.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeModeSchedule {
    calls: Vec<CodeModeCall>,
}

impl CodeModeSchedule {
    /// Bind explicit calls to the deferred selection that authorized exposure.
    ///
    /// # Errors
    /// Duplicate ids, unselected names, or more than 256 calls fail. No tool is
    /// executed by this constructor.
    pub fn new(
        calls: Vec<CodeModeCall>,
        selected: &DeferredToolSelection,
    ) -> Result<Self, DeferredToolError> {
        if calls.is_empty() {
            return Err(DeferredToolError::EmptyCodeModeSchedule);
        }
        if calls.len() > MAX_CODE_MODE_CALLS {
            return Err(DeferredToolError::CodeModeScheduleTooLarge);
        }
        let selected = selected.set();
        let mut ids = BTreeSet::new();
        for call in &calls {
            if !ids.insert(call.id.as_str()) {
                return Err(DeferredToolError::DuplicateCallId);
            }
            if !selected.contains(call.name.as_str()) {
                return Err(DeferredToolError::UnselectedCodeModeCall);
            }
        }
        Ok(Self { calls })
    }

    /// Materialize calls as the ordinary stream chunks A06 already consumes.
    ///
    /// This method performs no approval or execution. A provider yields these
    /// chunks; only the Agent's existing tool batch can run them.
    #[must_use]
    pub fn stream_chunks(&self) -> Vec<heycode_llm::StreamChunk> {
        let mut chunks = Vec::with_capacity(self.calls.len().saturating_add(1));
        for (index, call) in self.calls.iter().enumerate() {
            chunks.push(heycode_llm::StreamChunk::ToolCallDelta {
                index: u16::try_from(index).unwrap_or(u16::MAX),
                id: Some(call.id.as_str().to_owned()),
                name: Some(call.name.clone()),
                arguments_delta: call.arguments.to_string(),
            });
        }
        chunks.push(heycode_llm::StreamChunk::Finish(
            heycode_llm::FinishReason::ToolCalls,
        ));
        chunks
    }

    /// Materialize calls as strict-adapter inference events.
    ///
    /// Provider-specific opaque continuation state remains the adapter's
    /// responsibility. These events describe only the ordinary function calls
    /// the Agent can commit and execute through A06.
    ///
    /// # Errors
    /// A blank/control-bearing response identity is refused.
    pub fn inference_events(
        &self,
        response_id: impl Into<String>,
    ) -> Result<Vec<heycode_llm::InferenceEvent>, DeferredToolError> {
        let response_id = response_id.into();
        if !safe_name(&response_id) {
            return Err(DeferredToolError::InvalidResponseId);
        }
        let mut events = Vec::with_capacity(self.calls.len().saturating_mul(3).saturating_add(3));
        events.push(heycode_llm::InferenceEvent::ResponseStarted {
            response_id: response_id.clone(),
        });
        for (index, call) in self.calls.iter().enumerate() {
            let output_index = u32::try_from(index).unwrap_or(u32::MAX);
            let item_id = call.id.as_str().to_owned();
            events.push(heycode_llm::InferenceEvent::ItemStarted {
                output_index,
                item_id: item_id.clone(),
                kind: heycode_llm::StreamItemKind::FunctionCall,
            });
            events.push(heycode_llm::InferenceEvent::ToolCallDelta {
                output_index,
                id: Some(call.id.clone()),
                name: Some(call.name.clone()),
                arguments_delta: call.arguments.to_string(),
            });
            events.push(heycode_llm::InferenceEvent::ItemFinished {
                output_index,
                item_id,
                kind: heycode_llm::StreamItemKind::FunctionCall,
            });
        }
        events.push(heycode_llm::InferenceEvent::ResponseFinished {
            response_id,
            status: "tool_calls".to_owned(),
        });
        events.push(heycode_llm::InferenceEvent::Finish(
            heycode_llm::FinishReason::ToolCalls,
        ));
        Ok(events)
    }
}

pub(crate) type DeferredToolBinding = (Arc<dyn DeferredToolProvider>, Arc<()>);
pub(crate) type DeferredToolSlot = Mutex<Option<DeferredToolBinding>>;

/// Lifecycle owner for one Agent-local deferred provider installation.
pub struct DeferredToolRegistration {
    pub(crate) slot: Weak<DeferredToolSlot>,
    pub(crate) token: Arc<()>,
}

impl Drop for DeferredToolRegistration {
    fn drop(&mut self) {
        let Some(slot) = self.slot.upgrade() else {
            return;
        };
        let Ok(mut current) = slot.lock() else {
            return;
        };
        if current
            .as_ref()
            .is_some_and(|(_, token)| Arc::ptr_eq(token, &self.token))
        {
            *current = None;
        }
    }
}

/// Mount one deferred provider into an already-published Agent.
///
/// The plugin contributes no service: the provider is scoped to the one Agent
/// whose request/N01 preparation it intercepts. Its registration is a Context
/// effect and disappears on rollback/shutdown.
#[must_use]
pub fn deferred_tools_plugin(
    provider: Arc<dyn DeferredToolProvider>,
) -> Box<dyn heycode_core::Plugin> {
    struct DeferredToolsPlugin(Arc<dyn DeferredToolProvider>);

    impl heycode_core::Plugin for DeferredToolsPlugin {
        fn name(&self) -> &'static str {
            "deferred-tools"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Waterfall],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::InterceptionLayer,
                "agent/adapter-preparation:deferred-tools",
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_AGENT]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let agent = context
                .get::<crate::Agent>(crate::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service type mismatch"))?;
            let registration = agent
                .install_deferred_tool_provider(self.0.clone())
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(DeferredToolsPlugin(provider))
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn serialized_tool_bytes(tools: &[ToolSpec]) -> Result<usize, DeferredToolError> {
    serde_json::to_vec(tools)
        .map(|bytes| bytes.len())
        .map_err(|_| DeferredToolError::InvalidToolSchema)
}

fn tool_schema_nodes(tool: &ToolSpec) -> usize {
    2usize.saturating_add(json_nodes(&tool.parameters))
}

fn json_nodes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter().fold(1usize, |total, value| {
            total.saturating_add(json_nodes(value))
        }),
        serde_json::Value::Object(values) => values.values().fold(1usize, |total, value| {
            total.saturating_add(1).saturating_add(json_nodes(value))
        }),
        _ => 1,
    }
}

/// Deferred provider/catalog/Code Mode contract failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeferredToolError {
    /// Provider id is not valid lowercase kebab-case.
    #[error("deferred-tool provider id is invalid")]
    InvalidProviderId,
    /// Catalog or selection tool name is unsafe.
    #[error("deferred-tool name is invalid")]
    InvalidToolName,
    /// Client tool parameters are not an object schema.
    #[error("deferred-tool schema is invalid")]
    InvalidToolSchema,
    /// Client catalog contains the same name twice.
    #[error("deferred-tool catalog contains a duplicate name")]
    DuplicateCatalogName,
    /// N01 route set contains a duplicate logical name.
    #[error("deferred-tool catalog contains a duplicate native route")]
    DuplicateNativeRoute,
    /// Union catalog exceeded its defensive row cap.
    #[error("deferred-tool catalog is too large")]
    CatalogTooLarge,
    /// Provider selected more rows than one request may expose.
    #[error("deferred-tool selection is too large")]
    SelectionTooLarge,
    /// Composition selected an invalid provider result ceiling.
    #[error("deferred-tool selection limit is invalid")]
    InvalidSelectionLimit,
    /// Provider selected the same logical name twice.
    #[error("deferred-tool selection contains a duplicate")]
    DuplicateSelection,
    /// Provider selected a name absent from the current catalog generation.
    #[error("deferred-tool selection names an unknown tool")]
    UnknownSelection,
    /// Caller cancelled provider selection.
    #[error("deferred-tool selection was cancelled")]
    Cancelled,
    /// Agent-local provider slot already has an owner.
    #[error("a deferred-tool provider is already installed")]
    AlreadyInstalled,
    /// Provider slot state is unavailable.
    #[error("deferred-tool provider state is unavailable")]
    Unavailable,
    /// Code Mode call identity is unsafe.
    #[error("Code Mode call id is invalid")]
    InvalidCallId,
    /// Code Mode arguments must be a JSON object.
    #[error("Code Mode call arguments must be an object")]
    InvalidCallArguments,
    /// Strict-adapter response identity is unsafe.
    #[error("Code Mode response id is invalid")]
    InvalidResponseId,
    /// Code Mode reused a call id.
    #[error("Code Mode schedule contains a duplicate call id")]
    DuplicateCallId,
    /// Code Mode attempted a tool outside its deferred selection.
    #[error("Code Mode attempted a tool that was not selected")]
    UnselectedCodeModeCall,
    /// Code Mode emitted too many calls in one schedule.
    #[error("Code Mode schedule is too large")]
    CodeModeScheduleTooLarge,
    /// A tool-call schedule must contain at least one explicit call.
    #[error("Code Mode tool-call schedule is empty")]
    EmptyCodeModeSchedule,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod lexical_tests {
    use super::*;

    fn request(count: usize, query: &str) -> DeferredToolRequest {
        let mut entries = (0..count)
            .map(|index| DeferredToolEntry {
                name: format!("catalog-tool-{index:05}"),
                description: "generic capability".to_owned(),
                parameters: Some(serde_json::json!({"type":"object"})),
                kind: DeferredToolKind::Client,
            })
            .collect::<Vec<_>>();
        if let Some(last) = entries.last_mut() {
            last.name = "database-search".to_owned();
            last.description = "Search database records".to_owned();
        }
        DeferredToolRequest::new(
            "fake".to_owned(),
            "model".to_owned(),
            query.to_owned(),
            Arc::new(entries),
        )
    }

    #[tokio::test]
    async fn small_catalogs_pass_through_while_large_catalogs_rank_relevance() {
        let provider = LexicalDeferredToolProvider::new(8).unwrap();
        let small = provider
            .select(request(4, "database"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(small.names().len(), 4);

        let large = provider
            .select(request(100, "search database"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(large.names().len(), 8);
        assert!(large.names().iter().any(|name| name == "database-search"));
        assert_eq!(provider.id().as_str(), "lexical-local");
    }

    #[tokio::test]
    async fn invalid_limits_and_pre_cancelled_selection_fail_before_work() {
        assert!(LexicalDeferredToolProvider::new(0).is_err());
        assert!(LexicalDeferredToolProvider::new(MAX_SELECTIONS + 1).is_err());
        let provider = LexicalDeferredToolProvider::new(8).unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            provider.select(request(100, "search"), cancellation).await,
            Err(DeferredToolError::Cancelled)
        );
    }
}
