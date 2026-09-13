//! Provider-measured token counting through `POST /v1/messages/count_tokens`.
//!
//! The provider tokenizer evaluates the exact request shape; it is never a
//! local byte-ratio guess. Anthropic's current guide nevertheless describes
//! the returned number as an estimate. The shared P11 evidence vocabulary does
//! not yet distinguish a provider-tokenizer estimate from a local heuristic;
//! that remaining cross-crate evidence gap is documented in the crate README.
//! This implementation resolves its credential per operation and refuses —
//! never drops — content it cannot honestly represent.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpService, SERVICE_HTTP};
use heycode_llm::{
    CountableContent, Role, TokenCountFailure, TokenCountRequest, TokenCounter,
    TokenCounterDescriptor, TokenCounterId, TokenCounterRegistry, TokenCounterScope, TokenEvidence,
};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::catalog::{ANTHROPIC_PROVIDER, ANTHROPIC_VERSION, DEFAULT_BASE_URL};
use crate::{
    ANTHROPIC_CONTEXT_EDITING_BETA, AnthropicContextEditingPolicy, AnthropicPromptCachePolicy,
    AnthropicSettingsPolicies,
};

/// Stable id of the provider-measured Anthropic counter.
pub const ANTHROPIC_TOKEN_COUNTER_ID: &str = "anthropic:count-tokens";
/// The count response is one small object; anything larger is a protocol fault.
const COUNT_RESPONSE_LIMIT: usize = 64 * 1024;

/// Configuration captured by the Anthropic token-counter plugin.
#[derive(Clone)]
pub struct AnthropicTokenCounterConfig {
    base_url: String,
    credential: CredentialQuery,
    context_editing: Option<AnthropicContextEditingPolicy>,
    prompt_cache: Option<AnthropicPromptCachePolicy>,
}

impl AnthropicTokenCounterConfig {
    /// Use the official Anthropic API origin and an explicit credential query.
    #[must_use]
    pub fn official(credential: CredentialQuery) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            credential,
            context_editing: None,
            prompt_cache: None,
        }
    }

    /// Override the API base URL for a compatible proxy or test endpoint.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Apply the same server-side context-editing policy to token pressure
    /// measurement, returning both effective and original input counts.
    #[must_use]
    pub fn with_context_editing(mut self, policy: AnthropicContextEditingPolicy) -> Self {
        self.context_editing = Some(policy);
        self
    }

    /// Apply the same automatic prompt-cache breakpoint shape to counting.
    #[must_use]
    pub fn with_prompt_caching(mut self, policy: AnthropicPromptCachePolicy) -> Self {
        self.prompt_cache = Some(policy);
        self
    }

    /// Context-editing policy applied to count requests.
    #[must_use]
    pub const fn context_editing_policy(&self) -> Option<&AnthropicContextEditingPolicy> {
        self.context_editing.as_ref()
    }

    /// Prompt-cache policy applied to count requests.
    #[must_use]
    pub const fn prompt_cache_policy(&self) -> Option<&AnthropicPromptCachePolicy> {
        self.prompt_cache.as_ref()
    }
}

/// Provider token-count response with context-editing visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnthropicTokenCountReport {
    effective_input_tokens: u64,
    original_input_tokens: Option<u64>,
}

impl AnthropicTokenCountReport {
    /// Input tokens after any configured context editing is applied.
    #[must_use]
    pub const fn effective_input_tokens(self) -> u64 {
        self.effective_input_tokens
    }

    /// Original input tokens before editing, when the endpoint reports them.
    #[must_use]
    pub const fn original_input_tokens(self) -> Option<u64> {
        self.original_input_tokens
    }

    /// Provider-reported input tokens removed by editing.
    #[must_use]
    pub const fn cleared_input_tokens(self) -> Option<u64> {
        match self.original_input_tokens {
            Some(original) => original.checked_sub(self.effective_input_tokens),
            None => None,
        }
    }
}

/// Counts tokens by asking Anthropic to measure the exact request.
pub struct AnthropicTokenCounter {
    http: HttpService,
    credentials: CredentialsService,
    config: AnthropicTokenCounterConfig,
    descriptor: TokenCounterDescriptor,
}

impl AnthropicTokenCounter {
    /// Bind the composed HTTP and credential services to this counter.
    ///
    /// # Errors
    /// Impossible static descriptor validation failure.
    pub fn new(
        http: HttpService,
        credentials: CredentialsService,
        config: AnthropicTokenCounterConfig,
    ) -> Result<Self, CoreError> {
        let descriptor = TokenCounterDescriptor::new(
            TokenCounterId::new(ANTHROPIC_TOKEN_COUNTER_ID)
                .map_err(|error| CoreError::other(error.to_string()))?,
            TokenEvidence::Estimated(heycode_llm::EstimationMethod::ProviderTokenizer),
            TokenCounterScope::provider(ANTHROPIC_PROVIDER)
                .map_err(|error| CoreError::other(error.to_string()))?,
        )
        .map_err(|error| CoreError::other(error.to_string()))?;
        Ok(Self {
            http,
            credentials,
            config,
            descriptor,
        })
    }

