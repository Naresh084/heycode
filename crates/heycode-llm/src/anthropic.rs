//! Anthropic Messages protocol adapter over the shared raw HTTP/SSE service.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures::StreamExt as _;

use crate::{
    AuthenticationBinding, ChatMessage, FinishReason, InferenceAdapter, InferenceEvent,
    InferenceInput, InferenceStream, InferenceTarget, LlmError, ModelDescriptor, NativeFeature,
    ProviderDescriptor, ProviderProtocol, ProviderStateItem, ProviderStateKind, ReasoningEffortId,
    ReasoningEffortOptions, RequestDraft, ResolveError, ResolveSpec, ResolvedCall, Role,
    StreamItemKind, TokenUsage, ToolSpec, resolve_request,
};

/// Authentication header dialect for an Anthropic Messages-compatible route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicAuthWire {
    /// Native `x-api-key: <secret>` authentication.
    XApiKey,
    /// Gateway `Authorization: Bearer <secret>` authentication.
    Bearer,
}

/// Whether returned thinking is summarized or omitted while its signature is
/// still retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinkingDisplay {
    /// Return the provider's safe thinking summary.
    Summarized,
    /// Return only the opaque continuity signature.
    Omitted,
}

impl AnthropicThinkingDisplay {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// Exact Messages thinking request chosen for one canonical reasoning id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicThinkingMode {
    /// Explicitly disable thinking.
    Disabled {
        /// Optional provider `effort` value.
        wire_effort: Option<String>,
    },
    /// Let a compatible model choose its thinking budget.
    Adaptive {
        /// Optional display policy.
        display: Option<AnthropicThinkingDisplay>,
        /// Optional provider `effort` value.
        wire_effort: Option<String>,
    },
    /// Use legacy/manual thinking with an explicit token budget.
    Enabled {
        /// Manual thinking token budget.
        budget_tokens: u64,
        /// Optional display policy.
        display: Option<AnthropicThinkingDisplay>,
        /// Optional provider `effort` value.
        wire_effort: Option<String>,
        /// Whether route-proven interleaved accounting permits this budget to
        /// meet or exceed the per-request output cap.
        interleaved_budget: bool,
    },
}

impl AnthropicThinkingMode {
    /// Build an explicit disabled choice.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled { wire_effort: None }
    }

    /// Build an adaptive choice.
    #[must_use]
    pub const fn adaptive(display: Option<AnthropicThinkingDisplay>) -> Self {
        Self::Adaptive {
            display,
            wire_effort: None,
        }
    }

    /// Build a manual budget choice.
    #[must_use]
    pub const fn enabled(budget_tokens: u64, display: Option<AnthropicThinkingDisplay>) -> Self {
        Self::Enabled {
            budget_tokens,
            display,
            wire_effort: None,
            interleaved_budget: false,
        }
    }

    /// Attach the exact provider `output_config.effort` value for this choice.
    #[must_use]
    pub fn with_wire_effort(mut self, effort: impl Into<String>) -> Self {
        match &mut self {
            Self::Disabled { wire_effort }
            | Self::Adaptive { wire_effort, .. }
            | Self::Enabled { wire_effort, .. } => *wire_effort = Some(effort.into()),
        }
        self
    }

    /// Use the manual interleaved-thinking budget rule, which permits a
    /// thinking budget at or above `max_tokens` on routes that prove it.
    #[must_use]
    pub fn with_interleaved_budget(mut self) -> Self {
        if let Self::Enabled {
            interleaved_budget, ..
        } = &mut self
        {
            *interleaved_budget = true;
        }
        self
    }

    const fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled { .. })
    }

    fn wire_effort(&self) -> Option<&str> {
        match self {
            Self::Disabled { wire_effort }
            | Self::Adaptive { wire_effort, .. }
            | Self::Enabled { wire_effort, .. } => wire_effort.as_deref(),
        }
    }

    fn request_value(&self) -> serde_json::Value {
        match self {
            Self::Disabled { .. } => serde_json::json!({"type":"disabled"}),
            Self::Adaptive { display, .. } => {
                let mut value = serde_json::json!({"type":"adaptive"});
                if let Some(display) = display {
                    value["display"] = serde_json::json!(display.wire_name());
                }
                value
            }
            Self::Enabled {
                budget_tokens,
                display,
                ..
            } => {
                let mut value = serde_json::json!({
                    "type":"enabled",
                    "budget_tokens":budget_tokens,
                });
                if let Some(display) = display {
                    value["display"] = serde_json::json!(display.wire_name());
                }
                value
            }
        }
    }
}

/// Closed provider-result normalization failure with no provider content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnthropicServerToolNormalizationFault {
    /// The complete result block does not satisfy its provider-owned schema.
    #[error("invalid Anthropic server-tool result")]
    InvalidResult,
}

/// Provider-owned exact normalizer for one complete Messages result block.
pub trait AnthropicServerToolResultNormalizer: Send + Sync {
    /// Stable schema/dialect id included in the durable provider option.
    fn id(&self) -> &'static str;

    /// Normalize one already correlated complete result block.
    ///
    /// # Errors
    /// Malformed provider data returns only a closed fault.
    fn normalize(
        &self,
        block: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<heycode_core::ServerToolResult, AnthropicServerToolNormalizationFault>;
}

/// Exact Messages server-tool call/result correlation route.
#[derive(Clone)]
pub struct AnthropicServerToolRoute {
    block_type: &'static str,
    logical: String,
    provider_name: Option<String>,
    mcp_server_names: BTreeSet<String>,
    result_types: BTreeSet<String>,
    usage_field: Option<String>,
    result_normalizer: Option<Arc<dyn AnthropicServerToolResultNormalizer>>,
}

impl AnthropicServerToolRoute {
    /// Configure one exact `server_tool_use.name` and its possible result
    /// block types.
    ///
    /// # Errors
    /// Unsafe/blank identifiers, empty results or an invalid usage field fail.
    pub fn named(
        logical: impl Into<String>,
        provider_name: impl Into<String>,
        result_types: Vec<String>,
        usage_field: Option<String>,
    ) -> Result<Self, LlmError> {
        let route = Self {
            block_type: "server_tool_use",
            logical: logical.into(),
            provider_name: Some(provider_name.into()),
            mcp_server_names: BTreeSet::new(),
            result_types: result_types.into_iter().collect(),
            usage_field,
            result_normalizer: None,
        };
        route.validate()?;
        Ok(route)
    }

    /// Configure the Messages MCP connector call/result family. Remote tool
    /// names remain provider data and are validated on the returned block.
    ///
    /// # Errors
    /// The built-in route is validated with the same contract as named rows.
    pub fn mcp() -> Result<Self, LlmError> {
        let route = Self {
            block_type: "mcp_tool_use",
            logical: "remote_mcp".to_owned(),
            provider_name: None,
            mcp_server_names: BTreeSet::new(),
            result_types: BTreeSet::from(["mcp_tool_result".to_owned()]),
            usage_field: None,
            result_normalizer: None,
        };
        route.validate()?;
        Ok(route)
    }

    /// Restrict an MCP route to exact configured server names.
    ///
    /// # Errors
    /// Only MCP routes accept a nonempty unique safe server-name set.
    pub fn with_mcp_server_names(mut self, server_names: Vec<String>) -> Result<Self, LlmError> {
        if self.block_type != "mcp_tool_use" || server_names.is_empty() {
            return Err(invalid("Messages MCP server-name restriction is invalid"));
        }
        let count = server_names.len();
        self.mcp_server_names = server_names.into_iter().collect();
        if self.mcp_server_names.is_empty()
            || self.mcp_server_names.len() != count
            || self
                .mcp_server_names
                .iter()
                .any(|value| !safe_server_tool_identifier(value))
        {
            return Err(invalid("Messages MCP server-name restriction is invalid"));
        }
        self.validate()?;
        Ok(self)
    }

    /// Bind the provider-owned exact result normalizer for this route.
    ///
    /// # Errors
    /// Unsafe normalizer ids fail before the plan can become durable.
    pub fn with_result_normalizer(
        mut self,
        normalizer: Arc<dyn AnthropicServerToolResultNormalizer>,
    ) -> Result<Self, LlmError> {
        if !safe_server_tool_identifier(normalizer.id()) {
            return Err(invalid("Messages server-tool normalizer id is invalid"));
        }
        self.result_normalizer = Some(normalizer);
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), LlmError> {
        let provider_name = self.provider_name.as_deref().unwrap_or("remote_mcp");
        heycode_core::ServerToolCall::new(
            heycode_core::CallId::from_raw("validation"),
            self.logical.clone(),
            provider_name,
            serde_json::json!({}),
        )
        .map_err(|_| invalid("Messages server-tool route identity is invalid"))?;
        if !matches!(self.block_type, "server_tool_use" | "mcp_tool_use")
            || (self.block_type == "server_tool_use" && self.provider_name.is_none())
            || self.result_types.is_empty()
            || self
                .result_types
                .iter()
                .any(|value| !safe_server_tool_identifier(value))
            || self
                .usage_field
                .as_deref()
                .is_some_and(|value| !safe_server_tool_identifier(value))
            || self
                .result_normalizer
                .as_ref()
                .is_some_and(|normalizer| !safe_server_tool_identifier(normalizer.id()))
        {
            return Err(invalid("Messages server-tool route is invalid"));
        }
        Ok(())
    }

    fn matches_call(
        &self,
        block_type: &str,
        provider_name: &str,
        mcp_server_name: Option<&str>,
    ) -> bool {
        self.block_type == block_type
            && self
                .provider_name
                .as_deref()
                .is_none_or(|expected| expected == provider_name)
            && (self.block_type != "mcp_tool_use"
                || self.mcp_server_names.is_empty()
                || mcp_server_name.is_some_and(|name| self.mcp_server_names.contains(name)))
    }
}

impl PartialEq for AnthropicServerToolRoute {
    fn eq(&self, other: &Self) -> bool {
        self.block_type == other.block_type
            && self.logical == other.logical
            && self.provider_name == other.provider_name
            && self.mcp_server_names == other.mcp_server_names
            && self.result_types == other.result_types
            && self.usage_field == other.usage_field
            && self
                .result_normalizer
                .as_ref()
                .map(|normalizer| normalizer.id())
                == other
                    .result_normalizer
                    .as_ref()
                    .map(|normalizer| normalizer.id())
    }
}

impl Eq for AnthropicServerToolRoute {}

impl std::fmt::Debug for AnthropicServerToolRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicServerToolRoute")
            .field("block_type", &self.block_type)
            .field("logical", &self.logical)
            .field("provider_name", &self.provider_name)
            .field("mcp_server_count", &self.mcp_server_names.len())
            .field("result_types", &self.result_types)
            .field("usage_field", &self.usage_field)
            .field(
                "result_normalizer",
                &self
                    .result_normalizer
                    .as_ref()
                    .map(|normalizer| normalizer.id()),
            )
            .finish()
    }
}

/// Exact provider-option-gated Messages request extension and parser routes.
#[derive(Debug, Clone, PartialEq)]
pub struct AnthropicServerToolPlan {
    option_kind: String,
    tools: Vec<serde_json::Value>,
    beta_values: Vec<String>,
    request_fields: Vec<(String, serde_json::Value)>,
    routes: Vec<AnthropicServerToolRoute>,
    deferred_tool_names: BTreeSet<String>,
    required_native_feature: Option<NativeFeature>,
    option_data: serde_json::Value,
}

