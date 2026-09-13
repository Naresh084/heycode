//! Provider-owned Anthropic server-tool definitions, evidence and replay facts.
//!
//! Exact provider blocks remain the replay authority. The classifications in
//! this module are a separate bounded inspection/configuration plane; they do
//! not distill or replace the [`ProviderStateItem`] that Anthropic requires on
//! a continued turn.
//!
//! Primary sources:
//! <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-reference>,
//! <https://platform.claude.com/docs/en/agents-and-tools/tool-use/server-tools>,
//! <https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool>,
//! <https://platform.claude.com/docs/en/agents-and-tools/tool-use/advisor-tool>,
//! <https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool>,
//! and <https://platform.claude.com/docs/en/agents-and-tools/mcp-connector>.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use heycode_core::{
    CallId, NativeToolImplementationKind, NativeToolRoute, ProviderProtocol, ProviderRequestOption,
    ProviderStateItem, ProviderStateKind, ServerToolCall, ServerToolResult, ServerToolSource,
    UrlCitation,
};
use heycode_llm::{CapabilitySupport, FinishReason, InferenceInput};

use crate::ANTHROPIC_CLAUDE_OPUS_5;

/// Durable provider-option kind carrying one exact server-tool plan.
pub const ANTHROPIC_SERVER_TOOLS_OPTION_KIND: &str = "server-tools";

/// Beta value required by the advisor definition and its historical blocks.
pub const ANTHROPIC_ADVISOR_BETA: &str = "advisor-tool-2026-03-01";

/// Beta value required by the MCP connector definition and historical blocks.
pub const ANTHROPIC_MCP_CONNECTOR_BETA: &str = "mcp-client-2025-11-20";

/// Provider-owned bound used by the basic web-search definition.
pub const ANTHROPIC_WEB_SEARCH_MAX_USES: u32 = 5;

/// Provider-owned bound used by the basic web-fetch definition.
pub const ANTHROPIC_WEB_FETCH_MAX_USES: u32 = 5;

const MAX_SERVER_TOOL_DEFINITIONS: usize = 64;

/// Anthropic-executed tool families in PAN03.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnthropicServerToolKind {
    /// Anthropic web search.
    WebSearch,
    /// Anthropic web fetch.
    WebFetch,
    /// Anthropic sandboxed code execution toolset.
    CodeExecution,
    /// Separate-model advisor call.
    Advisor,
    /// Hosted regex tool search.
    ToolSearch,
    /// Remote MCP connector.
    McpConnector,
}

impl AnthropicServerToolKind {
    /// Every PAN03 family, in stable order.
    pub const ALL: [Self; 6] = [
        Self::WebSearch,
        Self::WebFetch,
        Self::CodeExecution,
        Self::Advisor,
        Self::ToolSearch,
        Self::McpConnector,
    ];

    /// Stable heycode logical identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::CodeExecution => "code_execution",
            Self::Advisor => "advisor",
            Self::ToolSearch => "tool_search",
            Self::McpConnector => "remote_mcp",
        }
    }

    /// Exact current `tools[].type` selected by this provider boundary.
    #[must_use]
    pub const fn tool_type(self) -> &'static str {
        match self {
            Self::WebSearch => "web_search_20250305",
            Self::WebFetch => "web_fetch_20250910",
            Self::CodeExecution => "code_execution_20260521",
            Self::Advisor => "advisor_20260301",
            Self::ToolSearch => "tool_search_tool_regex_20251119",
            Self::McpConnector => "mcp_toolset",
        }
    }

    /// Exact N01 implementation id the composition owner must register for
    /// this provider-native family.
    #[must_use]
    pub const fn implementation_id(self) -> &'static str {
        match self {
            Self::WebSearch => "anthropic:web_search",
            Self::WebFetch => "anthropic:web_fetch",
            Self::CodeExecution => "anthropic:code_execution",
            Self::Advisor => "anthropic:advisor",
            Self::ToolSearch => "anthropic:tool_search",
            Self::McpConnector => "anthropic:remote_mcp",
        }
    }

    /// Fixed request-definition name, when the definition has one.
    ///
    /// MCP toolsets deliberately have no `name`: their `mcp_server_name`
    /// selects a server and response calls carry the remote tool's own name.
    #[must_use]
    pub const fn definition_name(self) -> Option<&'static str> {
        match self {
            Self::WebSearch => Some("web_search"),
            Self::WebFetch => Some("web_fetch"),
            Self::CodeExecution => Some("code_execution"),
            Self::Advisor => Some("advisor"),
            Self::ToolSearch => Some("tool_search_tool_regex"),
            Self::McpConnector => None,
        }
    }

    fn recognizes_server_call(self, provider_name: &str) -> bool {
        match self {
            Self::CodeExecution => matches!(
                provider_name,
                "bash_code_execution" | "text_editor_code_execution"
            ),
            Self::McpConnector => false,
            _ => self.definition_name() == Some(provider_name),
        }
    }

    fn expected_result_type(self, provider_name: &str) -> Option<&'static str> {
        match (self, provider_name) {
            (Self::WebSearch, "web_search") => Some("web_search_tool_result"),
            (Self::WebFetch, "web_fetch") => Some("web_fetch_tool_result"),
            (Self::CodeExecution, "bash_code_execution") => Some("bash_code_execution_tool_result"),
            (Self::CodeExecution, "text_editor_code_execution") => {
                Some("text_editor_code_execution_tool_result")
            }
            (Self::Advisor, "advisor") => Some("advisor_tool_result"),
            (Self::ToolSearch, "tool_search_tool_regex") => Some("tool_search_tool_result"),
            (Self::McpConnector, _) => Some("mcp_tool_result"),
            _ => None,
        }
    }
}

