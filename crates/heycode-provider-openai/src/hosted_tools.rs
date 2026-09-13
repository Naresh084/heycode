//! Provider-owned OpenAI hosted-tool definitions and completed-item facts.
//!
//! Exact Responses output items remain the replay truth. The classifications
//! here are a separate bounded inspection plane: they use only provider ids
//! that actually exist, never expose tool bodies through `Debug`, and decline
//! to invent a normalized call when the API omitted its input.
//!
//! Primary sources:
//! <https://developers.openai.com/api/docs/models/gpt-5.6-sol>,
//! <https://developers.openai.com/api/reference/cli/resources/responses>, and
//! the current first-party guides under `/api/docs/guides/tools-*`.

use std::sync::Arc;

use heycode_core::{
    CallId, NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ProviderRequestOption,
    ProviderStateItem, ProviderStateKind, ServerToolCall, ServerToolResult, ServerToolSource,
    UrlCitation,
};
use heycode_llm::{
    CapabilitySupport, NativeFeature, OpenAiResponsesConfig, ResponsesServerToolDefinition,
    ResponsesServerToolFault, ResponsesServerToolNormalization, ResponsesServerToolNormalizer,
    ResponsesServerToolPlan,
};

use crate::OPENAI_GPT_5_6_SOL;

/// Durable provider-option kind carrying an exact request-selected hosted-tool
/// subset.
pub const OPENAI_HOSTED_TOOLS_OPTION_KIND: &str = "hosted-tools";

/// Hosted/server tool families documented for the default Responses model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenAiHostedToolKind {
    /// OpenAI web search.
    WebSearch,
    /// Search over configured vector stores.
    FileSearch,
    /// OpenAI-managed code interpreter container.
    CodeInterpreter,
    /// OpenAI-managed shell container.
    HostedShell,
    /// Client-executed OpenAI computer-use action loop.
    ComputerUse,
    /// OpenAI image generation.
    ImageGeneration,
    /// Remote Model Context Protocol server.
    RemoteMcp,
}

impl OpenAiHostedToolKind {
    /// Every POA03 tool family, in stable order.
    pub const ALL: [Self; 7] = [
        Self::WebSearch,
        Self::FileSearch,
        Self::CodeInterpreter,
        Self::HostedShell,
        Self::ComputerUse,
        Self::ImageGeneration,
        Self::RemoteMcp,
    ];

    /// Stable provider-independent identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebSearch => "web_search",
            Self::FileSearch => "file_search",
            Self::CodeInterpreter => "code_interpreter",
            Self::HostedShell => "hosted_shell",
            Self::ComputerUse => "computer_use",
            Self::ImageGeneration => "image_generation",
            Self::RemoteMcp => "remote_mcp",
        }
    }

    /// Exact request `tools[].type` discriminator.
    #[must_use]
    pub const fn request_type(self) -> &'static str {
        match self {
            Self::WebSearch => "web_search",
            Self::FileSearch => "file_search",
            Self::CodeInterpreter => "code_interpreter",
            Self::HostedShell => "shell",
            Self::ComputerUse => "computer",
            Self::ImageGeneration => "image_generation",
            Self::RemoteMcp => "mcp",
        }
    }

    /// Exact N01 implementation id the composition owner must register for
    /// this provider-native family.
    #[must_use]
    pub const fn implementation_id(self) -> &'static str {
        match self {
            Self::WebSearch => "openai:web_search",
            Self::FileSearch => "openai:file_search",
            Self::CodeInterpreter => "openai:code_interpreter",
            Self::HostedShell => "openai:hosted_shell",
            Self::ComputerUse => "openai:computer_use",
            Self::ImageGeneration => "openai:image_generation",
            Self::RemoteMcp => "openai:remote_mcp",
        }
    }

    /// Primary completed Responses output-item discriminator.
    ///
    /// Families with a paired/result/control item expose the complete set
    /// through [`Self::output_item_types`].
    #[must_use]
    pub const fn output_item_type(self) -> &'static str {
        self.output_item_types()[0]
    }

    /// Exact completed output-item discriminators associated with the family.
    #[must_use]
    pub const fn output_item_types(self) -> &'static [&'static str] {
        match self {
            Self::WebSearch => &["web_search_call"],
            Self::FileSearch => &["file_search_call"],
            Self::CodeInterpreter => &["code_interpreter_call"],
            Self::HostedShell => &["shell_call", "shell_call_output"],
            Self::ComputerUse => &["computer_call", "computer_call_output"],
            Self::ImageGeneration => &["image_generation_call"],
            Self::RemoteMcp => &["mcp_call", "mcp_list_tools", "mcp_approval_request"],
        }
    }

    /// Classify one exact completed provider item without distilling replay
    /// state.
    ///
    /// # Errors
    /// Unproven model capability, route/type mismatch or malformed completed
    /// metadata returns a closed safe refusal.
    pub fn classify(
        self,
        model: &str,
        state: ProviderStateItem,
    ) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
        let event = classify_hosted_tool_item(model, state)?;
        if event.kind == self {
            Ok(event)
        } else {
            Err(OpenAiHostedToolFault::WrongItemType)
        }
    }
}

/// Capability evidence for one hosted tool on one model.
///
/// The official model page proves all seven for the maintained provider
/// default. The account-scoped Models API publishes no per-tool fields, so any
/// other id remains Unknown rather than inheriting that evidence.
#[must_use]
pub fn hosted_tool_support(model: &str, _kind: OpenAiHostedToolKind) -> CapabilitySupport {
    if model == OPENAI_GPT_5_6_SOL {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    }
}