impl AnthropicServerToolPlan {
    /// Validate a complete request extension and exact parser route map.
    ///
    /// # Errors
    /// Unsafe option/beta/request keys, duplicate fields/routes, malformed
    /// tool definitions or data that cannot enter the durable option plane
    /// fail before adapter publication.
    pub fn new(
        option_kind: impl Into<String>,
        tools: Vec<serde_json::Value>,
        beta_values: Vec<String>,
        request_fields: Vec<(String, serde_json::Value)>,
        routes: Vec<AnthropicServerToolRoute>,
    ) -> Result<Self, LlmError> {
        let option_kind = option_kind.into();
        let deferred_tool_names = BTreeSet::new();
        let option_data = anthropic_plan_data(
            &tools,
            &beta_values,
            &request_fields,
            &routes,
            &deferred_tool_names,
        );
        let plan = Self {
            option_kind,
            tools,
            beta_values,
            request_fields,
            routes,
            deferred_tool_names,
            required_native_feature: None,
            option_data,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Require an existing shared native capability in addition to the exact
    /// provider option.
    #[must_use]
    pub fn with_required_native_feature(mut self, feature: NativeFeature) -> Self {
        self.required_native_feature = Some(feature);
        self
    }

    /// Mark exact offered tool names for deferred loading by a configured
    /// tool-search plan.
    ///
    /// # Errors
    /// Names must be nonempty, unique and safe provider tool identifiers.
    pub fn with_deferred_tool_names(mut self, names: Vec<String>) -> Result<Self, LlmError> {
        let count = names.len();
        self.deferred_tool_names = names.into_iter().collect();
        if self.deferred_tool_names.is_empty()
            || self.deferred_tool_names.len() != count
            || self
                .deferred_tool_names
                .iter()
                .any(|name| !safe_server_tool_identifier(name))
        {
            return Err(invalid("Messages deferred tool-name set is invalid"));
        }
        self.option_data = anthropic_plan_data(
            &self.tools,
            &self.beta_values,
            &self.request_fields,
            &self.routes,
            &self.deferred_tool_names,
        );
        self.validate()?;
        Ok(self)
    }

    /// Mint this plan's exact durable provider option after provider-owned
    /// model/account admission.
    ///
    /// # Errors
    /// The provider id or plan data cannot enter the durable option schema.
    pub fn provider_option(
        &self,
        provider: &str,
    ) -> Result<heycode_core::ProviderRequestOption, LlmError> {
        heycode_core::ProviderRequestOption::new(
            provider,
            self.option_kind.clone(),
            self.option_data.clone(),
        )
        .map_err(|_| invalid("Messages server-tool plan cannot become a provider option"))
    }

    fn validate(&self) -> Result<(), LlmError> {
        const RESERVED: &[&str] = &[
            "model",
            "max_tokens",
            "messages",
            "stream",
            "system",
            "tools",
            "tool_choice",
            "temperature",
            "thinking",
            "output_config",
        ];
        if !safe_server_tool_identifier(&self.option_kind)
            || self.tools.is_empty()
            || self.routes.is_empty()
        {
            return Err(invalid("Messages server-tool plan is invalid"));
        }
        let mut tool_values = Vec::new();
        for tool in &self.tools {
            let tool = tool
                .as_object()
                .ok_or_else(|| invalid("Messages server-tool definition must be an object"))?;
            required_nonempty_str(tool, "type", "Messages server-tool definition")?;
            let value = serde_json::Value::Object(tool.clone());
            if tool_values.contains(&value) {
                return Err(invalid("Messages server-tool definitions must be unique"));
            }
            tool_values.push(value);
        }
        let mut betas = BTreeSet::new();
        if self
            .beta_values
            .iter()
            .any(|value| !safe_header_value(value) || !betas.insert(value.as_str()))
        {
            return Err(invalid("Messages server-tool beta values are invalid"));
        }
        let mut fields = BTreeSet::new();
        for (field, _) in &self.request_fields {
            if !safe_server_tool_identifier(field)
                || RESERVED.contains(&field.as_str())
                || !fields.insert(field.as_str())
            {
                return Err(invalid("Messages server-tool request fields are invalid"));
            }
        }
        let mut calls = BTreeSet::new();
        for route in &self.routes {
            route.validate()?;
            if route.result_normalizer.is_none() {
                return Err(invalid(
                    "Messages configured server-tool route requires an exact result normalizer",
                ));
            }
            let identity = (route.block_type, route.provider_name.as_deref());
            if !calls.insert(identity) {
                return Err(invalid("Messages server-tool call routes must be unique"));
            }
        }
        let probe = self.provider_option("messages-plan")?;
        drop(probe);
        Ok(())
    }
}

fn anthropic_plan_data(
    tools: &[serde_json::Value],
    beta_values: &[String],
    request_fields: &[(String, serde_json::Value)],
    routes: &[AnthropicServerToolRoute],
    deferred_tool_names: &BTreeSet<String>,
) -> serde_json::Value {
    serde_json::json!({
        "tools":tools,
        "betas":beta_values,
        "request":request_fields.iter().cloned().collect::<BTreeMap<_,_>>(),
        "routes":routes.iter().map(|route| serde_json::json!({
            "block_type":route.block_type,
            "logical":route.logical,
            "provider_name":route.provider_name,
            "mcp_server_names":route.mcp_server_names,
            "result_types":route.result_types,
            "usage_field":route.usage_field,
            "result_normalizer":route.result_normalizer.as_ref().map(|normalizer| normalizer.id()),
        })).collect::<Vec<_>>(),
        "deferred_tool_names":deferred_tool_names,
    })
}

#[derive(Clone)]
enum AnthropicMessagesEndpoint {
    NativeBase,
    ExactModel { endpoint: String, model: String },
}

/// Data-driven request-wire differences for APIs that implement the
/// Anthropic Messages event protocol on a non-Anthropic endpoint.
///
/// The native dialect appends `/messages` to the configured base and carries
/// the selected model in the body. An exact-model dialect pins both the full
/// endpoint and its model, omits `model` from the body, and may add explicit
/// provider-owned body fields. The response parser remains the shared
/// Messages parser in either case.
#[derive(Clone)]
pub struct AnthropicMessagesDialect {
    endpoint: AnthropicMessagesEndpoint,
    body_fields: Vec<(String, serde_json::Value)>,
}

impl AnthropicMessagesDialect {
    /// Use the native Messages endpoint and body shape.
    #[must_use]
    pub const fn native() -> Self {
        Self {
            endpoint: AnthropicMessagesEndpoint::NativeBase,
            body_fields: Vec::new(),
        }
    }

    /// Pin one full endpoint to one exact selected model.
    ///
    /// Configuration validation rejects malformed endpoints or model ids; a
    /// request selecting another model fails before transport.
    #[must_use]
    pub fn exact_model_endpoint(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: AnthropicMessagesEndpoint::ExactModel {
                endpoint: endpoint.into(),
                model: model.into(),
            },
            body_fields: Vec::new(),
        }
    }

    /// Add one exact provider-owned top-level request-body field.
    ///
    /// Configuration validation rejects duplicate fields and collisions with
    /// the shared Messages serializer.
    #[must_use]
    pub fn with_body_field(mut self, field: impl Into<String>, value: serde_json::Value) -> Self {
        self.body_fields.push((field.into(), value));
        self
    }

    fn target(&self, base_url: &str) -> String {
        match &self.endpoint {
            AnthropicMessagesEndpoint::NativeBase => base_url.to_owned(),
            AnthropicMessagesEndpoint::ExactModel { endpoint, .. } => endpoint.clone(),
        }
    }

    fn request_url(&self, base_url: &str, selected_model: &str) -> Result<String, LlmError> {
        match &self.endpoint {
            AnthropicMessagesEndpoint::NativeBase => {
                Ok(format!("{}/messages", base_url.trim_end_matches('/')))
            }
            AnthropicMessagesEndpoint::ExactModel { endpoint, model }
                if model == selected_model =>
            {
                Ok(endpoint.clone())
            }
            AnthropicMessagesEndpoint::ExactModel { .. } => Err(invalid(
                "Messages exact endpoint does not match the selected model",
            )),
        }
    }

    fn accepts_model(&self, selected_model: &str) -> bool {
        match &self.endpoint {
            AnthropicMessagesEndpoint::NativeBase => true,
            AnthropicMessagesEndpoint::ExactModel { model, .. } => model == selected_model,
        }
    }

    fn apply_body(
        &self,
        selected_model: &str,
        body: &mut serde_json::Value,
    ) -> Result<(), LlmError> {
        let body = body
            .as_object_mut()
            .ok_or_else(|| invalid("Messages request body is not an object"))?;
        if matches!(&self.endpoint, AnthropicMessagesEndpoint::ExactModel { .. }) {
            if !self.accepts_model(selected_model) {
                return Err(invalid(
                    "Messages exact endpoint does not match the selected model",
                ));
            }
            body.remove("model");
        }
        for (field, value) in &self.body_fields {
            if body.insert(field.clone(), value.clone()).is_some() {
                return Err(invalid(
                    "Messages dialect body field collides with a serialized request field",
                ));
            }
        }
        Ok(())
    }
}

impl Default for AnthropicMessagesDialect {
    fn default() -> Self {
        Self::native()
    }
}

impl std::fmt::Debug for AnthropicMessagesDialect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("AnthropicMessagesDialect");
        match &self.endpoint {
            AnthropicMessagesEndpoint::NativeBase => {
                debug.field("endpoint_kind", &"native-base");
            }
            AnthropicMessagesEndpoint::ExactModel { model, .. } => {
                debug
                    .field("endpoint_kind", &"exact-model")
                    .field("model", model);
            }
        }
        debug
            .field("body_field_count", &self.body_fields.len())
            .finish()
    }
}

/// Reusable Anthropic Messages route configuration. Debug output is redacted.
#[derive(Clone)]
pub struct AnthropicMessagesConfig {
    provider: ProviderDescriptor,
    base_url: String,
    credential: crate::RouteCredential,
    auth_wire: AnthropicAuthWire,
    anthropic_version: Option<String>,
    dialect: AnthropicMessagesDialect,
    extra_headers: Vec<(String, String)>,
    thinking: Vec<(ReasoningEffortId, AnthropicThinkingMode)>,
    default_reasoning_effort: Option<ReasoningEffortId>,
    default_max_output_tokens: Option<u64>,
    server_tools: Vec<(NativeFeature, serde_json::Value)>,
    server_tool_plans: Vec<AnthropicServerToolPlan>,
    externally_consumed_provider_option_kinds: Vec<String>,
    retry_spec: crate::RetrySpec,
}

impl AnthropicMessagesConfig {
    /// Build one native-key Messages route using API version `2023-06-01`
    /// from a literal key captured for this adapter's lifetime.
    #[must_use]
    pub fn with_key(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_credential(provider, base_url, crate::RouteCredential::fixed(api_key))
    }

    /// Build one native-key Messages route whose credential is resolved once
    /// per operation.
    #[must_use]
    pub fn with_credential(
        provider: ProviderDescriptor,
        base_url: impl Into<String>,
        credential: crate::RouteCredential,
    ) -> Self {
        Self {
            provider,
            base_url: base_url.into(),
            credential,
            auth_wire: AnthropicAuthWire::XApiKey,
            anthropic_version: Some("2023-06-01".to_owned()),
            dialect: AnthropicMessagesDialect::native(),
            extra_headers: Vec::new(),
            thinking: Vec::new(),
            default_reasoning_effort: None,
            default_max_output_tokens: None,
            server_tools: Vec::new(),
            server_tool_plans: Vec::new(),
            externally_consumed_provider_option_kinds: Vec::new(),
            retry_spec: crate::RetrySpec::standard(),
        }
    }

    /// Select native-key or bearer authentication for a compatible route.
    #[must_use]
    pub fn with_auth_wire(mut self, auth_wire: AnthropicAuthWire) -> Self {
        self.auth_wire = auth_wire;
        self
    }

    /// Set or omit the `anthropic-version` header for this route.
    #[must_use]
    pub fn with_anthropic_version(mut self, version: Option<String>) -> Self {
        self.anthropic_version = version;
        self
    }

    /// Select one validated Messages request-wire dialect.
    #[must_use]
    pub fn with_dialect(mut self, dialect: AnthropicMessagesDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Attach validated-at-dispatch beta, attribution or gateway headers.
    #[must_use]
    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Attach exact canonical reasoning choices and their Messages wire modes.
    #[must_use]
    pub fn with_thinking(
        mut self,
        choices: Vec<(ReasoningEffortId, AnthropicThinkingMode)>,
        default: Option<ReasoningEffortId>,
    ) -> Self {
        self.thinking = choices;
        self.default_reasoning_effort = default;
        self
    }

    /// Attach an adapter-owned output default.
    #[must_use]
    pub fn with_default_max_output_tokens(mut self, value: Option<u64>) -> Self {
        self.default_max_output_tokens = value;
        self
    }

    /// Attach an explicit validated retry policy.
    #[must_use]
    pub fn with_retry_spec(mut self, retry_spec: crate::RetrySpec) -> Self {
        self.retry_spec = retry_spec;
        self
    }

    /// Attach one exact versioned provider server-tool definition to a native
    /// feature request.
    #[must_use]
    pub fn with_server_tool(
        mut self,
        feature: NativeFeature,
        definition: serde_json::Value,
    ) -> Self {
        self.server_tools.push((feature, definition));
        self
    }

    /// Register one exact provider-option-gated server-tool request/parser
    /// extension.
    #[must_use]
    pub fn with_server_tool_plan(mut self, plan: AnthropicServerToolPlan) -> Self {
        self.server_tool_plans.push(plan);
        self
    }

    /// Declare one exact provider-option kind consumed by a wrapping provider
    /// boundary before the generic Messages body serializer runs.
    ///
    /// The option remains durable request evidence; this declaration only
    /// prevents the shared server-tool selector from misclassifying it as a
    /// server-tool plan. Unknown undeclared kinds still fail before transport.
    #[must_use]
    pub fn with_externally_consumed_provider_option_kind(
        mut self,
        kind: impl Into<String>,
    ) -> Self {
        self.externally_consumed_provider_option_kinds
            .push(kind.into());
        self
    }

    /// Secret-free exact-route resolution proposal.
    #[must_use]
    pub fn resolve_spec(&self) -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::AnthropicMessages,
            target: InferenceTarget::Http {
                base_url: self.dialect.target(&self.base_url),
            },
            authentication: self.credential.binding(),
            default_max_output_tokens: self.default_max_output_tokens,
            reasoning_efforts: self
                .thinking
                .iter()
                .map(|(effort, _)| effort.clone())
                .collect(),
            default_reasoning_effort: self.default_reasoning_effort.clone(),
        }
    }

    fn thinking_mode(&self, effort: &ReasoningEffortId) -> Option<&AnthropicThinkingMode> {
        self.thinking
            .iter()
            .find_map(|(candidate, mode)| (candidate == effort).then_some(mode))
    }

    fn server_tools(&self, feature: NativeFeature) -> impl Iterator<Item = &serde_json::Value> {
        self.server_tools
            .iter()
            .filter_map(move |(candidate, tool)| (*candidate == feature).then_some(tool))
    }
}