/// Exact per-model support evidence from current official examples/tables.
///
/// `Unsupported` is returned only for explicit exclusions. Unlisted ids remain
/// `Unknown`, including ids that merely resemble a listed model.
#[must_use]
pub fn server_tool_support(model: &str, kind: AnthropicServerToolKind) -> CapabilitySupport {
    match kind {
        AnthropicServerToolKind::WebSearch => {
            support_from_list(model, &[ANTHROPIC_CLAUDE_OPUS_5, "claude-opus-4-8"], &[])
        }
        AnthropicServerToolKind::WebFetch => support_from_list(
            model,
            &["claude-opus-4-8", "claude-sonnet-5"],
            &[ANTHROPIC_CLAUDE_OPUS_5],
        ),
        AnthropicServerToolKind::CodeExecution => support_from_list(
            model,
            &[
                ANTHROPIC_CLAUDE_OPUS_5,
                "claude-fable-5",
                "claude-mythos-5",
                "claude-sonnet-5",
                "claude-opus-4-8",
                "claude-opus-4-7",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-opus-4-5-20251101",
                "claude-sonnet-4-5-20250929",
                "claude-haiku-4-5-20251001",
            ],
            &[],
        ),
        AnthropicServerToolKind::Advisor => support_from_list(
            model,
            &[
                "claude-haiku-4-5-20251001",
                "claude-sonnet-4-6",
                "claude-sonnet-5",
                "claude-opus-4-6",
                "claude-opus-4-7",
                "claude-opus-4-8",
                ANTHROPIC_CLAUDE_OPUS_5,
                "claude-fable-5",
                "claude-mythos-5",
            ],
            &[],
        ),
        AnthropicServerToolKind::ToolSearch => support_from_list(
            model,
            &[
                "claude-fable-5",
                "claude-mythos-5",
                ANTHROPIC_CLAUDE_OPUS_5,
                "claude-opus-4-8",
                "claude-opus-4-7",
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-opus-4-5-20251101",
                "claude-sonnet-4-5-20250929",
                "claude-haiku-4-5-20251001",
            ],
            &["claude-opus-4-1-20250805", "claude-opus-4-20250514"],
        ),
        AnthropicServerToolKind::McpConnector => {
            support_from_list(model, &[ANTHROPIC_CLAUDE_OPUS_5], &[])
        }
    }
}

/// Exact executor/advisor model-pair evidence for the advisor tool.
///
/// A known executor paired with a known but absent advisor is explicitly
/// unsupported. Unknown ids do not inherit a relative capability rank.
#[must_use]
pub fn advisor_pair_support(executor: &str, advisor: &str) -> CapabilitySupport {
    let allowed = match executor {
        "claude-haiku-4-5-20251001" | "claude-sonnet-4-6" => &[
            "claude-mythos-5",
            "claude-fable-5",
            ANTHROPIC_CLAUDE_OPUS_5,
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
        ][..],
        "claude-sonnet-5" => &[
            "claude-mythos-5",
            "claude-fable-5",
            ANTHROPIC_CLAUDE_OPUS_5,
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-sonnet-5",
        ][..],
        "claude-opus-4-6" => &[
            "claude-mythos-5",
            "claude-fable-5",
            ANTHROPIC_CLAUDE_OPUS_5,
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5",
        ][..],
        "claude-opus-4-7" | "claude-opus-4-8" => &[
            "claude-mythos-5",
            "claude-fable-5",
            ANTHROPIC_CLAUDE_OPUS_5,
            "claude-opus-4-8",
            "claude-opus-4-7",
        ][..],
        ANTHROPIC_CLAUDE_OPUS_5 | "claude-fable-5" | "claude-mythos-5" => {
            &["claude-mythos-5", "claude-fable-5", ANTHROPIC_CLAUDE_OPUS_5][..]
        }
        _ => return CapabilitySupport::Unknown,
    };
    if allowed.contains(&advisor) {
        CapabilitySupport::Supported
    } else if known_advisor_model(advisor) {
        CapabilitySupport::Unsupported
    } else {
        CapabilitySupport::Unknown
    }
}

fn known_advisor_model(model: &str) -> bool {
    matches!(
        model,
        "claude-mythos-5"
            | "claude-fable-5"
            | ANTHROPIC_CLAUDE_OPUS_5
            | "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
    )
}

fn support_from_list(model: &str, supported: &[&str], unsupported: &[&str]) -> CapabilitySupport {
    if supported.contains(&model) {
        CapabilitySupport::Supported
    } else if unsupported.contains(&model) {
        CapabilitySupport::Unsupported
    } else {
        CapabilitySupport::Unknown
    }
}

/// Exact provider tool definition plus beta/request extensions.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicServerToolDefinition {
    kind: AnthropicServerToolKind,
    tool: serde_json::Value,
    beta_headers: Vec<&'static str>,
    request_fields: Option<serde_json::Value>,
    advisor_model: Option<String>,
    mcp_server_name: Option<String>,
}

impl AnthropicServerToolDefinition {
    /// Basic current ZDR-eligible web search.
    #[must_use]
    pub fn web_search() -> Self {
        let mut definition = Self::plain(AnthropicServerToolKind::WebSearch);
        definition.tool["max_uses"] = serde_json::json!(ANTHROPIC_WEB_SEARCH_MAX_USES);
        definition
    }

    /// Basic current ZDR-eligible web fetch.
    #[must_use]
    pub fn web_fetch() -> Self {
        let mut definition = Self::plain(AnthropicServerToolKind::WebFetch);
        definition.tool["max_uses"] = serde_json::json!(ANTHROPIC_WEB_FETCH_MAX_USES);
        definition
    }

    /// Current code execution definition with the documented cell-limit hint.
    #[must_use]
    pub fn code_execution() -> Self {
        Self::plain(AnthropicServerToolKind::CodeExecution)
    }