/// One validated exact `tools[]` entry.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiHostedToolDefinition {
    kind: OpenAiHostedToolKind,
    wire: serde_json::Value,
}

impl OpenAiHostedToolDefinition {
    /// Minimal current web-search definition.
    #[must_use]
    pub fn web_search() -> Self {
        Self::plain(OpenAiHostedToolKind::WebSearch)
    }

    /// File search over one to sixteen bounded unique vector-store ids.
    ///
    /// # Errors
    /// Empty/oversized lists, duplicates or unsafe ids are refused.
    pub fn file_search(
        vector_store_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, OpenAiHostedToolFault> {
        let ids = vector_store_ids
            .into_iter()
            .map(|id| id.as_ref().to_owned())
            .collect::<Vec<_>>();
        if ids.is_empty()
            || ids.len() > 16
            || ids.iter().any(|id| !safe_identifier(id))
            || has_duplicate(&ids)
        {
            return Err(OpenAiHostedToolFault::InvalidConfiguration);
        }
        Ok(Self {
            kind: OpenAiHostedToolKind::FileSearch,
            wire: serde_json::json!({"type":"file_search","vector_store_ids":ids}),
        })
    }

    /// Code interpreter with an automatically created managed container.
    #[must_use]
    pub fn code_interpreter() -> Self {
        Self {
            kind: OpenAiHostedToolKind::CodeInterpreter,
            wire: serde_json::json!({"type":"code_interpreter","container":{"type":"auto"}}),
        }
    }

    /// Hosted shell in an automatically created container with networking
    /// explicitly disabled and direct invocation only.
    #[must_use]
    pub fn hosted_shell() -> Self {
        Self {
            kind: OpenAiHostedToolKind::HostedShell,
            wire: serde_json::json!({
                "type":"shell",
                "allowed_callers":["direct"],
                "environment":{
                    "type":"container_auto",
                    "network_policy":{"type":"disabled"}
                }
            }),
        }
    }

    /// Current computer-use tool definition.
    ///
    /// This definition starts a client-executed action loop; it is not a
    /// provider-executed server tool result.
    #[must_use]
    pub fn computer_use() -> Self {
        Self::plain(OpenAiHostedToolKind::ComputerUse)
    }

    /// Current image-generation tool definition.
    #[must_use]
    pub fn image_generation() -> Self {
        Self::plain(OpenAiHostedToolKind::ImageGeneration)
    }

    /// Remote MCP server with mandatory per-call approval and no inline
    /// authorization or arbitrary headers.
    ///
    /// # Errors
    /// Unsafe labels, non-HTTP(S), userinfo, query, fragment or oversized URLs
    /// are refused without echoing their text.
    pub fn remote_mcp(
        server_label: impl AsRef<str>,
        server_url: impl AsRef<str>,
    ) -> Result<Self, OpenAiHostedToolFault> {
        let label = server_label.as_ref();
        let url = server_url.as_ref();
        if !safe_identifier(label)
            || url.len() > 512
            || url.contains(['?', '#'])
            || heycode_http::HttpRequest::get(url).is_err()
        {
            return Err(OpenAiHostedToolFault::InvalidConfiguration);
        }
        Ok(Self {
            kind: OpenAiHostedToolKind::RemoteMcp,
            wire: serde_json::json!({
                "type":"mcp",
                "server_label":label,
                "server_url":url,
                "require_approval":"always"
            }),
        })
    }

    /// Tool family.
    #[must_use]
    pub const fn kind(&self) -> OpenAiHostedToolKind {
        self.kind
    }

    /// Exact request definition after model capability admission.
    ///
    /// # Errors
    /// Unknown model evidence never becomes permission to send the tool.
    pub fn wire_for(&self, model: &str) -> Result<&serde_json::Value, OpenAiHostedToolFault> {
        if hosted_tool_support(model, self.kind) == CapabilitySupport::Supported {
            Ok(&self.wire)
        } else {
            Err(OpenAiHostedToolFault::UnprovenCapability)
        }
    }

    fn plain(kind: OpenAiHostedToolKind) -> Self {
        Self {
            kind,
            wire: serde_json::json!({"type":kind.request_type()}),
        }
    }
}

impl std::fmt::Debug for OpenAiHostedToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.kind, formatter)
    }
}

/// A model-gated subset that the generic Responses adapter can serialize,
/// normalize and replay without an upward dependency.
///
/// Computer use still needs a client action/approval loop, image generation
/// needs attachment admission before its base64 result can become durable, and
/// this crate's mandatory-approval MCP definition needs an approval-response
/// owner. Those exact definitions remain classifiable but cannot enter this
/// plan yet.
#[derive(Clone)]
pub struct OpenAiHostedTools {
    definitions: Vec<OpenAiHostedToolDefinition>,
    shared_definitions: Vec<ResponsesServerToolDefinition>,
    plan: ResponsesServerToolPlan,
    option: ProviderRequestOption,
}