impl std::fmt::Debug for AnthropicMessagesConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicMessagesConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .field("auth_wire", &self.auth_wire)
            .field("anthropic_version", &self.anthropic_version)
            .field("dialect", &self.dialect)
            .field("extra_header_count", &self.extra_headers.len())
            .field("thinking", &self.thinking)
            .field("default_reasoning_effort", &self.default_reasoning_effort)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("server_tool_count", &self.server_tools.len())
            .field("server_tool_plan_count", &self.server_tool_plans.len())
            .field(
                "externally_consumed_provider_option_kind_count",
                &self.externally_consumed_provider_option_kinds.len(),
            )
            .field("retry_spec", &self.retry_spec)
            .finish()
    }
}

/// Reusable Anthropic Messages protocol adapter.
#[derive(Clone)]
pub struct AnthropicMessagesAdapter {
    config: AnthropicMessagesConfig,
    http: heycode_http::HttpService,
}

impl AnthropicMessagesAdapter {
    /// Validate configuration and bind the shared HTTP service.
    ///
    /// # Errors
    /// Missing protocol declaration, invalid endpoint/key/header, duplicate or
    /// malformed reasoning choices, output defaults, or server-tool metadata.
    pub fn new(
        config: AnthropicMessagesConfig,
        http: heycode_http::HttpService,
    ) -> Result<Self, LlmError> {
        validate_config(&config)?;
        Ok(Self { config, http })
    }
}

impl InferenceAdapter for AnthropicMessagesAdapter {
    fn descriptor(&self) -> ProviderDescriptor {
        self.config.provider.clone()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        self.config.resolve_spec().authentication
    }

    fn reasoning_effort_options(
        &self,
        model: &ModelDescriptor,
    ) -> Result<Option<ReasoningEffortOptions>, ResolveError> {
        let spec = self.config.resolve_spec();
        ReasoningEffortOptions::for_model(
            model,
            spec.reasoning_efforts,
            spec.default_reasoning_effort,
        )
    }

    fn resolve(
        &self,
        mut draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        if !self.config.dialect.accepts_model(&model.id) {
            return Err(ResolveError::InvalidRequest {
                field: "model",
                message: "Messages exact endpoint does not match the selected model".to_owned(),
            });
        }
        let server_tools = selected_anthropic_server_tools(
            &self.config,
            &draft.provider_options,
            &draft.native_features,
            &draft.inputs,
        )?;
        validate_draft(&self.config, &draft, &server_tools)?;
        let retry_spec = if requires_no_replay(&draft) || !server_tools.tools.is_empty() {
            self.config.retry_spec.clone().disable_replay()
        } else {
            self.config.retry_spec.clone()
        };
        let selected = selected_thinking_mode(&self.config, &draft, model);
        if selected.is_some_and(AnthropicThinkingMode::is_enabled) {
            draft.temperature = None;
        }
        let call = resolve_request(
            &self.config.provider,
            draft,
            model,
            &self.config.resolve_spec(),
        )?
        .with_retry_spec(retry_spec);
        let max_output_tokens =
            call.max_output_tokens()
                .ok_or_else(|| ResolveError::InvalidAdapter {
                    field: "default_max_output_tokens",
                    message: "Messages requires an explicit or adapter-defaulted output cap"
                        .to_owned(),
                })?;
        if let Some(AnthropicThinkingMode::Enabled {
            budget_tokens,
            interleaved_budget,
            ..
        }) = call
            .reasoning_effort()
            .and_then(|effort| self.config.thinking_mode(effort))
            && !interleaved_budget
            && *budget_tokens >= max_output_tokens
        {
            let message = "manual thinking budget must be less than the effective output cap unless the route proves interleaved accounting".to_owned();
            return if call.defaults().max_output_tokens {
                Err(ResolveError::InvalidAdapter {
                    field: "default_max_output_tokens",
                    message,
                })
            } else {
                Err(ResolveError::InvalidRequest {
                    field: "max_output_tokens",
                    message,
                })
            };
        }
        Ok(call)
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.stream_cancellable(call, tokio_util::sync::CancellationToken::new())
    }

    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> InferenceStream {
        let server_tools = match selected_anthropic_server_tools(
            &self.config,
            call.provider_options(),
            call.native_features(),
            call.inputs(),
        ) {
            Ok(selection) => selection,
            Err(_) => return one_error(invalid("resolved Messages server-tool option is invalid")),
        };
        let server_correlation =
            match anthropic_server_tool_replay_state(call.inputs(), &server_tools) {
                Ok(correlation) => correlation,
                Err(error) => return one_error(error),
            };
        let body = match messages_request_body(&call, &self.config, &server_tools) {
            Ok(body) => body,
            Err(error) => return one_error(error),
        };
        let headers = match anthropic_request_headers(&self.config, &server_tools) {
            Ok(headers) => headers,
            Err(error) => return one_error(error),
        };
        let url = match self
            .config
            .dialect
            .request_url(&self.config.base_url, call.model())
        {
            Ok(url) => url,
            Err(error) => return one_error(error),
        };
        let body = body.to_string().into_bytes();
        let provider = call.provider().to_owned();
        let model = call.model().to_owned();
        let client_tool_names = call
            .tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<BTreeSet<_>>();
        let retry_spec = call.retry_spec().clone();
        let config = self.config.clone();
        let http = self.http.clone();
        // Resolved once, before the first attempt: every retry of this one
        // operation reuses it, and the next operation resolves again.
        let credential = match self.config.credential.acquire() {
            Ok(credential) => credential,
            Err(error) => return one_error(LlmError::UnresolvedCredential(error)),
        };
        crate::retry::retrying_stream(retry_spec, cancellation, move |attempt_cancellation| {
            let mut request = match heycode_http::HttpSseRequest::post(url.clone(), body.clone())
                .and_then(|request| request.header("content-type", "application/json"))
            {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            let auth_request = match config.auth_wire {
                AnthropicAuthWire::XApiKey => request.header("x-api-key", credential.expose()),
                AnthropicAuthWire::Bearer => {
                    request.header("authorization", &format!("Bearer {}", credential.expose()))
                }
            };
            request = match auth_request {
                Ok(request) => request,
                Err(error) => return one_error(crate::classify_transport_error(error)),
            };
            if let Some(version) = &config.anthropic_version {
                request = match request.header("anthropic-version", version) {
                    Ok(request) => request,
                    Err(error) => return one_error(crate::classify_transport_error(error)),
                };
            }
            for (name, value) in &headers {
                request = match request.header(name, value) {
                    Ok(request) => request,
                    Err(error) => return one_error(crate::classify_transport_error(error)),
                };
            }
            let events = http.sse(request, attempt_cancellation);
            Box::pin(
                futures::stream::unfold(
                    AnthropicPhase::Read(
                        events,
                        Box::new(AnthropicParser::new(
                            provider.clone(),
                            model.clone(),
                            client_tool_names.clone(),
                            server_tools.clone(),
                            server_correlation.clone(),
                        )),
                    ),
                    drive_anthropic,
                )
                .flat_map(futures::stream::iter),
            )
        })
    }
}

fn validate_config(config: &AnthropicMessagesConfig) -> Result<(), LlmError> {
    if !config
        .provider
        .protocols
        .contains(&ProviderProtocol::AnthropicMessages)
    {
        return Err(invalid(
            "Messages adapter provider does not declare Anthropic Messages",
        ));
    }
    if config.credential.fixed_is_blank() {
        return Err(crate::retry::local_failure(
            crate::ProviderErrorClass::Authentication,
        ));
    }
    validate_dialect(config)?;
    let url = config.dialect.target(&config.base_url);
    let url = match &config.dialect.endpoint {
        AnthropicMessagesEndpoint::NativeBase => {
            format!("{}/messages", url.trim_end_matches('/'))
        }
        AnthropicMessagesEndpoint::ExactModel { .. } => url,
    };
    let mut request = heycode_http::HttpSseRequest::post(url, Vec::new())
        .and_then(|request| request.header("content-type", "application/json"))
        .map_err(map_transport_error)?;
    let probe = config.credential.probe_value();
    request = match config.auth_wire {
        AnthropicAuthWire::XApiKey => request.header("x-api-key", probe),
        AnthropicAuthWire::Bearer => request.header("authorization", &format!("Bearer {probe}")),
    }
    .map_err(map_transport_error)?;
    if let Some(version) = &config.anthropic_version {
        if version.is_empty() || version.trim() != version {
            return Err(invalid(
                "Messages API version must be non-blank with no surrounding whitespace",
            ));
        }
        request = request
            .header("anthropic-version", version)
            .map_err(map_transport_error)?;
    }
    let mut header_names = BTreeSet::new();
    for (name, value) in &config.extra_headers {
        let normalized = name.to_ascii_lowercase();
        if matches!(
            normalized.as_str(),
            "content-type" | "authorization" | "x-api-key" | "anthropic-version"
        ) || !header_names.insert(normalized)
        {
            return Err(invalid(
                "Messages extra headers must be unique and cannot replace protocol/auth headers",
            ));
        }
        request = request.header(name, value).map_err(map_transport_error)?;
    }
    drop(request);

    let mut efforts = BTreeSet::new();
    for (effort, mode) in &config.thinking {
        if !efforts.insert(effort.as_str()) {
            return Err(invalid(
                "Messages adapter has duplicate reasoning effort ids",
            ));
        }
        if let AnthropicThinkingMode::Enabled { budget_tokens, .. } = mode
            && *budget_tokens < 1_024
        {
            return Err(invalid(
                "Messages manual thinking budget must be at least 1024 tokens",
            ));
        }
        if mode
            .wire_effort()
            .is_some_and(|value| value.is_empty() || value.trim() != value || value.len() > 128)
        {
            return Err(invalid(
                "Messages wire effort must be 1..=128 bytes and trimmed",
            ));
        }
    }
    if config
        .default_reasoning_effort
        .as_ref()
        .is_some_and(|default| !efforts.contains(default.as_str()))
    {
        return Err(invalid(
            "Messages reasoning default is not in its exact choice list",
        ));
    }
    if config.default_max_output_tokens == Some(0) {
        return Err(invalid("Messages adapter output default must be positive"));
    }
    let mut tool_names = BTreeSet::new();
    for (feature, definition) in &config.server_tools {
        if *feature != NativeFeature::Web {
            return Err(invalid(
                "Messages currently accepts exact server-tool definitions for native web only",
            ));
        }
        let definition = definition
            .as_object()
            .ok_or_else(|| invalid("Messages server-tool definition must be an object"))?;
        required_nonempty_str(definition, "type", "Messages server tool")?;
        let name = required_nonempty_str(definition, "name", "Messages server tool")?;
        if !tool_names.insert(name) {
            return Err(invalid("Messages server-tool names must be unique"));
        }
    }
    let mut plan_kinds = BTreeSet::new();
    let mut plan_routes = BTreeSet::new();
    for plan in &config.server_tool_plans {
        plan.validate()?;
        if !plan_kinds.insert(plan.option_kind.as_str()) {
            return Err(invalid(
                "Messages server-tool provider-option kinds must be unique",
            ));
        }
        for route in &plan.routes {
            if !plan_routes.insert((route.block_type, route.provider_name.as_deref())) {
                return Err(invalid(
                    "Messages server-tool plans have overlapping call routes",
                ));
            }
        }
    }
    let mut consumed_option_kinds = BTreeSet::new();
    for kind in &config.externally_consumed_provider_option_kinds {
        if !safe_server_tool_identifier(kind)
            || plan_kinds.contains(kind.as_str())
            || !consumed_option_kinds.insert(kind.as_str())
        {
            return Err(invalid(
                "Messages externally consumed provider-option kinds are invalid",
            ));
        }
    }
    Ok(())
}

fn validate_dialect(config: &AnthropicMessagesConfig) -> Result<(), LlmError> {
    if let AnthropicMessagesEndpoint::ExactModel { endpoint, model } = &config.dialect.endpoint {
        if model.is_empty()
            || model.len() > 256
            || model.trim() != model
            || model.chars().any(char::is_control)
        {
            return Err(invalid("Messages exact endpoint model id is invalid"));
        }
        heycode_http::HttpSseRequest::post(endpoint, Vec::new()).map_err(map_transport_error)?;
    }
    let mut fields = BTreeSet::new();
    for (field, _) in &config.dialect.body_fields {
        if field.is_empty()
            || field.len() > 128
            || field.trim() != field
            || !field
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            || !fields.insert(field.as_str())
            || matches!(
                field.as_str(),
                "model"
                    | "max_tokens"
                    | "messages"
                    | "stream"
                    | "system"
                    | "container"
                    | "tools"
                    | "tool_choice"
                    | "temperature"
                    | "thinking"
                    | "output_config"
            )
        {
            return Err(invalid(
                "Messages dialect body fields must be unique safe provider-owned fields",
            ));
        }
        if field == "anthropic_version" && config.anthropic_version.is_some() {
            return Err(invalid(
                "Messages version cannot be sent in both a header and the request body",
            ));
        }
    }
    Ok(())
}

fn selected_thinking_mode<'a>(
    config: &'a AnthropicMessagesConfig,
    draft: &RequestDraft,
    model: &ModelDescriptor,
) -> Option<&'a AnthropicThinkingMode> {
    draft
        .reasoning_effort
        .as_ref()
        .and_then(|effort| config.thinking_mode(effort))
        .or_else(|| {
            (model.capabilities.reasoning == crate::CapabilitySupport::Supported)
                .then_some(config.default_reasoning_effort.as_ref())
                .flatten()
                .and_then(|effort| config.thinking_mode(effort))
        })
}