    /// Advisor tool with an explicit advisor model and positive request cap.
    ///
    /// The executor/advisor compatibility pair is checked later by
    /// [`Self::tool_for`], when the executor model is known.
    ///
    /// # Errors
    /// Unsafe model ids or a zero cap are refused.
    pub fn advisor(
        advisor_model: impl AsRef<str>,
        max_uses: u32,
    ) -> Result<Self, AnthropicServerToolFault> {
        let advisor_model = advisor_model.as_ref();
        if !safe_identifier(advisor_model) || max_uses == 0 {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        Ok(Self {
            kind: AnthropicServerToolKind::Advisor,
            tool: serde_json::json!({
                "type":"advisor_20260301",
                "name":"advisor",
                "model":advisor_model,
                "max_uses":max_uses
            }),
            beta_headers: vec![ANTHROPIC_ADVISOR_BETA],
            request_fields: None,
            advisor_model: Some(advisor_model.to_owned()),
            mcp_server_name: None,
        })
    }

    /// Hosted regex tool search.
    #[must_use]
    pub fn tool_search_regex() -> Self {
        Self::plain(AnthropicServerToolKind::ToolSearch)
    }

    /// Credential-free HTTPS MCP connector and matching toolset.
    ///
    /// Literal authorization is structurally absent. A future credential-aware
    /// operation owner must resolve a reference at dispatch rather than add a
    /// token to this durable provider option.
    ///
    /// # Errors
    /// Unsafe names, non-HTTPS/userinfo/query/fragment or oversized URLs fail.
    pub fn mcp_connector(
        server_name: impl AsRef<str>,
        server_url: impl AsRef<str>,
    ) -> Result<Self, AnthropicServerToolFault> {
        let name = server_name.as_ref();
        let url = server_url.as_ref();
        if !safe_identifier(name)
            || url.len() > 2_048
            || !url.starts_with("https://")
            || url.contains(['?', '#'])
            || heycode_http::HttpRequest::get(url).is_err()
        {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        Ok(Self {
            kind: AnthropicServerToolKind::McpConnector,
            tool: serde_json::json!({
                "type":"mcp_toolset",
                "mcp_server_name":name,
                "default_config":{"enabled":true,"defer_loading":false}
            }),
            beta_headers: vec![ANTHROPIC_MCP_CONNECTOR_BETA],
            request_fields: Some(serde_json::json!({
                "mcp_servers":[{"type":"url","url":url,"name":name}]
            })),
            advisor_model: None,
            mcp_server_name: Some(name.to_owned()),
        })
    }

    /// Server tool family.
    #[must_use]
    pub const fn kind(&self) -> AnthropicServerToolKind {
        self.kind
    }

    /// Exact `tools[]` entry before model admission.
    #[must_use]
    pub const fn tool(&self) -> &serde_json::Value {
        &self.tool
    }

    /// Exact `tools[]` entry after per-model capability admission.
    ///
    /// # Errors
    /// Unsupported and unknown evidence fail distinctly. Advisor definitions
    /// additionally require an exact supported executor/advisor pair.
    pub fn tool_for(&self, model: &str) -> Result<&serde_json::Value, AnthropicServerToolFault> {
        let support = if let Some(advisor_model) = &self.advisor_model {
            advisor_pair_support(model, advisor_model)
        } else {
            server_tool_support(model, self.kind)
        };
        match support {
            CapabilitySupport::Supported => Ok(&self.tool),
            CapabilitySupport::Unsupported => Err(AnthropicServerToolFault::UnsupportedCapability),
            CapabilitySupport::Unknown => Err(AnthropicServerToolFault::UnprovenCapability),
        }
    }

    /// Required beta values, without the header name.
    #[must_use]
    pub fn beta_headers(&self) -> &[&'static str] {
        &self.beta_headers
    }

    /// Additional exact top-level request fields.
    #[must_use]
    pub const fn request_fields(&self) -> Option<&serde_json::Value> {
        self.request_fields.as_ref()
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.mcp_server_name.as_deref()
    }

    fn plain(kind: AnthropicServerToolKind) -> Self {
        let name = kind
            .definition_name()
            .map_or(serde_json::Value::Null, serde_json::Value::from);
        let mut tool = serde_json::json!({"type":kind.tool_type()});
        if !name.is_null() {
            tool["name"] = name;
        }
        Self {
            kind,
            tool,
            beta_headers: Vec::new(),
            request_fields: None,
            advisor_model: None,
            mcp_server_name: None,
        }
    }
}

impl std::fmt::Debug for AnthropicServerToolDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicServerToolDefinition")
            .field("kind", &self.kind)
            .field("has_beta", &!self.beta_headers.is_empty())
            .field("has_request_fields", &self.request_fields.is_some())
            .finish()
    }
}

/// One deterministic, secret-free set of Anthropic server tools.
#[derive(Clone, PartialEq, Eq)]
pub struct AnthropicServerToolPlan {
    definitions: Vec<AnthropicServerToolDefinition>,
    beta_headers: Vec<&'static str>,
    request_fields: serde_json::Value,
    deferred_tool_names: Vec<String>,
}

impl AnthropicServerToolPlan {
    /// Build and validate one exact request plan.
    ///
    /// Non-MCP families occur at most once. MCP may occur once per unique
    /// server because the API permits multiple server/toolset pairs.
    ///
    /// # Errors
    /// Empty/oversized plans, duplicate families or MCP servers, and malformed
    /// request extensions fail before becoming durable.
    pub fn new(
        definitions: Vec<AnthropicServerToolDefinition>,
    ) -> Result<Self, AnthropicServerToolFault> {
        if definitions.is_empty() || definitions.len() > MAX_SERVER_TOOL_DEFINITIONS {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        let mut kinds = BTreeSet::new();
        let mut mcp_servers = BTreeSet::new();
        let mut betas = BTreeSet::new();
        let mut request_mcp_servers = Vec::new();
        for definition in &definitions {
            if definition.kind == AnthropicServerToolKind::McpConnector {
                let name = definition
                    .mcp_server_name()
                    .ok_or(AnthropicServerToolFault::InvalidConfiguration)?;
                if !mcp_servers.insert(name) {
                    return Err(AnthropicServerToolFault::InvalidConfiguration);
                }
            } else if !kinds.insert(definition.kind) {
                return Err(AnthropicServerToolFault::InvalidConfiguration);
            }
            betas.extend(definition.beta_headers.iter().copied());
            if let Some(fields) = &definition.request_fields {
                let servers = fields
                    .get("mcp_servers")
                    .and_then(serde_json::Value::as_array)
                    .ok_or(AnthropicServerToolFault::InvalidConfiguration)?;
                request_mcp_servers.extend(servers.iter().cloned());
            }
        }
        let request_fields = if request_mcp_servers.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({"mcp_servers":request_mcp_servers})
        };
        Ok(Self {
            definitions,
            beta_headers: betas.into_iter().collect(),
            request_fields,
            deferred_tool_names: Vec::new(),
        })
    }