impl OpenAiHostedTools {
    /// Build one exact generic-adapter plan after model and shared-bridge
    /// admission.
    ///
    /// # Errors
    /// Empty/duplicate selections, unproven model support, definitions that
    /// require an unowned upper bridge, or invalid shared configuration fail.
    pub fn new(
        model: &str,
        definitions: Vec<OpenAiHostedToolDefinition>,
    ) -> Result<Self, OpenAiHostedToolFault> {
        if definitions.is_empty()
            || definitions.iter().enumerate().any(|(index, definition)| {
                definitions[..index]
                    .iter()
                    .any(|prior| prior.kind == definition.kind)
            })
        {
            return Err(OpenAiHostedToolFault::InvalidConfiguration);
        }
        let mut shared = Vec::with_capacity(definitions.len());
        for definition in &definitions {
            if matches!(
                definition.kind,
                OpenAiHostedToolKind::ComputerUse
                    | OpenAiHostedToolKind::ImageGeneration
                    | OpenAiHostedToolKind::RemoteMcp
            ) {
                return Err(OpenAiHostedToolFault::MissingSharedBridge);
            }
            let request = definition.wire_for(model)?.clone();
            let normalizers = definition
                .kind
                .output_item_types()
                .iter()
                .map(|output_type| {
                    Arc::new(OpenAiHostedToolNormalizer { output_type })
                        as Arc<dyn ResponsesServerToolNormalizer>
                })
                .collect();
            let mut shared_definition = ResponsesServerToolDefinition::new(request, normalizers)
                .map_err(map_shared_configuration)?;
            if definition.kind == OpenAiHostedToolKind::WebSearch {
                shared_definition =
                    shared_definition.with_required_native_feature(NativeFeature::Web);
            }
            shared.push(shared_definition);
        }
        let plan = ResponsesServerToolPlan::new(OPENAI_HOSTED_TOOLS_OPTION_KIND, shared.clone())
            .map_err(map_shared_configuration)?;
        let option = plan
            .provider_option("openai", &shared)
            .map_err(map_shared_configuration)?;
        Ok(Self {
            definitions,
            shared_definitions: shared,
            plan,
            option,
        })
    }

    /// Provider-owned exact definitions in durable selection order.
    #[must_use]
    pub fn definitions(&self) -> &[OpenAiHostedToolDefinition] {
        &self.definitions
    }

    /// Exact provider option consumed by the shared plan.
    #[must_use]
    pub const fn provider_option(&self) -> &ProviderRequestOption {
        &self.option
    }

    /// Materialize only the definitions selected by the exact N01 route set.
    ///
    /// Client/MCP selections for the same logical capability deliberately
    /// produce no OpenAI option. An OpenAI-owned route for a known family must
    /// match both the provider-owned logical and implementation ids and must
    /// name a definition this provider instance actually configured.
    ///
    /// # Errors
    /// A mismatched/duplicate OpenAI route, an unconfigured known family, an
    /// unproven model capability or an invalid shared option fails before the
    /// request header or transport exists.
    pub fn provider_option_for_routes(
        &self,
        model: &str,
        routes: &[NativeToolRoute],
    ) -> Result<Option<ProviderRequestOption>, OpenAiHostedToolFault> {
        let mut selected = Vec::new();
        for route in routes {
            if route.kind() != NativeToolImplementationKind::Provider
                || route.provider() != Some("openai")
            {
                continue;
            }
            let Some(kind) = OpenAiHostedToolKind::ALL
                .iter()
                .copied()
                .find(|kind| kind.as_str() == route.logical())
            else {
                continue;
            };
            if route.implementation() != kind.implementation_id() {
                return Err(OpenAiHostedToolFault::InvalidConfiguration);
            }
            let Some(index) = self
                .definitions
                .iter()
                .position(|definition| definition.kind == kind)
            else {
                return Err(OpenAiHostedToolFault::InvalidConfiguration);
            };
            self.definitions[index].wire_for(model)?;
            if selected.contains(&index) {
                return Err(OpenAiHostedToolFault::InvalidConfiguration);
            }
            selected.push(index);
        }
        if selected.is_empty() {
            return Ok(None);
        }
        selected.sort_unstable();
        let selected = selected
            .into_iter()
            .map(|index| self.shared_definitions[index].clone())
            .collect::<Vec<_>>();
        self.plan
            .provider_option("openai", &selected)
            .map(Some)
            .map_err(map_shared_configuration)
    }

    /// Attach this plan to the generic Responses protocol adapter.
    #[must_use]
    pub fn configure(&self, config: OpenAiResponsesConfig) -> OpenAiResponsesConfig {
        config.with_server_tool_plan(self.plan.clone())
    }

    /// Recheck the provider-owned per-model gate before every resolution.
    ///
    /// # Errors
    /// A configured definition cannot be reused on an unproven model.
    pub fn validate_model(&self, model: &str) -> Result<(), OpenAiHostedToolFault> {
        for definition in &self.definitions {
            definition.wire_for(model)?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for OpenAiHostedTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiHostedTools")
            .field("definition_count", &self.definitions.len())
            .finish()
    }
}

struct OpenAiHostedToolNormalizer {
    output_type: &'static str,
}

impl ResponsesServerToolNormalizer for OpenAiHostedToolNormalizer {
    fn output_item_type(&self) -> &'static str {
        self.output_type
    }