#[derive(Debug, Clone, Default)]
struct AnthropicServerToolSelection {
    tools: Vec<serde_json::Value>,
    beta_values: Vec<String>,
    request_fields: BTreeMap<String, serde_json::Value>,
    routes: Vec<AnthropicServerToolRoute>,
    replay_routes: Vec<AnthropicServerToolRoute>,
    deferred_tool_names: BTreeSet<String>,
}

impl AnthropicServerToolSelection {
    fn route_for_call(
        &self,
        block_type: &str,
        provider_name: &str,
        mcp_server_name: Option<&str>,
    ) -> Option<&AnthropicServerToolRoute> {
        self.routes
            .iter()
            .find(|route| route.matches_call(block_type, provider_name, mcp_server_name))
    }

    fn usage_routes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.routes.iter().filter_map(|route| {
            route
                .usage_field
                .as_deref()
                .map(|field| (field, route.logical.as_str()))
        })
    }

    fn replay_route_for_call(
        &self,
        block_type: &str,
        provider_name: &str,
        mcp_server_name: Option<&str>,
    ) -> Option<&AnthropicServerToolRoute> {
        self.replay_routes
            .iter()
            .find(|route| route.matches_call(block_type, provider_name, mcp_server_name))
    }
}

fn selected_anthropic_server_tools(
    config: &AnthropicMessagesConfig,
    options: &[heycode_core::ProviderRequestOption],
    native_features: &[NativeFeature],
    inputs: &[InferenceInput],
) -> Result<AnthropicServerToolSelection, ResolveError> {
    let mut selection = AnthropicServerToolSelection {
        replay_routes: config
            .server_tool_plans
            .iter()
            .flat_map(|plan| plan.routes.iter().cloned())
            .collect(),
        ..AnthropicServerToolSelection::default()
    };
    let mut option_kinds = BTreeSet::new();
    for option in options {
        let Some(plan) = config
            .server_tool_plans
            .iter()
            .find(|plan| plan.option_kind == option.kind())
        else {
            if config
                .externally_consumed_provider_option_kinds
                .iter()
                .any(|kind| kind == option.kind())
            {
                continue;
            }
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages route received an undeclared provider option kind".to_owned(),
            });
        };
        if plan.option_data != *option.data() {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages server-tool option does not match one exact configured plan"
                    .to_owned(),
            });
        }
        if !option_kinds.insert(option.kind()) {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages server-tool plan was selected twice".to_owned(),
            });
        }
        if plan
            .required_native_feature
            .is_some_and(|feature| !native_features.contains(&feature))
        {
            return Err(ResolveError::InvalidRequest {
                field: "native_features",
                message: "Messages server-tool plan is missing its required native capability"
                    .to_owned(),
            });
        }
        merge_anthropic_plan(&mut selection, plan)?;
    }
    for feature in native_features {
        for tool in config.server_tools(*feature) {
            let definition = tool
                .as_object()
                .ok_or_else(|| ResolveError::InvalidRequest {
                    field: "native_features",
                    message: "Messages server-tool definition is malformed".to_owned(),
                })?;
            let name = definition
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ResolveError::InvalidRequest {
                    field: "native_features",
                    message: "Messages native server-tool definition has no name".to_owned(),
                })?;
            selection.tools.push(tool.clone());
            selection.routes.push(
                AnthropicServerToolRoute::named(
                    name,
                    name,
                    vec![format!("{name}_tool_result")],
                    Some(format!("{name}_requests")),
                )
                .map_err(|_| ResolveError::InvalidRequest {
                    field: "native_features",
                    message: "Messages native server-tool route is invalid".to_owned(),
                })?,
            );
            if let Some(route) = selection.routes.last().cloned() {
                selection.replay_routes.push(route);
            }
        }
    }
    retain_anthropic_replay_betas(config, inputs, &mut selection)?;
    validate_anthropic_selection(&selection)?;
    Ok(selection)
}

fn retain_anthropic_replay_betas(
    config: &AnthropicMessagesConfig,
    inputs: &[InferenceInput],
    selection: &mut AnthropicServerToolSelection,
) -> Result<(), ResolveError> {
    for state in inputs.iter().filter_map(|input| match input {
        InferenceInput::ProviderState(state)
            if state.kind() == ProviderStateKind::AnthropicMessage =>
        {
            Some(state)
        }
        InferenceInput::Message(_) | InferenceInput::ProviderState(_) => None,
    }) {
        let content = state
            .data()
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ResolveError::InvalidRequest {
                field: "provider_state",
                message: "Messages provider state has no content array".to_owned(),
            })?;
        for block in content {
            let Some(block) = block.as_object() else {
                continue;
            };
            let Some(block_type) = block.get("type").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !is_server_tool_use(block_type) {
                continue;
            }
            let Some(name) = block.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            for plan in &config.server_tool_plans {
                let server_name = block.get("server_name").and_then(serde_json::Value::as_str);
                if plan
                    .routes
                    .iter()
                    .any(|route| route.matches_call(block_type, name, server_name))
                {
                    selection
                        .beta_values
                        .extend(plan.beta_values.iter().cloned());
                }
            }
        }
    }
    selection.beta_values.sort();
    selection.beta_values.dedup();
    Ok(())
}

fn merge_anthropic_plan(
    selection: &mut AnthropicServerToolSelection,
    plan: &AnthropicServerToolPlan,
) -> Result<(), ResolveError> {
    selection.tools.extend(plan.tools.iter().cloned());
    selection
        .beta_values
        .extend(plan.beta_values.iter().cloned());
    for (field, value) in &plan.request_fields {
        if selection
            .request_fields
            .insert(field.clone(), value.clone())
            .is_some()
        {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages server-tool plans collide on a request field".to_owned(),
            });
        }
    }
    selection.routes.extend(plan.routes.iter().cloned());
    selection
        .deferred_tool_names
        .extend(plan.deferred_tool_names.iter().cloned());
    Ok(())
}

fn validate_anthropic_selection(
    selection: &AnthropicServerToolSelection,
) -> Result<(), ResolveError> {
    let mut tools = Vec::new();
    for tool in &selection.tools {
        if tools.contains(tool) {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages server-tool definitions were selected twice".to_owned(),
            });
        }
        tools.push(tool.clone());
    }
    let mut calls = BTreeSet::new();
    for route in &selection.routes {
        let key = (route.block_type, route.provider_name.as_deref());
        if !calls.insert(key) {
            return Err(ResolveError::InvalidRequest {
                field: "provider_options",
                message: "Messages server-tool call routes overlap".to_owned(),
            });
        }
    }
    Ok(())
}

fn requires_no_replay(draft: &RequestDraft) -> bool {
    !draft.native_features.is_empty()
        || draft.inputs.iter().any(|input| {
            let InferenceInput::ProviderState(state) = input else {
                return false;
            };
            if state.data().get("container").is_some() {
                return true;
            }
            state
                .data()
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|content| {
                    content.iter().any(|block| {
                        let block_type = block.get("type").and_then(serde_json::Value::as_str);
                        block_type.is_some_and(is_server_tool_use)
                            || block_type == Some("compaction")
                            || (block_type == Some("tool_use")
                                && block
                                    .get("caller")
                                    .and_then(serde_json::Value::as_object)
                                    .and_then(|caller| caller.get("type"))
                                    .and_then(serde_json::Value::as_str)
                                    .is_some_and(|caller| caller != "direct"))
                    })
                })
        })
}

fn validate_draft(
    config: &AnthropicMessagesConfig,
    draft: &RequestDraft,
    server_tools: &AnthropicServerToolSelection,
) -> Result<(), ResolveError> {
    if draft.structured_output.is_some() {
        return request_error(
            "structured_output",
            "Messages structured-output dialect is not configured yet",
        );
    }
    for feature in &draft.native_features {
        match feature {
            NativeFeature::Web if config.server_tools(*feature).next().is_some() => {}
            NativeFeature::Web => {
                return request_error(
                    "native_features",
                    "native web requires an exact versioned Messages server-tool definition",
                );
            }
            NativeFeature::Compaction | NativeFeature::PromptCache => {
                return request_error(
                    "native_features",
                    "Messages compaction/cache request dialects are not configured yet",
                );
            }
        }
    }
    let requested_server_names = server_tools
        .routes
        .iter()
        .filter_map(|route| route.provider_name.as_deref())
        .collect::<BTreeSet<_>>();
    let requested_client_names = draft
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    if draft
        .tools
        .iter()
        .any(|tool| requested_server_names.contains(tool.name.as_str()))
    {
        return request_error(
            "tools",
            "client and server tools cannot share a Messages tool name",
        );
    }
    for input in &draft.inputs {
        match input {
            InferenceInput::ProviderState(state)
                if state.kind() != ProviderStateKind::AnthropicMessage =>
            {
                return request_error(
                    "provider_state",
                    "Messages route received non-Anthropic provider state",
                );
            }
            InferenceInput::ProviderState(state) => {
                state.validate().map_err(|_| ResolveError::InvalidRequest {
                    field: "provider_state",
                    message: "Messages provider state failed schema validation".to_owned(),
                })?;
                let state_data =
                    state
                        .data()
                        .as_object()
                        .ok_or_else(|| ResolveError::InvalidRequest {
                            field: "provider_state",
                            message: "Messages provider state data must be an object".to_owned(),
                        })?;
                if state_data
                    .keys()
                    .any(|key| !matches!(key.as_str(), "role" | "content" | "container"))
                {
                    return request_error(
                        "provider_state",
                        "Messages provider state contains unsupported top-level metadata",
                    );
                }
                let has_container =
                    match state_data.get("container").filter(|value| !value.is_null()) {
                        Some(container) => {
                            validate_response_container(container).map_err(|_| {
                                ResolveError::InvalidRequest {
                                    field: "provider_state",
                                    message: "Messages provider state container is malformed"
                                        .to_owned(),
                                }
                            })?;
                            true
                        }
                        None => false,
                    };
                let content = state_data
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "provider_state",
                        message: "Messages provider state has no content array".to_owned(),
                    })?;
                let mut has_programmatic_tool = false;
                for block in content {
                    let block = block
                        .as_object()
                        .ok_or_else(|| ResolveError::InvalidRequest {
                            field: "provider_state",
                            message: "Messages provider state block must be an object".to_owned(),
                        })?;
                    let block_type = block.get("type").and_then(serde_json::Value::as_str);
                    let Some(block_type) = block_type else {
                        return request_error(
                            "provider_state",
                            "Messages provider state block has no type",
                        );
                    };
                    validate_started_block(block_type, block).map_err(|_| {
                        ResolveError::InvalidRequest {
                            field: "provider_state",
                            message: "Messages provider state block is malformed".to_owned(),
                        }
                    })?;
                    has_programmatic_tool |=
                        block_type == "tool_use" && is_programmatic_tool_call(block);
                    if is_server_tool_use(block_type) {
                        block
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| ResolveError::InvalidRequest {
                                field: "provider_state",
                                message: "Messages server tool state has no name".to_owned(),
                            })?;
                    } else if block_type == "tool_use" {
                        let name = block
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| ResolveError::InvalidRequest {
                                field: "provider_state",
                                message: "Messages client tool state has no name".to_owned(),
                            })?;
                        if !requested_client_names.contains(name) {
                            return request_error(
                                "tools",
                                "Messages client-tool replay requires the same tool definition",
                            );
                        }
                    }
                }
                if has_programmatic_tool && !has_container {
                    return request_error(
                        "provider_state",
                        "Messages programmatic tool replay requires its continuation container",
                    );
                }
            }
            InferenceInput::Message(message) => {
                validate_neutral_message(message)?;
                if message.role == Role::Assistant
                    && message.tool_calls.as_ref().is_some_and(|calls| {
                        calls
                            .iter()
                            .any(|call| !requested_client_names.contains(call.name.as_str()))
                    })
                {
                    return request_error(
                        "tools",
                        "Messages client-tool replay requires the same tool definition",
                    );
                }
            }
        }
    }
    validate_tool_result_chronology(&draft.inputs)?;
    anthropic_server_tool_replay_state(&draft.inputs, server_tools).map_err(|_| {
        ResolveError::InvalidRequest {
            field: "provider_state",
            message: "Messages server-tool call/result correlation is invalid".to_owned(),
        }
    })?;
    Ok(())
}