    /// Mark exact client/MCP tool names for hosted tool-search retrieval.
    ///
    /// The shared Messages hook projects these names as `defer_loading:true`;
    /// this provider type keeps selection exact and durable without importing
    /// any Agent or tool-registry owner.
    ///
    /// # Errors
    /// A plan without tool search, an empty list, unsafe names or duplicates
    /// fails rather than enabling a search tool with nothing searchable.
    pub fn with_deferred_tools(
        mut self,
        names: Vec<String>,
    ) -> Result<Self, AnthropicServerToolFault> {
        if !self
            .definitions
            .iter()
            .any(|definition| definition.kind == AnthropicServerToolKind::ToolSearch)
            || names.is_empty()
        {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        let mut unique = BTreeSet::new();
        for name in &names {
            if !safe_identifier(name) || !unique.insert(name.as_str()) {
                return Err(AnthropicServerToolFault::InvalidConfiguration);
            }
        }
        self.deferred_tool_names = names;
        Ok(self)
    }

    /// Exact definitions in request order.
    #[must_use]
    pub fn definitions(&self) -> &[AnthropicServerToolDefinition] {
        &self.definitions
    }

    /// Exact admitted `tools[]` entries for a selected model.
    ///
    /// # Errors
    /// Any unsupported/unproven definition rejects the whole plan.
    pub fn tools_for(
        &self,
        model: &str,
    ) -> Result<Vec<serde_json::Value>, AnthropicServerToolFault> {
        if self
            .definitions
            .iter()
            .any(|definition| definition.kind == AnthropicServerToolKind::ToolSearch)
            && self.deferred_tool_names.is_empty()
        {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        self.definitions
            .iter()
            .map(|definition| definition.tool_for(model).cloned())
            .collect()
    }

    /// Stable deduplicated beta values.
    #[must_use]
    pub fn beta_headers(&self) -> &[&'static str] {
        &self.beta_headers
    }

    /// Exact merged top-level request extensions.
    #[must_use]
    pub const fn request_fields(&self) -> &serde_json::Value {
        &self.request_fields
    }

    /// Exact names that the generic Messages hook must mark deferred.
    #[must_use]
    pub fn deferred_tool_names(&self) -> &[String] {
        &self.deferred_tool_names
    }

    /// Project this plan into the durable provider-option plane.
    ///
    /// # Errors
    /// A shared provider-option contract change or size violation fails closed.
    pub fn provider_option(&self) -> Result<ProviderRequestOption, AnthropicServerToolFault> {
        self.messages_plan_unchecked()?
            .provider_option("anthropic")
            .map_err(|_| AnthropicServerToolFault::InvalidConfiguration)
    }

    /// Materialize this exact plan only when N01 selected every configured
    /// provider-native family for the current request.
    ///
    /// The shared Messages plan is one atomic option today. A partial route
    /// set therefore fails instead of silently enabling the unselected
    /// families. Client/MCP routes for the same logical capability select no
    /// Anthropic option. Multiple configured MCP servers intentionally share
    /// the single `remote_mcp` logical family selected by N01.
    ///
    /// # Errors
    /// A mismatched/duplicate Anthropic route, a partial atomic plan,
    /// unsupported/unproven model evidence or an invalid option fails before
    /// the request header or transport exists.
    pub fn provider_option_for_routes(
        &self,
        model: &str,
        routes: &[NativeToolRoute],
    ) -> Result<Option<ProviderRequestOption>, AnthropicServerToolFault> {
        let configured = self
            .definitions
            .iter()
            .map(|definition| definition.kind)
            .collect::<BTreeSet<_>>();
        let mut selected = BTreeSet::new();
        for route in routes {
            if route.kind() != NativeToolImplementationKind::Provider
                || route.provider() != Some("anthropic")
            {
                continue;
            }
            let Some(kind) = AnthropicServerToolKind::ALL
                .iter()
                .copied()
                .find(|kind| kind.as_str() == route.logical())
            else {
                continue;
            };
            if route.implementation() != kind.implementation_id()
                || !configured.contains(&kind)
                || !selected.insert(kind)
            {
                return Err(AnthropicServerToolFault::InvalidConfiguration);
            }
        }
        if selected.is_empty() {
            return Ok(None);
        }
        if selected != configured {
            return Err(AnthropicServerToolFault::InvalidConfiguration);
        }
        self.tools_for(model)?;
        self.provider_option().map(Some)
    }

    /// Build the generic Messages request/parser plan after exact model gates.
    ///
    /// This is the downward-only bridge: the provider crate supplies current
    /// tool facts while `heycode-llm` remains the sole Messages serializer/parser.
    ///
    /// # Errors
    /// Unsupported/unproven model evidence or a shared-plan contract mismatch
    /// fails before the adapter can dispatch.
    pub fn messages_plan_for(
        &self,
        model: &str,
    ) -> Result<heycode_llm::AnthropicServerToolPlan, AnthropicServerToolFault> {
        self.tools_for(model)?;
        self.messages_plan_unchecked()
    }

    pub(crate) fn messages_plan_unchecked(
        &self,
    ) -> Result<heycode_llm::AnthropicServerToolPlan, AnthropicServerToolFault> {
        let request_fields = self
            .request_fields
            .as_object()
            .ok_or(AnthropicServerToolFault::InvalidConfiguration)?
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        let mut routes = Vec::new();
        let mut mcp_server_names = Vec::new();
        for definition in &self.definitions {
            match definition.kind {
                AnthropicServerToolKind::WebSearch => routes.push(named_route(
                    AnthropicServerToolKind::WebSearch,
                    "web_search",
                    "web_search",
                    "web_search_tool_result",
                    Some("web_search_requests"),
                )?),
                AnthropicServerToolKind::WebFetch => routes.push(named_route(
                    AnthropicServerToolKind::WebFetch,
                    "web_fetch",
                    "web_fetch",
                    "web_fetch_tool_result",
                    Some("web_fetch_requests"),
                )?),
                AnthropicServerToolKind::CodeExecution => {
                    routes.push(named_route(
                        AnthropicServerToolKind::CodeExecution,
                        "code_execution",
                        "bash_code_execution",
                        "bash_code_execution_tool_result",
                        Some("code_execution_requests"),
                    )?);
                    routes.push(named_route(
                        AnthropicServerToolKind::CodeExecution,
                        "code_execution",
                        "text_editor_code_execution",
                        "text_editor_code_execution_tool_result",
                        None,
                    )?);
                }
                AnthropicServerToolKind::Advisor => routes.push(named_route(
                    AnthropicServerToolKind::Advisor,
                    "advisor",
                    "advisor",
                    "advisor_tool_result",
                    None,
                )?),
                AnthropicServerToolKind::ToolSearch => routes.push(named_route(
                    AnthropicServerToolKind::ToolSearch,
                    "tool_search",
                    "tool_search_tool_regex",
                    "tool_search_tool_result",
                    None,
                )?),
                AnthropicServerToolKind::McpConnector => mcp_server_names.push(
                    definition
                        .mcp_server_name()
                        .ok_or(AnthropicServerToolFault::InvalidConfiguration)?
                        .to_owned(),
                ),
            }
        }
        if !mcp_server_names.is_empty() {
            routes.push(
                heycode_llm::AnthropicServerToolRoute::mcp()
                    .and_then(|route| route.with_mcp_server_names(mcp_server_names))
                    .and_then(|route| {
                        route.with_result_normalizer(Arc::new(AnthropicExactResultNormalizer::new(
                            AnthropicServerToolKind::McpConnector,
                            "remote_mcp",
                        )))
                    })
                    .map_err(|_| AnthropicServerToolFault::InvalidConfiguration)?,
            );
        }
        let plan = heycode_llm::AnthropicServerToolPlan::new(
            ANTHROPIC_SERVER_TOOLS_OPTION_KIND,
            self.definitions
                .iter()
                .map(|definition| definition.tool.clone())
                .collect(),
            self.beta_headers
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            request_fields,
            routes,
        )
        .map_err(|_| AnthropicServerToolFault::InvalidConfiguration)?;
        if self.deferred_tool_names.is_empty() {
            Ok(plan)
        } else {
            plan.with_deferred_tool_names(self.deferred_tool_names.clone())
                .map_err(|_| AnthropicServerToolFault::InvalidConfiguration)
        }
    }

    /// Classify every configured server call/result and web-search citation in
    /// one complete assistant provider item.
    ///
    /// # Errors
    /// Route mismatch, unsupported/unproven definitions, unadvertised calls,
    /// duplicate/orphan/mismatched results, or malformed normalized facts fail
    /// without returning provider content.
    pub fn classify(
        &self,
        model: &str,
        state: ProviderStateItem,
    ) -> Result<AnthropicServerToolClassification, AnthropicServerToolFault> {
        self.tools_for(model)?;
        validate_state_route(model, &state)?;
        let content = state
            .data()
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or(AnthropicServerToolFault::InvalidState)?;

        let mut calls = BTreeMap::<String, PendingCall>::new();
        let mut call_order = Vec::new();
        let mut client_tool_count = 0_usize;
        let mut citations = Vec::new();
        for block in content {
            let Some(block) = block.as_object() else {
                return Err(AnthropicServerToolFault::InvalidState);
            };
            let block_type = required_nonempty_str(block, "type")?;
            match block_type {
                "server_tool_use" => {
                    let id = required_nonempty_str(block, "id")?;
                    let provider_name = required_nonempty_str(block, "name")?;
                    if !id.starts_with("srvtoolu_") {
                        return Err(AnthropicServerToolFault::InvalidState);
                    }
                    let definition = self
                        .definitions
                        .iter()
                        .find(|definition| definition.kind.recognizes_server_call(provider_name))
                        .ok_or(AnthropicServerToolFault::InvalidState)?;
                    let input = block
                        .get("input")
                        .filter(|value| value.is_object())
                        .ok_or(AnthropicServerToolFault::InvalidState)?
                        .clone();
                    let expected_result_type = definition
                        .kind
                        .expected_result_type(provider_name)
                        .ok_or(AnthropicServerToolFault::InvalidState)?;
                    insert_call(
                        &mut calls,
                        &mut call_order,
                        id,
                        definition.kind,
                        provider_name,
                        input,
                        expected_result_type,
                    )?;
                }
                "mcp_tool_use" => {
                    let id = required_nonempty_str(block, "id")?;
                    if !id.starts_with("mcptoolu_") {
                        return Err(AnthropicServerToolFault::InvalidState);
                    }
                    let provider_name = required_nonempty_str(block, "name")?;
                    let server_name = required_nonempty_str(block, "server_name")?;
                    if !self.definitions.iter().any(|definition| {
                        definition.kind == AnthropicServerToolKind::McpConnector
                            && definition.mcp_server_name() == Some(server_name)
                    }) {
                        return Err(AnthropicServerToolFault::InvalidState);
                    }
                    let input = block
                        .get("input")
                        .filter(|value| value.is_object())
                        .ok_or(AnthropicServerToolFault::InvalidState)?
                        .clone();
                    insert_call(
                        &mut calls,
                        &mut call_order,
                        id,
                        AnthropicServerToolKind::McpConnector,
                        provider_name,
                        input,
                        "mcp_tool_result",
                    )?;
                }
                "tool_use" => client_tool_count = client_tool_count.saturating_add(1),
                "text" => classify_citations(block, &mut citations)?,
                _ => {}
            }
        }

        for block in content {
            let block = block
                .as_object()
                .ok_or(AnthropicServerToolFault::InvalidState)?;
            let block_type = required_nonempty_str(block, "type")?;
            if block_type == "tool_result" || !block_type.ends_with("_tool_result") {
                continue;
            }
            let id = required_nonempty_str(block, "tool_use_id")?;
            let Some(call) = calls.get_mut(id) else {
                if self.recognizes_result_type(block_type) {
                    return Err(AnthropicServerToolFault::InvalidState);
                }
                continue;
            };
            if block_type != call.expected_result_type || call.result.is_some() {
                return Err(AnthropicServerToolFault::InvalidState);
            }
            call.result = Some(normalize_exact_result(
                call.kind,
                call.call.provider_name(),
                block_type,
                block,
            )?);
        }

        let classified_calls = call_order
            .into_iter()
            .map(|id| {
                let call = calls
                    .remove(&id)
                    .ok_or(AnthropicServerToolFault::InvalidState)?;
                Ok(AnthropicServerToolCall {
                    kind: call.kind,
                    call: call.call,
                    result: call.result,
                })
            })
            .collect::<Result<Vec<_>, AnthropicServerToolFault>>()?;
        Ok(AnthropicServerToolClassification {
            state,
            calls: classified_calls,
            citations,
            client_tool_count,
        })
    }

    fn recognizes_result_type(&self, block_type: &str) -> bool {
        self.definitions
            .iter()
            .any(|definition| match definition.kind {
                AnthropicServerToolKind::CodeExecution => matches!(
                    block_type,
                    "bash_code_execution_tool_result" | "text_editor_code_execution_tool_result"
                ),
                AnthropicServerToolKind::McpConnector => block_type == "mcp_tool_result",
                kind => {
                    kind.definition_name()
                        .and_then(|name| kind.expected_result_type(name))
                        == Some(block_type)
                }
            })
    }
}

impl std::fmt::Debug for AnthropicServerToolPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicServerToolPlan")
            .field(
                "kinds",
                &self
                    .definitions
                    .iter()
                    .map(|definition| definition.kind)
                    .collect::<Vec<_>>(),
            )
            .field("beta_count", &self.beta_headers.len())
            .field("deferred_tool_count", &self.deferred_tool_names.len())
            .field(
                "has_request_fields",
                &self
                    .request_fields
                    .as_object()
                    .is_some_and(|fields| !fields.is_empty()),
            )
            .finish()
    }
}