    fn normalize(
        &self,
        state: &ProviderStateItem,
    ) -> Result<ResponsesServerToolNormalization, ResponsesServerToolFault> {
        if state.data().get("type").and_then(serde_json::Value::as_str) != Some(self.output_type) {
            return Err(ResponsesServerToolFault::InvalidItem);
        }
        let event = classify_hosted_tool_item(state.model(), state.clone())
            .map_err(|_| ResponsesServerToolFault::InvalidItem)?;
        let (call, result) = match event.role {
            OpenAiHostedToolItemRole::ProviderOperation
            | OpenAiHostedToolItemRole::ProviderResult => {
                (event.call.clone(), event.result.clone())
            }
            OpenAiHostedToolItemRole::ClientAction
            | OpenAiHostedToolItemRole::ClientResult
            | OpenAiHostedToolItemRole::ApprovalRequest => {
                return Err(ResponsesServerToolFault::InvalidItem);
            }
        };
        ResponsesServerToolNormalization::new(call, result, Vec::new())
    }
}

fn map_shared_configuration(_fault: ResponsesServerToolFault) -> OpenAiHostedToolFault {
    OpenAiHostedToolFault::InvalidConfiguration
}

/// Whether the completed item represents provider execution or a client-owned
/// continuation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiHostedToolItemRole {
    /// Provider executed or prepared one hosted operation.
    ProviderOperation,
    /// Provider returned the separately correlated result of a hosted call.
    ProviderResult,
    /// The application must execute the action itself.
    ClientAction,
    /// The application supplied the client-executed result.
    ClientResult,
    /// The application must approve or reject a proposed MCP call.
    ApprovalRequest,
}

/// Safe terminal classification for one completed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiHostedToolOutcome {
    /// Provider item completed successfully.
    Completed,
    /// Provider item terminated as failed or incomplete.
    Failed,
    /// Provider item is waiting on explicit application approval.
    ApprovalRequired,
}

/// Safe classification plus unchanged provider state used for replay.
#[derive(Clone, PartialEq)]
pub struct OpenAiHostedToolEvent {
    kind: OpenAiHostedToolKind,
    role: OpenAiHostedToolItemRole,
    outcome: OpenAiHostedToolOutcome,
    call: Option<ServerToolCall>,
    result: Option<ServerToolResult>,
    state: ProviderStateItem,
}

impl OpenAiHostedToolEvent {
    /// Hosted tool family.
    #[must_use]
    pub const fn kind(&self) -> OpenAiHostedToolKind {
        self.kind
    }

    /// Execution/continuation role of the exact item.
    #[must_use]
    pub const fn role(&self) -> OpenAiHostedToolItemRole {
        self.role
    }

    /// Terminal outcome.
    #[must_use]
    pub const fn outcome(&self) -> OpenAiHostedToolOutcome {
        self.outcome
    }

    /// Bounded provider-executed call when the item exposes an exact id and
    /// input object.
    #[must_use]
    pub const fn call(&self) -> Option<&ServerToolCall> {
        self.call.as_ref()
    }

    /// Bounded provider-executed result when exact correlation exists.
    #[must_use]
    pub const fn result(&self) -> Option<&ServerToolResult> {
        self.result.as_ref()
    }

    /// Exact unchanged provider item for durable replay.
    #[must_use]
    pub const fn state(&self) -> &ProviderStateItem {
        &self.state
    }
}

impl std::fmt::Debug for OpenAiHostedToolEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiHostedToolEvent")
            .field("kind", &self.kind)
            .field("role", &self.role)
            .field("outcome", &self.outcome)
            .field("has_call", &self.call.is_some())
            .field("has_result", &self.result.is_some())
            .finish()
    }
}

/// Classify one completed Responses output item from a configured hosted-tool
/// route.
///
/// The caller remains responsible for invoking this only for an
/// `response.output_item.done` item. `ProviderStateItem` is retained unchanged
/// whether or not the safe vocabulary can represent a normalized event.
///
/// # Errors
/// Wrong route/model/type, unproven capability, an intermediate status or a
/// malformed known item is refused with no provider body in the error.
pub fn classify_hosted_tool_item(
    model: &str,
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    validate_route(model, &state)?;
    let object = state
        .data()
        .as_object()
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    let item_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    let kind = item_kind(item_type).ok_or(OpenAiHostedToolFault::WrongItemType)?;
    if hosted_tool_support(model, kind) != CapabilitySupport::Supported {
        return Err(OpenAiHostedToolFault::UnprovenCapability);
    }
    match item_type {
        "web_search_call" => classify_web(state),
        "file_search_call" => classify_file_search(state),
        "code_interpreter_call" => classify_code_interpreter(state),
        "shell_call" => classify_shell_call(state),
        "shell_call_output" => classify_shell_output(state),
        "computer_call" => classify_computer_call(state),
        "computer_call_output" => classify_computer_output(state),
        "image_generation_call" => classify_image_generation(state),
        "mcp_call" => classify_mcp_call(state),
        "mcp_list_tools" => classify_mcp_list_tools(state),
        "mcp_approval_request" => classify_mcp_approval(state),
        _ => Err(OpenAiHostedToolFault::WrongItemType),
    }
}