fn validate_neutral_message(message: &ChatMessage) -> Result<(), ResolveError> {
    match message.role {
        Role::System => request_error(
            "inputs",
            "Messages system instructions belong in the top-level system slot",
        ),
        Role::User => {
            if message.tool_calls.is_some()
                || message.tool_call_id.is_some()
                || message.tool_result_is_error.is_some()
            {
                return request_error("inputs", "Messages user text has invalid tool metadata");
            }
            Ok(())
        }
        Role::Assistant => {
            if (!message.images.is_empty() || !message.documents.is_empty())
                || message.tool_call_id.is_some()
                || message.tool_result_is_error.is_some()
            {
                return request_error("inputs", "Messages assistant input cannot be a tool result");
            }
            if let Some(calls) = &message.tool_calls {
                let mut ids = BTreeSet::new();
                for call in calls {
                    if call.id.is_empty()
                        || call.id.trim() != call.id
                        || call.name.is_empty()
                        || call.name.trim() != call.name
                        || !ids.insert(call.id.as_str())
                        || serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .ok()
                            .is_none_or(|value| !value.is_object())
                    {
                        return request_error(
                            "inputs",
                            "Messages assistant tool calls require unique trimmed ids/names and object JSON input",
                        );
                    }
                }
            }
            Ok(())
        }
        Role::Tool => {
            if !message.images.is_empty() || !message.documents.is_empty() {
                return request_error("inputs", "Messages tool result cannot contain media");
            }
            if message.tool_calls.is_some()
                || message
                    .tool_call_id
                    .as_ref()
                    .is_none_or(|id| id.is_empty() || id.trim() != id)
            {
                return request_error(
                    "inputs",
                    "Messages tool results require one trimmed tool_use_id",
                );
            }
            Ok(())
        }
    }
}

fn anthropic_server_tool_replay_state(
    inputs: &[InferenceInput],
    server_tools: &AnthropicServerToolSelection,
) -> Result<AnthropicServerToolCorrelation, LlmError> {
    let mut correlation = AnthropicServerToolCorrelation::default();
    for state in inputs.iter().filter_map(|input| match input {
        InferenceInput::ProviderState(state)
            if state.kind() == ProviderStateKind::AnthropicMessage =>
        {
            Some(state)
        }
        InferenceInput::Message(_) | InferenceInput::ProviderState(_) => None,
    }) {
        let content = state
            .data()
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("Messages provider state has no content array"))?;
        for block in content {
            let block = block
                .as_object()
                .ok_or_else(|| invalid("Messages provider state block is not an object"))?;
            let block_type = required_nonempty_str(block, "type", "Messages provider state")?;
            if is_server_tool_use(block_type) {
                let id = required_nonempty_str(block, "id", "Messages server tool")?;
                let name = required_nonempty_str(block, "name", "Messages server tool")?;
                let server_name = block.get("server_name").and_then(serde_json::Value::as_str);
                let route = server_tools
                    .replay_route_for_call(block_type, name, server_name)
                    .cloned()
                    .ok_or_else(|| {
                        invalid("Messages server-tool replay omitted its exact definition")
                    })?;
                if !correlation.seen_calls.insert(id.to_owned()) {
                    return Err(invalid("Messages server tool id was replayed twice"));
                }
                correlation.pending.insert(id.to_owned(), route);
            } else if is_server_tool_result(block_type) {
                let id = required_nonempty_str(block, "tool_use_id", "Messages server result")?;
                let route = correlation.pending.get(id).ok_or_else(|| {
                    invalid("Messages server result has no preceding unique call")
                })?;
                if !route.result_types.contains(block_type)
                    || !correlation.results.insert(id.to_owned())
                {
                    return Err(invalid(
                        "Messages server result does not match one preceding unique call",
                    ));
                }
                correlation.pending.remove(id);
            }
        }
    }
    if correlation
        .pending
        .values()
        .any(|pending| !server_tools.routes.iter().any(|active| active == pending))
    {
        return Err(invalid(
            "Messages pending server-tool replay omitted its exact current definition",
        ));
    }
    Ok(correlation)
}

fn validate_tool_result_chronology(inputs: &[InferenceInput]) -> Result<(), ResolveError> {
    let mut pending = BTreeSet::new();
    let mut awaiting_assistant = false;
    for input in inputs {
        if !pending.is_empty() {
            let InferenceInput::Message(message) = input else {
                return request_error(
                    "inputs",
                    "Messages client tool calls must be followed immediately by their tool results",
                );
            };
            if message.role != Role::Tool {
                return request_error(
                    "inputs",
                    "Messages client tool calls must be followed immediately by their tool results",
                );
            }
            let id =
                message
                    .tool_call_id
                    .as_deref()
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "inputs",
                        message: "Messages tool result has no tool_use_id".to_owned(),
                    })?;
            if !pending.remove(id) {
                return request_error(
                    "inputs",
                    "Messages tool result does not match a pending client tool call",
                );
            }
            if pending.is_empty() {
                awaiting_assistant = true;
            }
            continue;
        }
        if awaiting_assistant {
            if !matches!(input, InferenceInput::ProviderState(_))
                && !matches!(input, InferenceInput::Message(message) if message.role == Role::Assistant)
            {
                return request_error(
                    "inputs",
                    "Messages tool-result batch must end the request or be followed by the assistant continuation",
                );
            }
            awaiting_assistant = false;
        }
        match input {
            InferenceInput::Message(message) if message.role == Role::Tool => {
                return request_error(
                    "inputs",
                    "Messages tool result has no immediately preceding client tool call",
                );
            }
            InferenceInput::Message(message) if message.role == Role::Assistant => {
                if let Some(calls) = &message.tool_calls {
                    pending.extend(calls.iter().map(|call| call.id.as_str()));
                }
            }
            InferenceInput::ProviderState(state) => {
                let content = state
                    .data()
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| ResolveError::InvalidRequest {
                        field: "provider_state",
                        message: "Messages provider state has no content array".to_owned(),
                    })?;
                for block in content {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use") {
                        let id = block
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| ResolveError::InvalidRequest {
                                field: "provider_state",
                                message: "Messages client tool block has no id".to_owned(),
                            })?;
                        if !pending.insert(id) {
                            return request_error(
                                "inputs",
                                "Messages assistant turn repeated a client tool id",
                            );
                        }
                    }
                }
            }
            InferenceInput::Message(_) => {}
        }
    }
    if pending.is_empty() {
        Ok(())
    } else {
        request_error(
            "inputs",
            "Messages client tool calls are missing immediate tool results",
        )
    }
}

fn request_error<T>(field: &'static str, message: impl Into<String>) -> Result<T, ResolveError> {
    Err(ResolveError::InvalidRequest {
        field,
        message: message.into(),
    })
}

fn messages_request_body(
    call: &ResolvedCall,
    config: &AnthropicMessagesConfig,
    server_tools: &AnthropicServerToolSelection,
) -> Result<serde_json::Value, LlmError> {
    if call.protocol() != ProviderProtocol::AnthropicMessages {
        return Err(invalid("resolved call protocol is not Anthropic Messages"));
    }
    let max_tokens = call
        .max_output_tokens()
        .ok_or_else(|| invalid("resolved Messages call has no output cap"))?;
    let mut messages = Vec::new();
    let mut pending_tool_results = Vec::new();
    let mut continuation_container = None;
    for input in call.inputs() {
        match input {
            InferenceInput::Message(message) if message.role == Role::Tool => {
                pending_tool_results.push(anthropic_tool_result_block(message)?);
            }
            InferenceInput::Message(message) => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                messages.push(anthropic_message(message)?);
            }
            InferenceInput::ProviderState(state) => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                let state = state
                    .data()
                    .as_object()
                    .ok_or_else(|| invalid("Messages provider state must be an object"))?;
                let role = state
                    .get("role")
                    .ok_or_else(|| invalid("Messages provider state has no role"))?
                    .clone();
                let content = state
                    .get("content")
                    .ok_or_else(|| invalid("Messages provider state has no content"))?
                    .clone();
                if let Some(container) = state.get("container").filter(|value| !value.is_null()) {
                    continuation_container =
                        Some(validate_response_container(container)?.to_owned());
                }
                messages.push(serde_json::json!({"role":role,"content":content}));
            }
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results);
    let mut body = serde_json::json!({
        "model":call.model(),
        "max_tokens":max_tokens,
        "messages":messages,
        "stream":true,
    });
    if let Some(system) = call.system() {
        body["system"] = serde_json::json!(system);
    }
    if let Some(container) = continuation_container {
        body["container"] = serde_json::json!(container);
    }
    let mut tools = call
        .tools()
        .iter()
        .map(anthropic_client_tool)
        .collect::<Vec<_>>();
    tools.extend(server_tools.tools.iter().cloned());
    for deferred in &server_tools.deferred_tool_names {
        let matches = tools
            .iter()
            .enumerate()
            .filter(|tool| {
                tool.1.get("name").and_then(serde_json::Value::as_str) == Some(deferred.as_str())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [index] = matches.as_slice() else {
            return Err(invalid(
                "Messages deferred tool name must match exactly one offered definition",
            ));
        };
        tools[*index]
            .as_object_mut()
            .ok_or_else(|| invalid("Messages deferred tool definition must be an object"))?
            .insert("defer_loading".to_owned(), serde_json::Value::Bool(true));
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!({
            "type":"auto",
            "disable_parallel_tool_use":false,
        });
    }
    if let Some(temperature) = call.temperature() {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(effort) = call.reasoning_effort() {
        let mode = config
            .thinking_mode(effort)
            .ok_or_else(|| invalid("resolved Messages effort has no thinking wire mode"))?;
        body["thinking"] = mode.request_value();
        if let Some(wire_effort) = mode.wire_effort() {
            body["output_config"] = serde_json::json!({"effort":wire_effort});
        }
    }
    for (field, value) in &server_tools.request_fields {
        body[field] = value.clone();
    }
    config.dialect.apply_body(call.model(), &mut body)?;
    Ok(body)
}

fn anthropic_request_headers(
    config: &AnthropicMessagesConfig,
    server_tools: &AnthropicServerToolSelection,
) -> Result<Vec<(String, String)>, LlmError> {
    let mut headers = Vec::new();
    let mut beta_values = BTreeSet::new();
    for (name, value) in &config.extra_headers {
        if name.eq_ignore_ascii_case("anthropic-beta") {
            for beta in value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if !safe_header_value(beta) {
                    return Err(invalid("Messages beta header contains an invalid value"));
                }
                beta_values.insert(beta.to_owned());
            }
        } else {
            headers.push((name.clone(), value.clone()));
        }
    }
    beta_values.extend(server_tools.beta_values.iter().cloned());
    if !beta_values.is_empty() {
        headers.push((
            "anthropic-beta".to_owned(),
            beta_values.into_iter().collect::<Vec<_>>().join(","),
        ));
    }
    Ok(headers)
}

fn anthropic_message(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    match message.role {
        Role::System => Err(invalid(
            "Messages system input appeared outside the top-level system slot",
        )),
        Role::User => {
            let mut content =
                Vec::with_capacity(message.images.len() + message.documents.len() + 1);
            content.extend(message.images.iter().map(|image| {
                serde_json::json!({
                    "type":"image",
                    "source":{
                        "type":"base64",
                        "media_type":image.media_type().as_str(),
                        "data":crate::vocab::image_base64(image),
                    }
                })
            }));
            content.extend(message.documents.iter().map(|document| {
                serde_json::json!({
                    "type":"document",
                    "source":{
                        "type":"base64",
                        "media_type":document.media_type().as_str(),
                        "data":crate::vocab::document_base64(document),
                    },
                    "title":document.filename(),
                })
            }));
            if !message.content.is_empty() {
                content.push(serde_json::json!({"type":"text","text":message.content}));
            }
            Ok(serde_json::json!({"role":"user","content":content}))
        }
        Role::Tool => Ok(serde_json::json!({
            "role":"user",
            "content":[anthropic_tool_result_block(message)?],
        })),
        Role::Assistant => {
            let mut content = Vec::new();
            if !message.content.is_empty() {
                content.push(serde_json::json!({"type":"text","text":message.content}));
            }
            if let Some(calls) = &message.tool_calls {
                for call in calls {
                    let input: serde_json::Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| invalid("Messages assistant tool input is not JSON"))?;
                    if !input.is_object() {
                        return Err(invalid(
                            "Messages assistant tool input must be a JSON object",
                        ));
                    }
                    content.push(serde_json::json!({
                        "type":"tool_use",
                        "id":call.id,
                        "name":call.name,
                        "input":input,
                    }));
                }
            }
            Ok(serde_json::json!({"role":"assistant","content":content}))
        }
    }
}