struct AnthropicExactResultNormalizer {
    kind: AnthropicServerToolKind,
    provider_name: &'static str,
}

impl AnthropicExactResultNormalizer {
    const fn new(kind: AnthropicServerToolKind, provider_name: &'static str) -> Self {
        Self {
            kind,
            provider_name,
        }
    }
}

impl heycode_llm::AnthropicServerToolResultNormalizer for AnthropicExactResultNormalizer {
    fn id(&self) -> &'static str {
        match (self.kind, self.provider_name) {
            (AnthropicServerToolKind::WebSearch, _) => "anthropic-web-search-v1",
            (AnthropicServerToolKind::WebFetch, _) => "anthropic-web-fetch-v1",
            (AnthropicServerToolKind::CodeExecution, "bash_code_execution") => {
                "anthropic-code-bash-v1"
            }
            (AnthropicServerToolKind::CodeExecution, "text_editor_code_execution") => {
                "anthropic-code-text-editor-v1"
            }
            (AnthropicServerToolKind::Advisor, _) => "anthropic-advisor-v1",
            (AnthropicServerToolKind::ToolSearch, _) => "anthropic-tool-search-v1",
            (AnthropicServerToolKind::McpConnector, _) => "anthropic-mcp-v1",
            (AnthropicServerToolKind::CodeExecution, _) => "anthropic-code-invalid-v1",
        }
    }

    fn normalize(
        &self,
        block: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ServerToolResult, heycode_llm::AnthropicServerToolNormalizationFault> {
        let block_type = required_nonempty_str(block, "type")
            .map_err(|_| heycode_llm::AnthropicServerToolNormalizationFault::InvalidResult)?;
        normalize_exact_result(self.kind, self.provider_name, block_type, block)
            .map_err(|_| heycode_llm::AnthropicServerToolNormalizationFault::InvalidResult)
    }
}