/// Extract bounded URL citations from one completed assistant message item.
///
/// File/container citations remain inside exact provider state because the
/// shared normalized citation vocabulary intentionally represents public URLs
/// only. No citation is associated with a call id the API did not publish.
///
/// # Errors
/// Wrong route, unproven web capability, malformed known URL annotations or a
/// non-completed assistant message are refused safely.
pub fn classify_hosted_tool_citations(
    model: &str,
    state: &ProviderStateItem,
) -> Result<Vec<UrlCitation>, OpenAiHostedToolFault> {
    validate_route(model, state)?;
    if hosted_tool_support(model, OpenAiHostedToolKind::WebSearch) != CapabilitySupport::Supported {
        return Err(OpenAiHostedToolFault::UnprovenCapability);
    }
    let object = state
        .data()
        .as_object()
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    if string_field(object, "type")? != "message"
        || string_field(object, "status")? != "completed"
        || string_field(object, "role")? != "assistant"
    {
        return Err(OpenAiHostedToolFault::WrongItemType);
    }
    let content = array_field(object, "content")?;
    let mut citations = Vec::new();
    for part in content {
        let Some(part) = part.as_object() else {
            return Err(OpenAiHostedToolFault::InvalidItem);
        };
        if part.get("type").and_then(serde_json::Value::as_str) != Some("output_text") {
            continue;
        }
        for annotation in array_field(part, "annotations")? {
            let Some(annotation) = annotation.as_object() else {
                return Err(OpenAiHostedToolFault::InvalidItem);
            };
            if annotation.get("type").and_then(serde_json::Value::as_str) != Some("url_citation") {
                continue;
            }
            let start = integer_u32(annotation, "start_index")?;
            let end = integer_u32(annotation, "end_index")?;
            let citation = UrlCitation::new(
                string_field(annotation, "url")?,
                Some(string_field(annotation, "title")?),
                None,
                Some(start),
                Some(end),
            )
            .map_err(|_| OpenAiHostedToolFault::InvalidItem)?;
            citations.push(citation);
        }
    }
    Ok(citations)
}

fn classify_web(state: ProviderStateItem) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let outcome = required_status(object, &["completed"], &["failed"])?;
    let Some(action) = object.get("action") else {
        return Ok(event(
            OpenAiHostedToolKind::WebSearch,
            OpenAiHostedToolItemRole::ProviderOperation,
            outcome,
            None,
            None,
            state,
        ));
    };
    if !action.is_object() {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let call = server_call(id, "web_search", "web_search", action.clone())?;
    let result = match outcome {
        OpenAiHostedToolOutcome::Completed => {
            let (count, sources) = web_sources(action)?;
            Some(server_success(call.id().clone(), count, sources)?)
        }
        OpenAiHostedToolOutcome::Failed => Some(server_error(call.id().clone(), "failed")?),
        OpenAiHostedToolOutcome::ApprovalRequired => None,
    };
    Ok(event(
        OpenAiHostedToolKind::WebSearch,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        result,
        state,
    ))
}

fn classify_file_search(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let outcome = required_status(object, &["completed"], &["incomplete", "failed"])?;
    let queries = array_field(object, "queries")?;
    if queries.iter().any(|query| !query.is_string()) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let call = server_call(
        id,
        "file_search",
        "file_search",
        serde_json::json!({"queries":queries}),
    )?;
    let result = result_from_status(
        &call,
        outcome,
        optional_array_count(object, "results")?,
        Vec::new(),
    )?;
    Ok(event(
        OpenAiHostedToolKind::FileSearch,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        Some(result),
        state,
    ))
}