fn anthropic_tool_result_block(message: &ChatMessage) -> Result<serde_json::Value, LlmError> {
    let tool_use_id = message
        .tool_call_id
        .as_ref()
        .ok_or_else(|| invalid("Messages tool result has no tool_use_id"))?;
    let mut block = serde_json::json!({
        "type":"tool_result",
        "tool_use_id":tool_use_id,
        "content":message.content,
    });
    if message.tool_result_is_error == Some(true) {
        block["is_error"] = serde_json::json!(true);
    }
    Ok(block)
}

fn flush_tool_results(messages: &mut Vec<serde_json::Value>, pending: &mut Vec<serde_json::Value>) {
    if !pending.is_empty() {
        messages.push(serde_json::json!({
            "role":"user",
            "content":std::mem::take(pending),
        }));
    }
}

fn anthropic_client_tool(tool: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "name":tool.name,
        "description":tool.description,
        "input_schema":tool.parameters,
    })
}

enum AnthropicPhase {
    Read(heycode_http::SseEventStream, Box<AnthropicParser>),
    Done,
}

async fn drive_anthropic(
    phase: AnthropicPhase,
) -> Option<(Vec<Result<InferenceEvent, LlmError>>, AnthropicPhase)> {
    match phase {
        AnthropicPhase::Read(mut events, mut parser) => match events.next().await {
            Some(Ok(event)) => {
                let output = parser.event(event);
                let next = if parser.terminal {
                    AnthropicPhase::Done
                } else {
                    AnthropicPhase::Read(events, parser)
                };
                Some((output, next))
            }
            Some(Err(error)) => Some((vec![Err(map_transport_error(error))], AnthropicPhase::Done)),
            None => Some((parser.finish(), AnthropicPhase::Done)),
        },
        AnthropicPhase::Done => None,
    }
}

struct ActiveBlock {
    index: u32,
    item_id: String,
    kind: StreamItemKind,
    block: serde_json::Map<String, serde_json::Value>,
    input_json: String,
    saw_input_delta: bool,
    announced_client_tool: bool,
    saw_signature_delta: bool,
    saw_compaction_delta: bool,
}

#[derive(Debug, Clone, Default)]
struct AnthropicServerToolCorrelation {
    seen_calls: BTreeSet<String>,
    pending: BTreeMap<String, AnthropicServerToolRoute>,
    results: BTreeSet<String>,
}

struct AnthropicParser {
    provider: String,
    model: String,
    response_id: Option<String>,
    blocks: Vec<serde_json::Value>,
    active: Option<ActiveBlock>,
    client_tool_count: usize,
    programmatic_tool_count: usize,
    client_calls: BTreeSet<String>,
    allowed_client_tools: BTreeSet<String>,
    server_tools: AnthropicServerToolSelection,
    server_correlation: AnthropicServerToolCorrelation,
    server_usage: BTreeMap<String, u32>,
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    container: Option<serde_json::Value>,
    saw_message_delta: bool,
    stop_reason: Option<String>,
    terminal: bool,
}

impl AnthropicParser {
    fn new(
        provider: String,
        model: String,
        allowed_client_tools: BTreeSet<String>,
        server_tools: AnthropicServerToolSelection,
        server_correlation: AnthropicServerToolCorrelation,
    ) -> Self {
        Self {
            provider,
            model,
            response_id: None,
            blocks: Vec::new(),
            active: None,
            client_tool_count: 0,
            programmatic_tool_count: 0,
            client_calls: BTreeSet::new(),
            allowed_client_tools,
            server_tools,
            server_correlation,
            server_usage: BTreeMap::new(),
            input_tokens: None,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            completion_tokens: None,
            container: None,
            saw_message_delta: false,
            stop_reason: None,
            terminal: false,
        }
    }