fn named_route(
    kind: AnthropicServerToolKind,
    logical: &'static str,
    provider_name: &'static str,
    result_type: &'static str,
    usage_field: Option<&'static str>,
) -> Result<heycode_llm::AnthropicServerToolRoute, AnthropicServerToolFault> {
    heycode_llm::AnthropicServerToolRoute::named(
        logical,
        provider_name,
        vec![result_type.to_owned()],
        usage_field.map(str::to_owned),
    )
    .and_then(|route| {
        route.with_result_normalizer(Arc::new(AnthropicExactResultNormalizer::new(
            kind,
            provider_name,
        )))
    })
    .map_err(|_| AnthropicServerToolFault::InvalidConfiguration)
}

struct PendingCall {
    kind: AnthropicServerToolKind,
    call: ServerToolCall,
    expected_result_type: &'static str,
    result: Option<ServerToolResult>,
}

fn insert_call(
    calls: &mut BTreeMap<String, PendingCall>,
    order: &mut Vec<String>,
    id: &str,
    kind: AnthropicServerToolKind,
    provider_name: &str,
    input: serde_json::Value,
    expected_result_type: &'static str,
) -> Result<(), AnthropicServerToolFault> {
    if !safe_identifier(id) || !safe_identifier(provider_name) || calls.contains_key(id) {
        return Err(AnthropicServerToolFault::InvalidState);
    }
    let call = ServerToolCall::new(CallId::from_raw(id), kind.as_str(), provider_name, input)
        .map_err(|_| AnthropicServerToolFault::InvalidState)?;
    calls.insert(
        id.to_owned(),
        PendingCall {
            kind,
            call,
            expected_result_type,
            result: None,
        },
    );
    order.push(id.to_owned());
    Ok(())
}

fn normalize_exact_result(
    kind: AnthropicServerToolKind,
    provider_name: &str,
    block_type: &str,
    block: &serde_json::Map<String, serde_json::Value>,
) -> Result<ServerToolResult, AnthropicServerToolFault> {
    let call_id = CallId::from_raw(required_nonempty_str(block, "tool_use_id")?);
    let content = block
        .get("content")
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    if kind == AnthropicServerToolKind::McpConnector {
        let is_error = block
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .ok_or(AnthropicServerToolFault::InvalidState)?;
        if is_error {
            return ServerToolResult::error(call_id, "provider_reported_error")
                .map_err(|_| AnthropicServerToolFault::InvalidState);
        }
        let count = content
            .as_array()
            .ok_or(AnthropicServerToolFault::InvalidState)?
            .len();
        return ServerToolResult::success(call_id, checked_count(count)?, Vec::new())
            .map_err(|_| AnthropicServerToolFault::InvalidState);
    }

    let expected_error_type = format!("{block_type}_error");
    if let Some(error) = content.as_object()
        && error.get("type").and_then(serde_json::Value::as_str)
            == Some(expected_error_type.as_str())
    {
        let code = required_nonempty_str(error, "error_code")?;
        return ServerToolResult::error(call_id, code)
            .map_err(|_| AnthropicServerToolFault::InvalidState);
    }

    let (count, sources) = match kind {
        AnthropicServerToolKind::WebSearch => classify_web_search_result(content)?,
        AnthropicServerToolKind::WebFetch => classify_web_fetch_result(content)?,
        AnthropicServerToolKind::CodeExecution => {
            classify_code_execution_result(provider_name, content)?
        }
        AnthropicServerToolKind::Advisor => classify_advisor_result(content)?,
        AnthropicServerToolKind::ToolSearch => classify_tool_search_result(content)?,
        AnthropicServerToolKind::McpConnector => {
            return Err(AnthropicServerToolFault::InvalidState);
        }
    };
    ServerToolResult::success(call_id, count, sources)
        .map_err(|_| AnthropicServerToolFault::InvalidState)
}

fn classify_web_search_result(
    content: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), AnthropicServerToolFault> {
    let items = content
        .as_array()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    let mut sources = Vec::with_capacity(items.len());
    for item in items {
        let item = item
            .as_object()
            .ok_or(AnthropicServerToolFault::InvalidState)?;
        if required_nonempty_str(item, "type")? != "web_search_result" {
            return Err(AnthropicServerToolFault::InvalidState);
        }
        sources.push(public_source(item, "url", item.get("title"))?);
    }
    Ok((checked_count(items.len())?, sources))
}

fn classify_web_fetch_result(
    content: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), AnthropicServerToolFault> {
    let result = content
        .as_object()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    if required_nonempty_str(result, "type")? != "web_fetch_result" {
        return Err(AnthropicServerToolFault::InvalidState);
    }
    let title = result
        .get("content")
        .and_then(serde_json::Value::as_object)
        .and_then(|content| content.get("title"));
    Ok((Some(1), vec![public_source(result, "url", title)?]))
}

fn classify_code_execution_result(
    provider_name: &str,
    content: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), AnthropicServerToolFault> {
    let content = content
        .as_object()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    let kind = required_nonempty_str(content, "type")?;
    let valid = match provider_name {
        "bash_code_execution" => kind == "bash_code_execution_result",
        "text_editor_code_execution" => matches!(
            kind,
            "text_editor_code_execution_view_result"
                | "text_editor_code_execution_create_result"
                | "text_editor_code_execution_str_replace_result"
        ),
        _ => false,
    };
    if !valid {
        return Err(AnthropicServerToolFault::InvalidState);
    }
    Ok((Some(1), Vec::new()))
}