fn classify_code_interpreter(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let outcome = required_status(object, &["completed"], &["incomplete", "failed"])?;
    let code = object
        .get("code")
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    if !code.is_null() && !code.is_string() {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let container_id = string_field(object, "container_id")?;
    if !safe_identifier(container_id) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let call = server_call(
        id,
        "code_interpreter",
        "code_interpreter",
        serde_json::json!({"code":code,"container_id":container_id}),
    )?;
    let result = result_from_status(
        &call,
        outcome,
        optional_array_count(object, "outputs")?,
        Vec::new(),
    )?;
    Ok(event(
        OpenAiHostedToolKind::CodeInterpreter,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        Some(result),
        state,
    ))
}

fn classify_shell_call(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    validate_optional_exact_id(object, "id")?;
    let call_id = exact_id(object, "call_id")?;
    let outcome = optional_status(object, &["completed"], &["incomplete"])?;
    let environment = object
        .get("environment")
        .and_then(serde_json::Value::as_object)
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    let environment_type = string_field(environment, "type")?;
    let role = match environment_type {
        "container_reference" => OpenAiHostedToolItemRole::ProviderOperation,
        "local" => OpenAiHostedToolItemRole::ClientAction,
        _ => return Err(OpenAiHostedToolFault::InvalidItem),
    };
    let action = object
        .get("action")
        .filter(|action| action.is_object())
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    if role == OpenAiHostedToolItemRole::ClientAction {
        return Ok(event(
            OpenAiHostedToolKind::HostedShell,
            role,
            outcome,
            None,
            None,
            state,
        ));
    }
    let call = server_call(call_id, "hosted_shell", "shell", action.clone())?;
    let result = if outcome == OpenAiHostedToolOutcome::Failed {
        Some(server_error(call.id().clone(), "incomplete")?)
    } else {
        None
    };
    Ok(event(
        OpenAiHostedToolKind::HostedShell,
        role,
        outcome,
        Some(call),
        result,
        state,
    ))
}

fn classify_shell_output(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    validate_optional_exact_id(object, "id")?;
    let call_id = exact_id(object, "call_id")?;
    let mut outcome = optional_status(object, &["completed"], &["incomplete"])?;
    let outputs = array_field(object, "output")?;
    let mut error_code = if outcome == OpenAiHostedToolOutcome::Failed {
        Some("incomplete")
    } else {
        None
    };
    for output in outputs {
        let output = output
            .as_object()
            .ok_or(OpenAiHostedToolFault::InvalidItem)?;
        let outcome_object = output
            .get("outcome")
            .and_then(serde_json::Value::as_object)
            .ok_or(OpenAiHostedToolFault::InvalidItem)?;
        match string_field(outcome_object, "type")? {
            "timeout" => error_code = Some("timeout"),
            "exit" => {
                if outcome_object
                    .get("exit_code")
                    .and_then(serde_json::Value::as_i64)
                    .is_none_or(|code| code != 0)
                {
                    error_code = Some("nonzero_exit");
                }
            }
            _ => return Err(OpenAiHostedToolFault::InvalidItem),
        }
    }
    let result = if let Some(error_code) = error_code {
        outcome = OpenAiHostedToolOutcome::Failed;
        server_error(CallId::from_raw(call_id), error_code)?
    } else {
        server_success(
            CallId::from_raw(call_id),
            Some(count(outputs.len())?),
            Vec::new(),
        )?
    };
    Ok(event(
        OpenAiHostedToolKind::HostedShell,
        OpenAiHostedToolItemRole::ProviderResult,
        outcome,
        None,
        Some(result),
        state,
    ))
}

fn classify_computer_call(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let _item_id = exact_id(object, "id")?;
    let _call_id = exact_id(object, "call_id")?;
    let outcome = required_status(object, &["completed"], &["incomplete"])?;
    if object
        .get("action")
        .is_some_and(|action| !action.is_object())
    {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    Ok(event(
        OpenAiHostedToolKind::ComputerUse,
        OpenAiHostedToolItemRole::ClientAction,
        outcome,
        None,
        None,
        state,
    ))
}

fn classify_computer_output(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let _item_id = exact_id(object, "id")?;
    let _call_id = exact_id(object, "call_id")?;
    let outcome = required_status(object, &["completed"], &["incomplete", "failed"])?;
    if !object
        .get("output")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    Ok(event(
        OpenAiHostedToolKind::ComputerUse,
        OpenAiHostedToolItemRole::ClientResult,
        outcome,
        None,
        None,
        state,
    ))
}

fn classify_image_generation(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let outcome = required_status(object, &["completed"], &["failed"])?;
    let result_value = object
        .get("result")
        .and_then(serde_json::Value::as_str)
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    if outcome == OpenAiHostedToolOutcome::Completed && result_value.is_empty() {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let Some(prompt) = object
        .get("revised_prompt")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(event(
            OpenAiHostedToolKind::ImageGeneration,
            OpenAiHostedToolItemRole::ProviderOperation,
            outcome,
            None,
            None,
            state,
        ));
    };
    let call = server_call(
        id,
        "image_generation",
        "image_generation",
        serde_json::json!({"revised_prompt":prompt}),
    )?;
    let result = result_from_status(&call, outcome, Some(1), Vec::new())?;
    Ok(event(
        OpenAiHostedToolKind::ImageGeneration,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        Some(result),
        state,
    ))
}

fn classify_mcp_call(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let arguments_text = string_field(object, "arguments")?;
    let arguments: serde_json::Value =
        serde_json::from_str(arguments_text).map_err(|_| OpenAiHostedToolFault::InvalidItem)?;
    if !arguments.is_object() {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let name = string_field(object, "name")?;
    let server_label = string_field(object, "server_label")?;
    if !safe_identifier(name) || !safe_identifier(server_label) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let call = server_call(
        id,
        "remote_mcp",
        "mcp",
        serde_json::json!({
            "server_label":server_label,
            "name":name,
            "arguments":arguments
        }),
    )?;
    let status = optional_mcp_status(object)?;
    let error = object.get("error").filter(|value| !value.is_null());
    let output = object.get("output").filter(|value| !value.is_null());
    if output.is_some_and(|value| !value.is_string()) || (error.is_some() && output.is_some()) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let (outcome, result) = if let Some(error) = error {
        if status == Some("completed") {
            return Err(OpenAiHostedToolFault::InvalidItem);
        }
        let error_type = error
            .as_object()
            .and_then(|error| error.get("type"))
            .and_then(serde_json::Value::as_str)
            .filter(|kind| {
                matches!(
                    *kind,
                    "mcp_protocol_error" | "mcp_tool_execution_error" | "http_error"
                )
            })
            .ok_or(OpenAiHostedToolFault::InvalidItem)?;
        (
            OpenAiHostedToolOutcome::Failed,
            server_error(call.id().clone(), error_type)?,
        )
    } else if matches!(status, Some("failed" | "incomplete")) {
        (
            OpenAiHostedToolOutcome::Failed,
            server_error(call.id().clone(), status.unwrap_or("failed"))?,
        )
    } else if output.is_some() || status == Some("completed") {
        (
            OpenAiHostedToolOutcome::Completed,
            server_success(call.id().clone(), output.map(|_| 1), Vec::new())?,
        )
    } else {
        return Err(OpenAiHostedToolFault::InvalidItem);
    };
    Ok(event(
        OpenAiHostedToolKind::RemoteMcp,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        Some(result),
        state,
    ))
}

fn classify_mcp_list_tools(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let id = exact_id(object, "id")?;
    let server_label = string_field(object, "server_label")?;
    if !safe_identifier(server_label) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let tools = array_field(object, "tools")?;
    if tools.iter().any(|tool| !tool.is_object()) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let call = server_call(
        id,
        "remote_mcp",
        "mcp_list_tools",
        serde_json::json!({"server_label":server_label}),
    )?;
    let error = object.get("error").filter(|value| !value.is_null());
    if error.is_some_and(|value| !value.is_string()) {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    let (outcome, result) = if error.is_some() {
        (
            OpenAiHostedToolOutcome::Failed,
            server_error(call.id().clone(), "list_tools_failed")?,
        )
    } else {
        (
            OpenAiHostedToolOutcome::Completed,
            server_success(call.id().clone(), Some(count(tools.len())?), Vec::new())?,
        )
    };
    Ok(event(
        OpenAiHostedToolKind::RemoteMcp,
        OpenAiHostedToolItemRole::ProviderOperation,
        outcome,
        Some(call),
        Some(result),
        state,
    ))
}

fn classify_mcp_approval(
    state: ProviderStateItem,
) -> Result<OpenAiHostedToolEvent, OpenAiHostedToolFault> {
    let object = object(&state)?;
    let _id = exact_id(object, "id")?;
    let arguments = string_field(object, "arguments")?;
    let _: serde_json::Value =
        serde_json::from_str(arguments).map_err(|_| OpenAiHostedToolFault::InvalidItem)?;
    if !safe_identifier(string_field(object, "name")?)
        || !safe_identifier(string_field(object, "server_label")?)
    {
        return Err(OpenAiHostedToolFault::InvalidItem);
    }
    Ok(event(
        OpenAiHostedToolKind::RemoteMcp,
        OpenAiHostedToolItemRole::ApprovalRequest,
        OpenAiHostedToolOutcome::ApprovalRequired,
        None,
        None,
        state,
    ))
}

fn validate_route(model: &str, state: &ProviderStateItem) -> Result<(), OpenAiHostedToolFault> {
    state
        .validate()
        .map_err(|_| OpenAiHostedToolFault::InvalidItem)?;
    if state.provider() != "openai"
        || state.model() != model
        || state.protocol() != ProviderProtocol::OpenAiResponses
        || state.kind() != ProviderStateKind::ResponseOutputItem
    {
        return Err(OpenAiHostedToolFault::WrongRoute);
    }
    Ok(())
}

fn item_kind(item_type: &str) -> Option<OpenAiHostedToolKind> {
    match item_type {
        "web_search_call" => Some(OpenAiHostedToolKind::WebSearch),
        "file_search_call" => Some(OpenAiHostedToolKind::FileSearch),
        "code_interpreter_call" => Some(OpenAiHostedToolKind::CodeInterpreter),
        "shell_call" | "shell_call_output" => Some(OpenAiHostedToolKind::HostedShell),
        "computer_call" | "computer_call_output" => Some(OpenAiHostedToolKind::ComputerUse),
        "image_generation_call" => Some(OpenAiHostedToolKind::ImageGeneration),
        "mcp_call" | "mcp_list_tools" | "mcp_approval_request" => {
            Some(OpenAiHostedToolKind::RemoteMcp)
        }
        _ => None,
    }
}

fn object(
    state: &ProviderStateItem,
) -> Result<&serde_json::Map<String, serde_json::Value>, OpenAiHostedToolFault> {
    state
        .data()
        .as_object()
        .ok_or(OpenAiHostedToolFault::InvalidItem)
}

fn exact_id<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, OpenAiHostedToolFault> {
    let value = string_field(object, field)?;
    if safe_identifier(value) {
        Ok(value)
    } else {
        Err(OpenAiHostedToolFault::InvalidItem)
    }
}

fn validate_optional_exact_id(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<(), OpenAiHostedToolFault> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(()),
        Some(value) if value.as_str().is_some_and(safe_identifier) => Ok(()),
        Some(_) => Err(OpenAiHostedToolFault::InvalidItem),
    }
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, OpenAiHostedToolFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(OpenAiHostedToolFault::InvalidItem)
}

fn array_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a Vec<serde_json::Value>, OpenAiHostedToolFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or(OpenAiHostedToolFault::InvalidItem)
}

fn integer_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<u32, OpenAiHostedToolFault> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(OpenAiHostedToolFault::InvalidItem)
}

fn required_status(
    object: &serde_json::Map<String, serde_json::Value>,
    completed: &[&str],
    failed: &[&str],
) -> Result<OpenAiHostedToolOutcome, OpenAiHostedToolFault> {
    classify_status(Some(string_field(object, "status")?), completed, failed)
}

fn optional_status(
    object: &serde_json::Map<String, serde_json::Value>,
    completed: &[&str],
    failed: &[&str],
) -> Result<OpenAiHostedToolOutcome, OpenAiHostedToolFault> {
    let status = match object.get("status") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_str().ok_or(OpenAiHostedToolFault::InvalidItem)?),
    };
    classify_status(status, completed, failed)
}