    fn event(&mut self, event: heycode_http::SseEvent) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            return Vec::new();
        }
        let result = self.parse_event(event);
        match result {
            Ok(events) => events.into_iter().map(Ok).collect(),
            Err(error) => {
                self.terminal = true;
                vec![Err(error)]
            }
        }
    }

    fn parse_event(
        &mut self,
        event: heycode_http::SseEvent,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let value: serde_json::Value = serde_json::from_str(&event.data)
            .map_err(|error| invalid(format!("Messages event is not JSON: {error}")))?;
        let object = as_object(&value, "Messages event")?;
        let event_type = required_nonempty_str(object, "type", "Messages event")?;
        if event.event != event_type {
            return Err(invalid(
                "Messages SSE event name disagrees with its payload type",
            ));
        }
        match event_type {
            "message_start" => self.message_start(object),
            "content_block_start" => self.block_start(object),
            "content_block_delta" => self.block_delta(object),
            "content_block_stop" => self.block_stop(object),
            "message_delta" => self.message_delta(object),
            "message_stop" => self.message_stop(),
            "ping" => Ok(Vec::new()),
            "error" => Err(stream_error(object)),
            _ => Ok(Vec::new()),
        }
    }

    fn message_start(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if self.response_id.is_some() {
            return Err(invalid("Messages stream started more than one message"));
        }
        let message = required_object(event, "message", "Messages message_start")?;
        if required_nonempty_str(message, "type", "Messages response")? != "message"
            || required_nonempty_str(message, "role", "Messages response")? != "assistant"
        {
            return Err(invalid(
                "Messages message_start requires a message/assistant response",
            ));
        }
        let content = message
            .get("content")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| invalid("Messages message_start content must be an array"))?;
        if !content.is_empty() {
            return Err(invalid(
                "Messages message_start content must be empty before block events",
            ));
        }
        if message
            .get("stop_reason")
            .is_none_or(|value| !value.is_null())
            || message
                .get("stop_sequence")
                .is_none_or(|value| !value.is_null())
        {
            return Err(invalid(
                "Messages message_start stop_reason and stop_sequence must be null",
            ));
        }
        let response_model = required_nonempty_str(message, "model", "Messages response")?;
        if response_model != self.model {
            return Err(invalid(
                "Messages response model differs from the resolved model",
            ));
        }
        let id = required_nonempty_str(message, "id", "Messages response")?.to_owned();
        if let Some(container) = message.get("container").filter(|value| !value.is_null()) {
            validate_response_container(container)?;
            self.container = Some(container.clone());
        }
        let usage = required_object(message, "usage", "Messages message_start")?;
        self.input_tokens = Some(required_u64(usage, "input_tokens", "Messages usage")?);
        self.cache_creation_input_tokens = Some(
            optional_u64(usage, "cache_creation_input_tokens", "Messages usage")?.unwrap_or(0),
        );
        self.cache_read_input_tokens =
            Some(optional_u64(usage, "cache_read_input_tokens", "Messages usage")?.unwrap_or(0));
        self.completion_tokens = Some(required_u64(usage, "output_tokens", "Messages usage")?);
        self.response_id = Some(id.clone());
        Ok(vec![InferenceEvent::ResponseStarted { response_id: id }])
    }

    fn block_start(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let response_id = self
            .response_id
            .as_ref()
            .ok_or_else(|| invalid("Messages content block started before message_start"))?;
        if self.active.is_some() || self.saw_message_delta {
            return Err(invalid(
                "Messages content blocks must be sequential and precede message_delta",
            ));
        }
        let index = required_u32(event, "index", "Messages content block")?;
        if usize::try_from(index).ok() != Some(self.blocks.len()) {
            return Err(invalid(
                "Messages content block indexes must be contiguous from zero",
            ));
        }
        let mut block = required_object(event, "content_block", "Messages block start")?.clone();
        let block_type = required_nonempty_str(&block, "type", "Messages content block")?;
        validate_started_block(block_type, &block)?;
        if block_type == "tool_use" {
            let name = required_nonempty_str(&block, "name", "Messages client tool")?;
            if !self.allowed_client_tools.contains(name) {
                return Err(invalid(
                    "Messages response requested an unadvertised client tool",
                ));
            }
        } else if is_server_tool_use(block_type) {
            let name = required_nonempty_str(&block, "name", "Messages server tool")?;
            let server_name = if block_type == "mcp_tool_use" {
                Some(required_nonempty_str(
                    &block,
                    "server_name",
                    "Messages MCP server tool",
                )?)
            } else {
                None
            };
            if self
                .server_tools
                .route_for_call(block_type, name, server_name)
                .is_none()
            {
                return Err(invalid(
                    "Messages response requested an unadvertised server tool",
                ));
            }
        }
        let item_id = block
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{response_id}/content/{index}"));
        let kind = block_kind(block_type);
        if block_type == "tool_use" {
            let id = required_nonempty_str(&block, "id", "Messages client tool")?;
            if !self.client_calls.insert(id.to_owned()) {
                return Err(invalid("Messages client tool id was used twice"));
            }
            if is_programmatic_tool_call(&block) {
                self.programmatic_tool_count = self.programmatic_tool_count.saturating_add(1);
            }
            self.client_tool_count = self.client_tool_count.saturating_add(1);
        } else if is_server_tool_use(block_type) {
            let id = required_nonempty_str(&block, "id", "Messages server tool")?;
            let name = required_nonempty_str(&block, "name", "Messages server tool")?;
            let server_name = block.get("server_name").and_then(serde_json::Value::as_str);
            let route = self
                .server_tools
                .route_for_call(block_type, name, server_name)
                .cloned()
                .ok_or_else(|| invalid("Messages server tool has no configured route"))?;
            if !self.server_correlation.seen_calls.insert(id.to_owned()) {
                return Err(invalid("Messages server tool id was used twice"));
            }
            self.server_correlation.pending.insert(id.to_owned(), route);
        } else if is_server_tool_result(block_type) {
            let id = required_nonempty_str(&block, "tool_use_id", "Messages server result")?;
            let route = self.server_correlation.pending.get(id).ok_or_else(|| {
                invalid("Messages server result has no preceding server tool call")
            })?;
            if !route.result_types.contains(block_type) {
                return Err(invalid(
                    "Messages server result type does not match its preceding call",
                ));
            }
            if !self.server_correlation.results.insert(id.to_owned()) {
                return Err(invalid("Messages server result id was used twice"));
            }
        }
        let mut output = vec![InferenceEvent::ItemStarted {
            output_index: index,
            item_id: item_id.clone(),
            kind: kind.clone(),
        }];
        match block_type {
            "text" => {
                let text = required_str(&block, "text", "Messages text block")?;
                if !text.is_empty() {
                    output.push(InferenceEvent::TextDelta(text.to_owned()));
                }
                if let Some(citations) =
                    block.get("citations").and_then(serde_json::Value::as_array)
                {
                    for citation in citations {
                        if let Some(citation) = normalize_url_citation(citation)? {
                            output.push(InferenceEvent::Citation {
                                output_index: index,
                                citation,
                            });
                        }
                    }
                }
            }
            "thinking" => {
                let thinking = required_str(&block, "thinking", "Messages thinking block")?;
                if !thinking.is_empty() {
                    output.push(InferenceEvent::ReasoningDelta(thinking.to_owned()));
                }
            }
            _ => {}
        }
        let saw_signature_delta = block_type == "thinking"
            && block
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|signature| !signature.is_empty());
        self.active = Some(ActiveBlock {
            index,
            item_id,
            kind,
            block: std::mem::take(&mut block),
            input_json: String::new(),
            saw_input_delta: false,
            announced_client_tool: false,
            saw_signature_delta,
            saw_compaction_delta: false,
        });
        Ok(output)
    }

    fn block_delta(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let index = required_u32(event, "index", "Messages content delta")?;
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| invalid("Messages content delta has no active block"))?;
        if active.index != index {
            return Err(invalid(
                "Messages content delta index differs from its active block",
            ));
        }
        let delta = required_object(event, "delta", "Messages content delta")?;
        let delta_type = required_nonempty_str(delta, "type", "Messages content delta")?;
        let block_type =
            required_nonempty_str(&active.block, "type", "Messages active content block")?;
        match delta_type {
            "text_delta" if block_type == "text" => {
                let text = required_str(delta, "text", "Messages text delta")?;
                append_string(&mut active.block, "text", text, "Messages text block")?;
                Ok(vec![InferenceEvent::TextDelta(text.to_owned())])
            }
            "thinking_delta" if block_type == "thinking" => {
                if active.saw_signature_delta {
                    return Err(invalid(
                        "Messages thinking delta arrived after its signature",
                    ));
                }
                let thinking = required_str(delta, "thinking", "Messages thinking delta")?;
                append_string(
                    &mut active.block,
                    "thinking",
                    thinking,
                    "Messages thinking block",
                )?;
                Ok(vec![InferenceEvent::ReasoningDelta(thinking.to_owned())])
            }
            "signature_delta" if block_type == "thinking" => {
                let signature =
                    required_nonempty_str(delta, "signature", "Messages signature delta")?;
                let existing = required_str(&active.block, "signature", "Messages thinking block")?;
                if active.saw_signature_delta || !existing.is_empty() {
                    return Err(invalid(
                        "Messages thinking block supplied its signature twice",
                    ));
                }
                active.saw_signature_delta = true;
                active.block.insert(
                    "signature".to_owned(),
                    serde_json::Value::String(signature.to_owned()),
                );
                Ok(Vec::new())
            }
            "input_json_delta" if is_tool_input_block(block_type) => {
                let partial = required_str(delta, "partial_json", "Messages tool input delta")?;
                active.input_json.push_str(partial);
                active.saw_input_delta = true;
                if block_type == "tool_use" {
                    let first = !active.announced_client_tool;
                    active.announced_client_tool = true;
                    let id = first
                        .then(|| active.block.get("id"))
                        .flatten()
                        .and_then(serde_json::Value::as_str)
                        .map(heycode_core::CallId::from_raw);
                    let name = first
                        .then(|| active.block.get("name"))
                        .flatten()
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    Ok(vec![InferenceEvent::ToolCallDelta {
                        output_index: index,
                        id,
                        name,
                        arguments_delta: partial.to_owned(),
                    }])
                } else {
                    Ok(Vec::new())
                }
            }
            "citations_delta" if block_type == "text" => {
                let citation = delta
                    .get("citation")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| invalid("Messages citation delta must contain an object"))?
                    .clone();
                let normalized = normalize_url_citation(&citation)?;
                let citations = active
                    .block
                    .entry("citations".to_owned())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                    .as_array_mut()
                    .ok_or_else(|| invalid("Messages text citations must be an array"))?;
                citations.push(citation);
                Ok(normalized
                    .map(|citation| InferenceEvent::Citation {
                        output_index: index,
                        citation,
                    })
                    .into_iter()
                    .collect())
            }
            "compaction_delta" if block_type == "compaction" => {
                if active.saw_compaction_delta {
                    return Err(invalid(
                        "Messages compaction block supplied more than one delta",
                    ));
                }
                let content = required_str(delta, "content", "Messages compaction delta")?;
                active.saw_compaction_delta = true;
                active.block.insert(
                    "content".to_owned(),
                    serde_json::Value::String(content.to_owned()),
                );
                Ok(Vec::new())
            }
            _ => Err(invalid(
                "Messages content delta type is incompatible with its active block",
            )),
        }
    }

    fn block_stop(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        let index = required_u32(event, "index", "Messages content stop")?;
        let mut active = self
            .active
            .take()
            .ok_or_else(|| invalid("Messages content block stopped without a start"))?;
        if active.index != index {
            return Err(invalid(
                "Messages content stop index differs from its active block",
            ));
        }
        let block_type =
            required_nonempty_str(&active.block, "type", "Messages active content block")?
                .to_owned();
        let mut output = Vec::new();
        if is_tool_input_block(&block_type) {
            if active.saw_input_delta && !active.input_json.trim().is_empty() {
                let input: serde_json::Value = serde_json::from_str(&active.input_json)
                    .map_err(|_| invalid("Messages tool input deltas did not form JSON"))?;
                if !input.is_object() {
                    return Err(invalid(
                        "Messages tool input deltas must form a JSON object",
                    ));
                }
                active.block.insert("input".to_owned(), input);
            }
            if block_type == "tool_use" && !active.announced_client_tool {
                let input = active
                    .block
                    .get("input")
                    .ok_or_else(|| invalid("Messages tool block has no input"))?;
                output.push(InferenceEvent::ToolCallDelta {
                    output_index: index,
                    id: active
                        .block
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(heycode_core::CallId::from_raw),
                    name: active
                        .block
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    arguments_delta: serde_json::to_string(input)
                        .map_err(|_| invalid("Messages tool input could not be serialized"))?,
                });
            } else if block_type == "tool_use" && active.input_json.trim().is_empty() {
                let input = active
                    .block
                    .get("input")
                    .ok_or_else(|| invalid("Messages tool block has no input"))?;
                output.push(InferenceEvent::ToolCallDelta {
                    output_index: index,
                    id: None,
                    name: None,
                    arguments_delta: serde_json::to_string(input)
                        .map_err(|_| invalid("Messages tool input could not be serialized"))?,
                });
            }
        }
        if block_type == "thinking"
            && required_str(&active.block, "signature", "Messages thinking block")?.is_empty()
        {
            return Err(invalid(
                "Messages thinking block finished without an opaque signature",
            ));
        }
        if block_type == "compaction" && !active.saw_compaction_delta {
            return Err(invalid(
                "Messages compaction block finished without exactly one delta",
            ));
        }
        let normalized_server_event = if is_server_tool_use(&block_type) {
            let id = required_nonempty_str(&active.block, "id", "Messages server tool")?;
            let route = self
                .server_correlation
                .pending
                .get(id)
                .ok_or_else(|| invalid("Messages server tool route disappeared"))?;
            Some(InferenceEvent::ServerToolCall {
                output_index: index,
                call: normalize_server_tool_call(&active.block, route)?,
            })
        } else if is_server_tool_result(&block_type) {
            let call_id =
                required_nonempty_str(&active.block, "tool_use_id", "Messages server result")?
                    .to_owned();
            let route = self
                .server_correlation
                .pending
                .get(&call_id)
                .cloned()
                .ok_or_else(|| invalid("Messages server result route disappeared"))?;
            let result = normalize_server_tool_result(&active.block, &route)?;
            self.server_correlation.pending.remove(&call_id);
            Some(InferenceEvent::ServerToolResult {
                output_index: index,
                result,
            })
        } else {
            None
        };
        self.blocks
            .push(serde_json::Value::Object(active.block.clone()));
        output.push(InferenceEvent::ItemFinished {
            output_index: index,
            item_id: active.item_id,
            kind: active.kind,
        });
        output.extend(normalized_server_event);
        Ok(output)
    }

    fn message_delta(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<InferenceEvent>, LlmError> {
        if self.response_id.is_none() || self.active.is_some() {
            return Err(invalid(
                "Messages message_delta requires a started message with no active block",
            ));
        }
        self.saw_message_delta = true;
        let delta = required_object(event, "delta", "Messages message_delta")?;
        if let Some(reason) = delta.get("stop_reason").filter(|value| !value.is_null()) {
            let reason = reason
                .as_str()
                .filter(|reason| !reason.is_empty())
                .ok_or_else(|| invalid("Messages stop_reason must be a non-empty string"))?;
            if self.stop_reason.replace(reason.to_owned()).is_some() {
                return Err(invalid("Messages stream supplied stop_reason twice"));
            }
        }
        if let Some(sequence) = delta.get("stop_sequence")
            && !sequence.is_null()
            && !sequence.is_string()
        {
            return Err(invalid("Messages stop_sequence must be a string or null"));
        }
        if let Some(container) = delta.get("container").filter(|value| !value.is_null()) {
            let next_id = validate_response_container(container)?;
            if self.container.as_ref().is_some_and(|current| {
                current
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|current_id| current_id != next_id)
            }) {
                return Err(invalid("Messages container id changed mid-stream"));
            }
            self.container = Some(container.clone());
        }
        if let Some(usage) = event.get("usage").filter(|value| !value.is_null()) {
            let usage = as_object(usage, "Messages message_delta usage")?;
            update_usage_component(
                &mut self.input_tokens,
                optional_u64(usage, "input_tokens", "Messages usage")?,
                "input_tokens",
            )?;
            update_usage_component(
                &mut self.cache_creation_input_tokens,
                optional_u64(usage, "cache_creation_input_tokens", "Messages usage")?,
                "cache_creation_input_tokens",
            )?;
            update_usage_component(
                &mut self.cache_read_input_tokens,
                optional_u64(usage, "cache_read_input_tokens", "Messages usage")?,
                "cache_read_input_tokens",
            )?;
            update_usage_component(
                &mut self.completion_tokens,
                optional_u64(usage, "output_tokens", "Messages usage")?,
                "output_tokens",
            )?;
            if let Some(server_usage) = usage
                .get("server_tool_use")
                .filter(|value| !value.is_null())
            {
                let server_usage = as_object(server_usage, "Messages server-tool usage")?;
                for (field, logical) in self.server_tools.usage_routes() {
                    let Some(requests) =
                        optional_u64(server_usage, field, "Messages server-tool usage")?
                    else {
                        continue;
                    };
                    let requests = u32::try_from(requests)
                        .map_err(|_| invalid("Messages server-tool usage is too large"))?;
                    let previous = self.server_usage.get(logical).copied().unwrap_or(0);
                    if requests < previous {
                        return Err(invalid(
                            "Messages server-tool usage decreased within one response",
                        ));
                    }
                    self.server_usage.insert(logical.to_owned(), requests);
                }
            }
        }
        Ok(Vec::new())
    }

    fn message_stop(&mut self) -> Result<Vec<InferenceEvent>, LlmError> {
        if self.active.is_some() {
            return Err(invalid(
                "Messages message_stop arrived with an unfinished content block",
            ));
        }
        let response_id = self
            .response_id
            .clone()
            .ok_or_else(|| invalid("Messages message_stop arrived before message_start"))?;
        let stop_reason = self
            .stop_reason
            .clone()
            .ok_or_else(|| invalid("Messages message_stop arrived before stop_reason"))?;
        if !matches!(stop_reason.as_str(), "tool_use" | "pause_turn")
            && !self.server_correlation.pending.is_empty()
        {
            return Err(invalid(
                "Messages completed with an unresolved server tool outside a continuation stop",
            ));
        }
        if self.programmatic_tool_count > 0 && self.container.is_none() {
            return Err(invalid(
                "Messages programmatic client tool call omitted its continuation container",
            ));
        }
        let finish = match stop_reason.as_str() {
            "end_turn" | "stop_sequence" => FinishReason::Stop,
            "tool_use" if self.client_tool_count > 0 => FinishReason::ToolCalls,
            "tool_use" => {
                return Err(invalid(
                    "Messages tool_use stop has no client tool_use block",
                ));
            }
            "pause_turn" if self.client_tool_count == 0 => FinishReason::Pause,
            "pause_turn" => {
                return Err(invalid(
                    "Messages pause_turn requires pending server work and no client tool call",
                ));
            }
            "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
            "refusal" => {
                return Err(invalid("Messages response stopped with a provider refusal"));
            }
            _ => {
                return Err(invalid("Messages response used an unknown stop_reason"));
            }
        };
        let mut state_data = serde_json::json!({"role":"assistant","content":self.blocks});
        if let Some(container) = &self.container {
            state_data["container"] = container.clone();
        }
        let state = ProviderStateItem::new(
            self.provider.clone(),
            self.model.clone(),
            ProviderProtocol::AnthropicMessages,
            ProviderStateKind::AnthropicMessage,
            state_data,
        )
        .map_err(|error| invalid(error.to_string()))?;
        let input_tokens = self
            .input_tokens
            .ok_or_else(|| invalid("Messages response omitted input usage"))?;
        let cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .ok_or_else(|| invalid("Messages response omitted cache-creation usage"))?;
        let cache_read_input_tokens = self
            .cache_read_input_tokens
            .ok_or_else(|| invalid("Messages response omitted cache-read usage"))?;
        let prompt_tokens = input_tokens
            .checked_add(cache_creation_input_tokens)
            .and_then(|total| total.checked_add(cache_read_input_tokens))
            .ok_or_else(|| invalid("Messages prompt usage overflowed u64"))?;
        let usage = TokenUsage {
            prompt_tokens,
            completion_tokens: self
                .completion_tokens
                .ok_or_else(|| invalid("Messages response omitted output usage"))?,
        };
        self.terminal = true;
        let mut output = vec![
            InferenceEvent::ProviderState(state),
            InferenceEvent::ResponseFinished {
                response_id,
                status: stop_reason,
            },
        ];
        for (logical, requests) in &self.server_usage {
            if *requests == 0 {
                continue;
            }
            output.push(InferenceEvent::ServerToolUsage(
                heycode_core::ServerToolUsage::new(
                    logical.clone(),
                    *requests,
                    heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                    heycode_core::ServerToolUsageCost::Unknown,
                )
                .map_err(|_| invalid("Messages server-tool usage is invalid"))?,
            ));
        }
        output.push(InferenceEvent::Usage(usage));
        output.push(InferenceEvent::Finish(finish));
        Ok(output)
    }

    fn finish(mut self) -> Vec<Result<InferenceEvent, LlmError>> {
        if self.terminal {
            Vec::new()
        } else {
            self.terminal = true;
            vec![Err(invalid(
                "Messages SSE ended before a terminal message_stop event",
            ))]
        }
    }
}