    /// Ask Anthropic for the effective input count and optional pre-edit count.
    ///
    /// # Errors
    /// Unsupported input, missing credentials, cancellation, transport/server
    /// failure, or malformed count metadata returns a closed token-count
    /// failure without provider response content.
    pub async fn count_report(
        &self,
        request: &TokenCountRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<AnthropicTokenCountReport, TokenCountFailure> {
        if cancellation.is_cancelled() {
            return Err(TokenCountFailure::cancelled());
        }
        let mut body = build_body(request)?;
        if let Some(policy) = &self.config.context_editing {
            merge_top_level(&mut body, policy.request_fields())?;
        }
        if let Some(policy) = &self.config.prompt_cache {
            merge_top_level(&mut body, policy.request_fields())?;
        }
        let secret = self
            .credentials
            .resolve(&self.config.credential)
            .map_err(|_| failed("credential unavailable"))?
            .ok_or_else(|| unsupported("no Anthropic credential is configured"))?;
        let mut http_request = HttpRequest::post(
            format!("{}/v1/messages/count_tokens", self.config.base_url),
            serde_json::to_vec(&body).map_err(|_| failed("request encoding failed"))?,
        )
        .and_then(|request| request.header("accept", "application/json"))
        .and_then(|request| request.header("content-type", "application/json"))
        .and_then(|request| request.header("x-api-key", secret.expose()))
        .and_then(|request| request.header("anthropic-version", ANTHROPIC_VERSION))
        .map_err(|_| failed("request construction failed"))?;
        if self.config.context_editing.is_some() {
            http_request = http_request
                .header("anthropic-beta", ANTHROPIC_CONTEXT_EDITING_BETA)
                .map_err(|_| failed("request construction failed"))?;
        }
        let http_request = http_request.with_max_response_bytes(COUNT_RESPONSE_LIMIT);
        drop(secret);

        let response = self
            .http
            .send(http_request, cancellation.clone())
            .await
            .map_err(|_| {
                if cancellation.is_cancelled() {
                    TokenCountFailure::cancelled()
                } else {
                    failed("token count request failed")
                }
            })?;
        if !(200..300).contains(&response.status) {
            return Err(failed("token count request was rejected"));
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|_| failed("token count reply is malformed"))?;
        let effective_input_tokens = value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .ok_or_else(|| failed("token count reply is missing input_tokens"))?;
        let original_input_tokens = value
            .get("context_management")
            .and_then(|metadata| metadata.get("original_input_tokens"))
            .and_then(Value::as_u64);
        if self.config.context_editing.is_some()
            && original_input_tokens.is_none_or(|original| original < effective_input_tokens)
        {
            return Err(failed("token count reply has invalid context metadata"));
        }
        Ok(AnthropicTokenCountReport {
            effective_input_tokens,
            original_input_tokens,
        })
    }
}

impl std::fmt::Debug for AnthropicTokenCounter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicTokenCounter")
            .field("id", &ANTHROPIC_TOKEN_COUNTER_ID)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TokenCounter for AnthropicTokenCounter {
    fn descriptor(&self) -> TokenCounterDescriptor {
        self.descriptor.clone()
    }

    async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure> {
        Ok(self
            .count_report(request, cancellation)
            .await?
            .effective_input_tokens())
    }
}

/// Build the exact `count_tokens` body.
///
/// The endpoint measures a real request, so it needs a model plus at least one
/// message. Content this counter cannot faithfully represent is refused as
/// Unsupported rather than dropped, because dropping it would under-count.
fn build_body(request: &TokenCountRequest<'_>) -> Result<Value, TokenCountFailure> {
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut system = String::new();
    let mut tools: Vec<Value> = Vec::new();
    for part in request.content() {
        match part {
            CountableContent::Text(text) => {
                flush_tool_results(&mut messages, &mut pending_tool_results);
                push_text(&mut messages, text);
            }
            CountableContent::Message(message) => match message.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&message.content);
                }
                Role::User | Role::Assistant => {
                    flush_tool_results(&mut messages, &mut pending_tool_results);
                    if let Some(value) = structured_message(message)? {
                        messages.push(value);
                    }
                }
                Role::Tool => {
                    pending_tool_results.push(tool_result_block(message)?);
                }
            },
            CountableContent::Tool(spec) => {
                let mut tool = Map::new();
                tool.insert("name".to_owned(), json!(spec.name));
                tool.insert("description".to_owned(), json!(spec.description));
                tool.insert("input_schema".to_owned(), spec.parameters.clone());
                tools.push(Value::Object(tool));
            }
        }
    }
    flush_tool_results(&mut messages, &mut pending_tool_results);
    if messages.is_empty() {
        // The endpoint rejects an empty transcript; a zero would be a lie.
        return Err(unsupported(
            "a token count needs at least one user or assistant message",
        ));
    }
    let mut body = Map::new();
    body.insert("model".to_owned(), json!(request.model()));
    body.insert("messages".to_owned(), Value::Array(messages));
    if !system.is_empty() {
        body.insert("system".to_owned(), json!(system));
    }
    if !tools.is_empty() {
        body.insert("tools".to_owned(), Value::Array(tools));
    }
    Ok(Value::Object(body))
}