fn classify_status(
    status: Option<&str>,
    completed: &[&str],
    failed: &[&str],
) -> Result<OpenAiHostedToolOutcome, OpenAiHostedToolFault> {
    match status {
        None => Ok(OpenAiHostedToolOutcome::Completed),
        Some(status) if completed.contains(&status) => Ok(OpenAiHostedToolOutcome::Completed),
        Some(status) if failed.contains(&status) => Ok(OpenAiHostedToolOutcome::Failed),
        Some(_) => Err(OpenAiHostedToolFault::InvalidItem),
    }
}

fn optional_mcp_status(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<&str>, OpenAiHostedToolFault> {
    match object.get("status") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            let status = value.as_str().ok_or(OpenAiHostedToolFault::InvalidItem)?;
            if matches!(status, "completed" | "failed" | "incomplete") {
                Ok(Some(status))
            } else {
                Err(OpenAiHostedToolFault::InvalidItem)
            }
        }
    }
}

fn optional_array_count(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u32>, OpenAiHostedToolFault> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_array()
            .ok_or(OpenAiHostedToolFault::InvalidItem)
            .and_then(|values| count(values.len()).map(Some)),
    }
}

fn web_sources(
    action: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), OpenAiHostedToolFault> {
    let action = action
        .as_object()
        .ok_or(OpenAiHostedToolFault::InvalidItem)?;
    match action.get("sources") {
        Some(serde_json::Value::Array(values)) => {
            let mut sources = Vec::with_capacity(values.len());
            for value in values {
                let value = value
                    .as_object()
                    .ok_or(OpenAiHostedToolFault::InvalidItem)?;
                if string_field(value, "type")? != "url" {
                    return Err(OpenAiHostedToolFault::InvalidItem);
                }
                sources.push(
                    ServerToolSource::new(string_field(value, "url")?, None)
                        .map_err(|_| OpenAiHostedToolFault::InvalidItem)?,
                );
            }
            Ok((Some(count(values.len())?), sources))
        }
        Some(serde_json::Value::Null) | None => {
            let action_type = string_field(action, "type")?;
            if matches!(action_type, "open_page" | "find_in_page")
                && let Some(url) = action.get("url").and_then(serde_json::Value::as_str)
            {
                let source = ServerToolSource::new(url, None)
                    .map_err(|_| OpenAiHostedToolFault::InvalidItem)?;
                return Ok((Some(1), vec![source]));
            }
            Ok((None, Vec::new()))
        }
        Some(_) => Err(OpenAiHostedToolFault::InvalidItem),
    }
}