fn classify_advisor_result(
    content: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), AnthropicServerToolFault> {
    let content = content
        .as_object()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    match required_nonempty_str(content, "type")? {
        "advisor_result" => {
            required_str(content, "text")?;
        }
        "advisor_redacted_result" => {
            required_nonempty_str(content, "encrypted_content")?;
        }
        _ => return Err(AnthropicServerToolFault::InvalidState),
    }
    Ok((Some(1), Vec::new()))
}

fn classify_tool_search_result(
    content: &serde_json::Value,
) -> Result<(Option<u32>, Vec<ServerToolSource>), AnthropicServerToolFault> {
    let content = content
        .as_object()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    if required_nonempty_str(content, "type")? != "tool_search_tool_search_result" {
        return Err(AnthropicServerToolFault::InvalidState);
    }
    let references = content
        .get("tool_references")
        .and_then(serde_json::Value::as_array)
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    for reference in references {
        let reference = reference
            .as_object()
            .ok_or(AnthropicServerToolFault::InvalidState)?;
        if required_nonempty_str(reference, "type")? != "tool_reference"
            || !safe_identifier(required_nonempty_str(reference, "tool_name")?)
        {
            return Err(AnthropicServerToolFault::InvalidState);
        }
    }
    Ok((checked_count(references.len())?, Vec::new()))
}

fn public_source(
    object: &serde_json::Map<String, serde_json::Value>,
    url_key: &'static str,
    title: Option<&serde_json::Value>,
) -> Result<ServerToolSource, AnthropicServerToolFault> {
    let url = required_nonempty_str(object, url_key)?;
    let title = match title {
        Some(value) => Some(
            value
                .as_str()
                .ok_or(AnthropicServerToolFault::InvalidState)?,
        ),
        None => None,
    };
    ServerToolSource::new(url, title).map_err(|_| AnthropicServerToolFault::InvalidState)
}

fn classify_citations(
    block: &serde_json::Map<String, serde_json::Value>,
    output: &mut Vec<UrlCitation>,
) -> Result<(), AnthropicServerToolFault> {
    let Some(citations) = block.get("citations") else {
        return Ok(());
    };
    let citations = citations
        .as_array()
        .ok_or(AnthropicServerToolFault::InvalidState)?;
    for citation in citations {
        let citation = citation
            .as_object()
            .ok_or(AnthropicServerToolFault::InvalidState)?;
        if required_nonempty_str(citation, "type")? != "web_search_result_location" {
            continue;
        }
        let url = required_nonempty_str(citation, "url")?;
        let title = optional_nonempty_str(citation, "title")?;
        let cited_text = required_str(citation, "cited_text")?;
        required_nonempty_str(citation, "encrypted_index")?;
        output.push(
            UrlCitation::new(url, title, Some(cited_text), None, None)
                .map_err(|_| AnthropicServerToolFault::InvalidState)?,
        );
    }
    Ok(())
}

fn checked_count(count: usize) -> Result<Option<u32>, AnthropicServerToolFault> {
    u32::try_from(count)
        .map(Some)
        .map_err(|_| AnthropicServerToolFault::InvalidState)
}

fn validate_state_route(
    model: &str,
    state: &ProviderStateItem,
) -> Result<(), AnthropicServerToolFault> {
    state
        .validate()
        .map_err(|_| AnthropicServerToolFault::InvalidState)?;
    if state.provider() != "anthropic"
        || state.model() != model
        || state.protocol() != ProviderProtocol::AnthropicMessages
        || state.kind() != ProviderStateKind::AnthropicMessage
    {
        return Err(AnthropicServerToolFault::WrongRoute);
    }
    Ok(())
}

/// Pending/completed/failed state of one provider-executed call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicServerToolOutcome {
    /// Call is present without its paired result.
    Pending,
    /// Paired result completed without a provider-declared error.
    Completed,
    /// Paired result carries an error discriminator or MCP `is_error` marker.
    Failed,
}

/// Safe normalized facts for one call while exact state remains separate.
#[derive(Clone, PartialEq)]
pub struct AnthropicServerToolCall {
    kind: AnthropicServerToolKind,
    call: ServerToolCall,
    result: Option<ServerToolResult>,
}

impl AnthropicServerToolCall {
    /// Provider tool family.
    #[must_use]
    pub const fn kind(&self) -> AnthropicServerToolKind {
        self.kind
    }

    /// Actual provider call with its original id and redacted-in-Debug input.
    #[must_use]
    pub const fn call(&self) -> &ServerToolCall {
        &self.call
    }

    /// Paired normalized result, if the provider completed the call.
    #[must_use]
    pub const fn result(&self) -> Option<&ServerToolResult> {
        self.result.as_ref()
    }

    /// Safe completion class.
    #[must_use]
    pub fn outcome(&self) -> AnthropicServerToolOutcome {
        match self.result.as_ref().map(ServerToolResult::outcome) {
            None => AnthropicServerToolOutcome::Pending,
            Some(heycode_core::ServerToolOutcome::Success) => AnthropicServerToolOutcome::Completed,
            Some(heycode_core::ServerToolOutcome::Error) => AnthropicServerToolOutcome::Failed,
        }
    }
}

impl std::fmt::Debug for AnthropicServerToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicServerToolCall")
            .field("kind", &self.kind)
            .field("id", self.call.id())
            .field("outcome", &self.outcome())
            .finish()
    }
}

/// Complete provider classification plus unchanged assistant replay state.
#[derive(Clone, PartialEq)]
pub struct AnthropicServerToolClassification {
    state: ProviderStateItem,
    calls: Vec<AnthropicServerToolCall>,
    citations: Vec<UrlCitation>,
    client_tool_count: usize,
}

impl AnthropicServerToolClassification {
    /// Exact unchanged assistant item required for provider replay.
    #[must_use]
    pub const fn state(&self) -> &ProviderStateItem {
        &self.state
    }

    /// Calls in original assistant-content order.
    #[must_use]
    pub fn calls(&self) -> &[AnthropicServerToolCall] {
        &self.calls
    }

    /// Public web-search citations. Opaque indexes remain only in exact state.
    #[must_use]
    pub fn citations(&self) -> &[UrlCitation] {
        &self.citations
    }