fn push_text(messages: &mut Vec<Value>, text: &str) {
    if text.is_empty() {
        return;
    }
    messages.push(json!({"role": "user", "content": text}));
}

fn structured_message(
    message: &heycode_llm::ChatMessage,
) -> Result<Option<Value>, TokenCountFailure> {
    let role = if message.role == Role::User {
        "user"
    } else {
        "assistant"
    };
    let mut content = Vec::new();
    if message.role == Role::User {
        for image in &message.images {
            content.push(json!({
                "type":"image",
                "source":{"type":"base64","media_type":image.media_type().as_str(),
                    "data":base64::engine::general_purpose::STANDARD.encode(image.bytes())}
            }));
        }
        for document in &message.documents {
            content.push(json!({
                "type":"document",
                "source":{"type":"base64","media_type":document.media_type().as_str(),
                    "data":base64::engine::general_purpose::STANDARD.encode(document.bytes())},
                "title":document.filename()
            }));
        }
    } else if !message.images.is_empty() || !message.documents.is_empty() {
        return Err(unsupported("assistant media is not valid Messages input"));
    }
    if !message.content.is_empty() {
        content.push(json!({"type":"text","text":message.content}));
    }
    if let Some(calls) = &message.tool_calls {
        if message.role != Role::Assistant {
            return Err(unsupported("tool calls require an assistant message"));
        }
        for call in calls {
            let input: Value = serde_json::from_str(&call.arguments)
                .map_err(|_| unsupported("tool-call input is not object JSON"))?;
            if call.id.is_empty() || call.name.is_empty() || !input.is_object() {
                return Err(unsupported("tool-call input is invalid"));
            }
            content.push(json!({"type":"tool_use","id":call.id,"name":call.name,"input":input}));
        }
    }
    Ok((!content.is_empty()).then(|| json!({"role":role,"content":content})))
}

fn tool_result_block(message: &heycode_llm::ChatMessage) -> Result<Value, TokenCountFailure> {
    let tool_use_id = message
        .tool_call_id
        .as_deref()
        .filter(|id| !id.is_empty() && id.trim() == *id)
        .ok_or_else(|| unsupported("tool result has no valid tool_use_id"))?;
    let mut block = json!({
        "type":"tool_result",
        "tool_use_id":tool_use_id,
        "content":message.content
    });
    if message.tool_result_is_error == Some(true) {
        block["is_error"] = json!(true);
    }
    Ok(block)
}

fn flush_tool_results(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if pending.is_empty() {
        return;
    }
    messages.push(json!({"role":"user","content":std::mem::take(pending)}));
}

fn merge_top_level(body: &mut Value, fields: &Value) -> Result<(), TokenCountFailure> {
    let body = body
        .as_object_mut()
        .ok_or_else(|| failed("count request body is invalid"))?;
    let fields = fields
        .as_object()
        .ok_or_else(|| failed("count request extension is invalid"))?;
    for (key, value) in fields {
        if body.insert(key.clone(), value.clone()).is_some() {
            return Err(failed("count request extension conflicts"));
        }
    }
    Ok(())
}

fn unsupported(message: &'static str) -> TokenCountFailure {
    TokenCountFailure::unsupported(message)
}

fn failed(message: &'static str) -> TokenCountFailure {
    TokenCountFailure::failed(message)
}

/// Register the provider-tokenizer Anthropic token counter.
#[must_use]
pub fn anthropic_token_counter_plugin(config: AnthropicTokenCounterConfig) -> Box<dyn Plugin> {
    struct AnthropicTokenCounterPlugin(AnthropicTokenCounterConfig);

    impl Plugin for AnthropicTokenCounterPlugin {
        fn name(&self) -> &'static str {
            "token-count-anthropic"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::TokenCounter,
                ANTHROPIC_TOKEN_COUNTER_ID,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                SERVICE_HTTP,
                SERVICE_CREDENTIALS,
                heycode_llm::SERVICE_TOKEN_COUNTERS,
                heycode_settings::SERVICE_SETTINGS,
            ]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let registry = context
                .get::<TokenCounterRegistry>(heycode_llm::SERVICE_TOKEN_COUNTERS)
                .ok_or_else(|| {
                    CoreError::MissingService(heycode_llm::SERVICE_TOKEN_COUNTERS.to_string())
                })?;
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| {
                    CoreError::MissingService(heycode_settings::SERVICE_SETTINGS.to_string())
                })?;
            let policies = AnthropicSettingsPolicies::resolve(&settings)
                .map_err(|error| CoreError::other(error.to_string()))?;
            let counter = AnthropicTokenCounter::new(
                (*http).clone(),
                (*credentials).clone(),
                policies.apply_token_counter_config(self.0.clone()),
            )?;
            registry
                .register(context, Arc::new(counter))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(AnthropicTokenCounterPlugin(config))
}