fn server_call(
    id: &str,
    logical: &str,
    provider_name: &str,
    input: serde_json::Value,
) -> Result<ServerToolCall, OpenAiHostedToolFault> {
    ServerToolCall::new(CallId::from_raw(id), logical, provider_name, input)
        .map_err(|_| OpenAiHostedToolFault::InvalidItem)
}

fn server_success(
    call_id: CallId,
    output_count: Option<u32>,
    sources: Vec<ServerToolSource>,
) -> Result<ServerToolResult, OpenAiHostedToolFault> {
    ServerToolResult::success(call_id, output_count, sources)
        .map_err(|_| OpenAiHostedToolFault::InvalidItem)
}

fn server_error(call_id: CallId, code: &str) -> Result<ServerToolResult, OpenAiHostedToolFault> {
    ServerToolResult::error(call_id, code).map_err(|_| OpenAiHostedToolFault::InvalidItem)
}

fn result_from_status(
    call: &ServerToolCall,
    outcome: OpenAiHostedToolOutcome,
    output_count: Option<u32>,
    sources: Vec<ServerToolSource>,
) -> Result<ServerToolResult, OpenAiHostedToolFault> {
    match outcome {
        OpenAiHostedToolOutcome::Completed => {
            server_success(call.id().clone(), output_count, sources)
        }
        OpenAiHostedToolOutcome::Failed => server_error(call.id().clone(), "failed"),
        OpenAiHostedToolOutcome::ApprovalRequired => Err(OpenAiHostedToolFault::InvalidItem),
    }
}

fn event(
    kind: OpenAiHostedToolKind,
    role: OpenAiHostedToolItemRole,
    outcome: OpenAiHostedToolOutcome,
    call: Option<ServerToolCall>,
    result: Option<ServerToolResult>,
    state: ProviderStateItem,
) -> OpenAiHostedToolEvent {
    OpenAiHostedToolEvent {
        kind,
        role,
        outcome,
        call,
        result,
        state,
    }
}

fn count(value: usize) -> Result<u32, OpenAiHostedToolFault> {
    u32::try_from(value).map_err(|_| OpenAiHostedToolFault::InvalidItem)
}

/// Closed hosted-tool refusal that never carries provider/configuration text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAiHostedToolFault {
    /// Tool-specific configuration is invalid.
    InvalidConfiguration,
    /// A definition needs a client, approval or attachment bridge not owned by
    /// the generic Responses adapter.
    MissingSharedBridge,
    /// The selected model has no exact support evidence.
    UnprovenCapability,
    /// Provider/model/protocol/state-kind identity is wrong.
    WrongRoute,
    /// Output item belongs to a different or unsupported tool family.
    WrongItemType,
    /// Completed output item metadata is malformed or non-terminal.
    InvalidItem,
}

impl std::fmt::Display for OpenAiHostedToolFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "OpenAI hosted-tool configuration is invalid",
            Self::MissingSharedBridge => "OpenAI hosted-tool shared bridge is unavailable",
            Self::UnprovenCapability => "OpenAI hosted-tool capability is unproven",
            Self::WrongRoute => "OpenAI hosted-tool state route is invalid",
            Self::WrongItemType => "OpenAI hosted-tool item type is invalid",
            Self::InvalidItem => "OpenAI hosted-tool item is malformed",
        })
    }
}

impl std::error::Error for OpenAiHostedToolFault {}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value.as_bytes().iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn has_duplicate(values: &[String]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}