    /// Interpret a normalized terminal reason against pending calls.
    ///
    /// # Errors
    /// Pause with client calls, tool-call finish without client calls, or a
    /// terminal non-continuation with unresolved server calls fails closed.
    pub fn continuation(
        &self,
        finish: FinishReason,
        plan: &AnthropicServerToolPlan,
    ) -> Result<AnthropicServerToolContinuation, AnthropicServerToolFault> {
        let pending_call_ids = self
            .calls
            .iter()
            .filter(|call| call.result.is_none())
            .map(|call| call.call.id().clone())
            .collect::<Vec<_>>();
        match finish {
            FinishReason::Pause if self.client_tool_count == 0 => Ok(
                AnthropicServerToolContinuation::Pause(AnthropicPendingPauseState {
                    state: self.state.clone(),
                    pending_call_ids,
                    required_provider_option: plan.provider_option()?,
                }),
            ),
            FinishReason::Pause => Err(AnthropicServerToolFault::InvalidState),
            FinishReason::ToolCalls if self.client_tool_count > 0 => {
                Ok(AnthropicServerToolContinuation::AwaitingClientTools { pending_call_ids })
            }
            FinishReason::ToolCalls => Err(AnthropicServerToolFault::InvalidState),
            FinishReason::Stop | FinishReason::Length if pending_call_ids.is_empty() => {
                Ok(AnthropicServerToolContinuation::Complete)
            }
            FinishReason::Stop | FinishReason::Length => {
                Err(AnthropicServerToolFault::InvalidState)
            }
        }
    }
}

impl std::fmt::Debug for AnthropicServerToolClassification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicServerToolClassification")
            .field(
                "kinds",
                &self.calls.iter().map(|call| call.kind).collect::<Vec<_>>(),
            )
            .field("call_count", &self.calls.len())
            .field("citation_count", &self.citations.len())
            .field("client_tool_count", &self.client_tool_count)
            .finish()
    }
}

/// Exact continuation action after classifying one complete assistant item.
#[derive(Clone, PartialEq)]
pub enum AnthropicServerToolContinuation {
    /// No provider/client call remains to continue.
    Complete,
    /// Client results must be sent; server calls listed here remain pending.
    AwaitingClientTools {
        /// Actual pending provider call ids, never synthesized.
        pending_call_ids: Vec<CallId>,
    },
    /// Provider loop paused and must replay the complete assistant item.
    Pause(AnthropicPendingPauseState),
}

impl std::fmt::Debug for AnthropicServerToolContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete => formatter.write_str("Complete"),
            Self::AwaitingClientTools { pending_call_ids } => formatter
                .debug_struct("AwaitingClientTools")
                .field("pending_count", &pending_call_ids.len())
                .finish(),
            Self::Pause(state) => std::fmt::Debug::fmt(state, formatter),
        }
    }
}

/// Exact pause replay item plus the identical required tool configuration.
#[derive(Clone, PartialEq)]
pub struct AnthropicPendingPauseState {
    state: ProviderStateItem,
    pending_call_ids: Vec<CallId>,
    required_provider_option: ProviderRequestOption,
}

impl AnthropicPendingPauseState {
    /// Exact unchanged assistant provider state.
    #[must_use]
    pub const fn state(&self) -> &ProviderStateItem {
        &self.state
    }

    /// Actual unresolved provider call ids.
    #[must_use]
    pub fn pending_call_ids(&self) -> &[CallId] {
        &self.pending_call_ids
    }

    /// Exact durable tool configuration that must remain on the resumed call.
    #[must_use]
    pub const fn required_provider_option(&self) -> &ProviderRequestOption {
        &self.required_provider_option
    }

    /// Clone the lossless provider item into the shared request input plane.
    #[must_use]
    pub fn replay_input(&self) -> InferenceInput {
        InferenceInput::ProviderState(self.state.clone())
    }
}

impl std::fmt::Debug for AnthropicPendingPauseState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicPendingPauseState")
            .field("pending_count", &self.pending_call_ids.len())
            .field("required_option", &self.required_provider_option)
            .finish()
    }
}

/// Discover beta values required by historical provider blocks even when the
/// live tool definition has been removed from a later request.
///
/// # Errors
/// Invalid Anthropic provider-state shape fails rather than dropping a beta
/// that the API requires to parse the historical block.
pub fn historical_server_tool_beta_headers(
    inputs: &[InferenceInput],
) -> Result<Vec<&'static str>, AnthropicServerToolFault> {
    let mut betas = BTreeSet::new();
    for input in inputs {
        let InferenceInput::ProviderState(state) = input else {
            continue;
        };
        if state.provider() != "anthropic"
            || state.protocol() != ProviderProtocol::AnthropicMessages
            || state.kind() != ProviderStateKind::AnthropicMessage
        {
            continue;
        }
        state
            .validate()
            .map_err(|_| AnthropicServerToolFault::InvalidState)?;
        let content = state
            .data()
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or(AnthropicServerToolFault::InvalidState)?;
        for block in content {
            let block = block
                .as_object()
                .ok_or(AnthropicServerToolFault::InvalidState)?;
            let block_type = required_nonempty_str(block, "type")?;
            if block_type == "advisor_tool_result"
                || (block_type == "server_tool_use"
                    && block.get("name").and_then(serde_json::Value::as_str) == Some("advisor"))
            {
                betas.insert(ANTHROPIC_ADVISOR_BETA);
            }
            if matches!(block_type, "mcp_tool_use" | "mcp_tool_result") {
                betas.insert(ANTHROPIC_MCP_CONNECTOR_BETA);
            }
        }
    }
    Ok(betas.into_iter().collect())
}

/// Closed server-tool refusal that never carries provider/configuration text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnthropicServerToolFault {
    /// Tool-specific configuration is invalid.
    InvalidConfiguration,
    /// Exact documentation says the selected model/pair does not support it.
    UnsupportedCapability,
    /// No exact support evidence exists for the selected model/pair.
    UnprovenCapability,
    /// Provider/model/protocol/state-kind identity is wrong.
    WrongRoute,
    /// Call/result/citation blocks are malformed, duplicated or mismatched.
    InvalidState,
}

impl std::fmt::Display for AnthropicServerToolFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "Anthropic server-tool configuration is invalid",
            Self::UnsupportedCapability => "Anthropic server tool is unsupported by this model",
            Self::UnprovenCapability => "Anthropic server-tool capability is unproven",
            Self::WrongRoute => "Anthropic server-tool state route is invalid",
            Self::InvalidState => "Anthropic server-tool state is malformed",
        })
    }
}

impl std::error::Error for AnthropicServerToolFault {}

fn required_nonempty_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<&'a str, AnthropicServerToolFault> {
    let value = required_str(object, key)?;
    if value.is_empty() {
        Err(AnthropicServerToolFault::InvalidState)
    } else {
        Ok(value)
    }
}

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<&'a str, AnthropicServerToolFault> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or(AnthropicServerToolFault::InvalidState)
}

fn optional_nonempty_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<Option<&'a str>, AnthropicServerToolFault> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(AnthropicServerToolFault::InvalidState),
    }
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value.as_bytes().iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}