fn validate_started_block(
    block_type: &str,
    block: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), LlmError> {
    match block_type {
        "text" => {
            required_str(block, "text", "Messages text block")?;
            if block
                .get("citations")
                .is_some_and(|value| !value.is_array())
            {
                return Err(invalid("Messages text citations must be an array"));
            }
        }
        "thinking" => {
            required_str(block, "thinking", "Messages thinking block")?;
            required_str(block, "signature", "Messages thinking block")?;
        }
        "redacted_thinking" => {
            required_nonempty_str(block, "data", "Messages redacted thinking block")?;
        }
        value if is_tool_input_block(value) => {
            required_nonempty_str(block, "id", "Messages tool block")?;
            required_nonempty_str(block, "name", "Messages tool block")?;
            if block.get("input").is_none_or(|value| !value.is_object()) {
                return Err(invalid("Messages tool block input must be an object"));
            }
            if value == "tool_use"
                && let Some(caller) = block.get("caller").filter(|value| !value.is_null())
            {
                let caller = as_object(caller, "Messages client tool caller")?;
                let caller_type =
                    required_nonempty_str(caller, "type", "Messages client tool caller")?;
                if caller_type != "direct" {
                    required_nonempty_str(caller, "tool_id", "Messages programmatic tool caller")?;
                }
            }
        }
        value if is_server_tool_result(value) => {
            required_nonempty_str(block, "tool_use_id", "Messages server result")?;
        }
        "compaction" => {
            required_str(block, "content", "Messages compaction block")?;
        }
        _ => {}
    }
    Ok(())
}

fn block_kind(block_type: &str) -> StreamItemKind {
    match block_type {
        "text" => StreamItemKind::Message,
        "thinking" | "redacted_thinking" => StreamItemKind::Reasoning,
        "tool_use" => StreamItemKind::FunctionCall,
        other => StreamItemKind::Other(other.to_owned()),
    }
}

fn normalize_server_tool_call(
    block: &serde_json::Map<String, serde_json::Value>,
    route: &AnthropicServerToolRoute,
) -> Result<heycode_core::ServerToolCall, LlmError> {
    let id = required_nonempty_str(block, "id", "Messages server tool")?;
    let provider_name = required_nonempty_str(block, "name", "Messages server tool")?;
    let input = block
        .get("input")
        .filter(|value| value.is_object())
        .ok_or_else(|| invalid("Messages server tool input must be an object"))?
        .clone();
    heycode_core::ServerToolCall::new(
        heycode_core::CallId::from_raw(id),
        route.logical.clone(),
        provider_name,
        input,
    )
    .map_err(|error| invalid(error.to_string()))
}

fn normalize_server_tool_result(
    block: &serde_json::Map<String, serde_json::Value>,
    route: &AnthropicServerToolRoute,
) -> Result<heycode_core::ServerToolResult, LlmError> {
    if let Some(normalizer) = &route.result_normalizer {
        return normalizer
            .normalize(block)
            .map_err(|_| invalid("Messages server result failed exact normalization"));
    }
    let call_id = heycode_core::CallId::from_raw(required_nonempty_str(
        block,
        "tool_use_id",
        "Messages server result",
    )?);
    let content = block
        .get("content")
        .ok_or_else(|| invalid("Messages server result has no content"))?;
    if block.get("type").and_then(serde_json::Value::as_str) == Some("mcp_tool_result") {
        let is_error = block
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| invalid("Messages MCP result must carry is_error"))?;
        if is_error {
            return heycode_core::ServerToolResult::error(call_id, "provider_reported_error")
                .map_err(|error| invalid(error.to_string()));
        }
    }
    if let Some(error) = content.as_object().filter(|object| {
        object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind.ends_with("_tool_result_error"))
    }) {
        let code = required_nonempty_str(error, "error_code", "Messages server result error")?;
        return heycode_core::ServerToolResult::error(call_id, code)
            .map_err(|error| invalid(error.to_string()));
    }

    let mut sources = Vec::new();
    if let Some(items) = content.as_array() {
        for item in items {
            let Some(item) = item.as_object() else {
                continue;
            };
            if item.get("type").and_then(serde_json::Value::as_str) != Some("web_search_result") {
                continue;
            }
            let url = required_nonempty_str(item, "url", "Messages web search result")?;
            let title = optional_string(item, "title", "Messages web search result")?
                .map(str::trim)
                .filter(|title| !title.is_empty());
            sources.push(
                heycode_core::ServerToolSource::new(url, title)
                    .map_err(|error| invalid(error.to_string()))?,
            );
        }
    }
    let output_count = match content {
        serde_json::Value::Array(items) => Some(
            u32::try_from(items.len())
                .map_err(|_| invalid("Messages server result output count is too large"))?,
        ),
        serde_json::Value::Null => Some(0),
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Object(_) => Some(1),
    };
    heycode_core::ServerToolResult::success(call_id, output_count, sources)
        .map_err(|error| invalid(error.to_string()))
}

fn normalize_url_citation(
    value: &serde_json::Value,
) -> Result<Option<heycode_core::UrlCitation>, LlmError> {
    let citation = value
        .as_object()
        .ok_or_else(|| invalid("Messages citation must be an object"))?;
    let citation_type = required_nonempty_str(citation, "type", "Messages citation")?;
    let Some(url) = citation.get("url") else {
        if citation_type == "web_search_result_location" {
            return Err(invalid("Messages web citation has no URL"));
        }
        return Ok(None);
    };
    let url = url
        .as_str()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| invalid("Messages citation URL must be a non-empty string"))?;
    let title = optional_string(citation, "title", "Messages citation")?
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let cited_text = optional_string(citation, "cited_text", "Messages citation")?
        .filter(|text| !text.is_empty());
    let start_index = optional_u32(citation, "start_index", "Messages citation")?;
    let end_index = optional_u32(citation, "end_index", "Messages citation")?;
    heycode_core::UrlCitation::new(url, title, cited_text, start_index, end_index)
        .map(Some)
        .map_err(|error| invalid(error.to_string()))
}

fn optional_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
    context: &str,
) -> Result<Option<&'a str>, LlmError> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(invalid(format!("{context} `{key}` must be a string"))),
    }
}

fn optional_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    context: &str,
) -> Result<Option<u32>, LlmError> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("{context} `{key}` must be a u32"))),
    }
}

fn is_tool_input_block(block_type: &str) -> bool {
    block_type == "tool_use" || is_server_tool_use(block_type)
}

fn is_server_tool_use(block_type: &str) -> bool {
    block_type == "server_tool_use" || block_type == "mcp_tool_use"
}

fn is_server_tool_result(block_type: &str) -> bool {
    block_type != "tool_result" && block_type.ends_with("_tool_result")
}

fn is_programmatic_tool_call(block: &serde_json::Map<String, serde_json::Value>) -> bool {
    block
        .get("caller")
        .and_then(serde_json::Value::as_object)
        .and_then(|caller| caller.get("type"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|caller| caller != "direct")
}

fn validate_response_container(container: &serde_json::Value) -> Result<&str, LlmError> {
    let container = as_object(container, "Messages response container")?;
    let id = required_nonempty_str(container, "id", "Messages response container")?;
    if container
        .get("expires_at")
        .is_some_and(|value| !value.is_string())
        || container
            .get("skills")
            .is_some_and(|value| !value.is_array())
    {
        return Err(invalid(
            "Messages response container expiry/skills have invalid shapes",
        ));
    }
    Ok(id)
}

fn append_string(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    delta: &str,
    context: &str,
) -> Result<(), LlmError> {
    let current = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a string")))?
        .to_owned();
    object.insert(
        field.to_owned(),
        serde_json::Value::String(format!("{current}{delta}")),
    );
    Ok(())
}

fn stream_error(event: &serde_json::Map<String, serde_json::Value>) -> LlmError {
    let code = event
        .get("error")
        .and_then(serde_json::Value::as_object)
        .and_then(|error| error.get("type"))
        .and_then(serde_json::Value::as_str);
    crate::retry::provider_event_error(crate::ProviderErrorClass::Server, code)
}

fn required_object<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    object
        .get(field)
        .ok_or_else(|| invalid(format!("{context} is missing `{field}`")))
        .and_then(|value| as_object(value, context))
}

fn as_object<'a>(
    value: &'a serde_json::Value,
    context: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, LlmError> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn required_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a string")))
}

fn required_nonempty_str<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<&'a str, LlmError> {
    let value = required_str(object, field, context)?;
    if value.is_empty() || value.trim() != value {
        Err(invalid(format!("{context} `{field}` must be non-empty")))
    } else {
        Ok(value)
    }
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u64, LlmError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| invalid(format!("{context} `{field}` must be a u64")))
}

fn optional_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<u64>, LlmError> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{context} `{field}` must be a u64"))),
    }
}

fn update_usage_component(
    current: &mut Option<u64>,
    next: Option<u64>,
    field: &str,
) -> Result<(), LlmError> {
    if let Some(next) = next {
        if current.is_some_and(|previous| next < previous) {
            return Err(invalid(format!(
                "Messages cumulative `{field}` usage moved backwards"
            )));
        }
        *current = Some(next);
    }
    Ok(())
}

fn required_u32(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u32, LlmError> {
    u32::try_from(required_u64(object, field, context)?)
        .map_err(|_| invalid(format!("{context} `{field}` must be a u32")))
}

fn safe_server_tool_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}

fn safe_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b',')
}

fn invalid(message: impl Into<String>) -> LlmError {
    LlmError::InvalidResponse(message.into())
}

fn map_transport_error(error: heycode_http::TransportError) -> LlmError {
    crate::classify_transport_error(error)
}

fn one_error(error: LlmError) -> InferenceStream {
    Box::pin(futures::stream::once(async move { Err(error) }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod image_tests {
    use super::*;

    #[test]
    fn messages_user_images_use_base64_blocks_before_text() {
        let image = crate::ChatImage::new(
            heycode_core::AttachmentMediaType::new("image/webp").unwrap(),
            vec![1, 2, 3],
        )
        .unwrap();
        let value =
            anthropic_message(&ChatMessage::user_with_images("describe", vec![image])).unwrap();
        assert_eq!(
            value["content"][0],
            serde_json::json!({
                "type":"image",
                "source":{"type":"base64","media_type":"image/webp","data":"AQID"}
            })
        );
        assert_eq!(
            value["content"][1],
            serde_json::json!({
                "type":"text","text":"describe"
            })
        );
    }

    #[test]
    fn messages_user_pdf_uses_document_block_before_text() {
        let document = crate::ChatDocument::new(
            heycode_core::AttachmentMediaType::new("application/pdf").unwrap(),
            "guide.pdf",
            b"%PDF-".to_vec(),
        )
        .unwrap();
        let value = anthropic_message(&ChatMessage::user_with_media(
            "summarize",
            Vec::new(),
            vec![document],
        ))
        .unwrap();
        assert_eq!(
            value["content"][0],
            serde_json::json!({
                "type":"document",
                "source":{"type":"base64","media_type":"application/pdf","data":"JVBERi0="},
                "title":"guide.pdf"
            })
        );
        assert_eq!(
            value["content"][1],
            serde_json::json!({"type":"text","text":"summarize"})
        );
    }
}
