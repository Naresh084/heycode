//! PGCP07 Claude on Google Cloud profile and live-evidence probe.
//!
//! Google now presents this surface as Agent Platform while the tracker retains
//! the established "Claude on Vertex" name. The wire remains the Google Cloud
//! `aiplatform.googleapis.com` publisher-model route. It is nearly Anthropic
//! Messages, with two documented differences: the model moves from the body to
//! the endpoint URL, and `anthropic_version = "vertex-2023-10-16"` moves into
//! the body.
//!
//! PGCP01 deliberately cannot prove ADC works: it reads no token and reports a
//! configured account as Unknown. [`ClaudeVertexProfile::auth_preflight`]
//! preserves that fact. Only [`ClaudeVertexProfile::probe`] can construct
//! [`ClaudeVertexLiveEvidence`], after an authenticated stream proves the
//! selected model, a client tool call, and adaptive thinking in one response.
//!
//! Sources:
//! - <https://platform.claude.com/docs/en/build-with-claude/claude-on-vertex-ai>
//! - <https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/partner-models/claude/sonnet-5>
//! - <https://platform.claude.com/docs/en/build-with-claude/effort>
//! - <https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview>

use futures::StreamExt as _;

use heycode_authorization_gcp::{
    GcpAccountHealth, GcpAuthProfile, GcpHealth, GcpLocationHealth, GcpLocationKind,
    GcpProjectHealth,
};
use heycode_credentials::{CredentialQuery, CredentialsService};
use heycode_http::{HttpService, HttpSseRequest};
use heycode_llm::{
    CapabilitySupport, ModelCapabilities, ModelDescriptor, ModelLifecycle, ModelPerformance,
    ModelPricing, ProviderDescriptor, ProviderProfile,
};
use tokio_util::sync::CancellationToken;

/// Registry identity for Anthropic Messages served by Google Cloud.
pub const CLAUDE_VERTEX_PROVIDER: &str = "vertex-claude";

/// Current maintained Google Cloud model for this profile.
pub const CLAUDE_VERTEX_DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Messages API version required in every Google Cloud request body.
pub const CLAUDE_VERTEX_ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

/// OAuth scope the official Vertex SDK requests for Google Cloud access.
pub const CLAUDE_VERTEX_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

const PROBE_TOOL: &str = "heycode_vertex_probe";
const MAX_PROBE_EVENTS: usize = 4_096;
const MAX_PROBE_TOOL_INPUT_BYTES: usize = 16 * 1024;

/// Thinking modes admitted by the maintained Sonnet 5 Google Cloud route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeVertexThinking {
    /// Provider decides when and how deeply to think, guided by effort.
    Adaptive,
    /// Explicitly disable thinking blocks.
    Disabled,
}

impl ClaudeVertexThinking {
    /// Complete documented set for Sonnet 5.
    pub const SUPPORTED: [Self; 2] = [Self::Adaptive, Self::Disabled];

    /// Messages request value for `thinking.type`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Disabled => "disabled",
        }
    }
}

/// Effort levels admitted by Claude Sonnet 5 on Google Cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeVertexEffort {
    /// Lowest token spend and latency.
    Low,
    /// Balanced token spend and capability.
    Medium,
    /// Default high-capability behavior.
    High,
    /// Extended long-horizon capability.
    XHigh,
    /// Maximum capability without a token-efficiency constraint.
    Max,
}

impl ClaudeVertexEffort {
    /// Complete documented set for Sonnet 5.
    pub const SUPPORTED: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max];

    /// Messages request value for `output_config.effort`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Explicit Sonnet 5 thinking and effort selection for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeVertexControls {
    thinking: ClaudeVertexThinking,
    effort: ClaudeVertexEffort,
}

impl ClaudeVertexControls {
    /// Build one explicit request control selection.
    #[must_use]
    pub const fn new(thinking: ClaudeVertexThinking, effort: ClaudeVertexEffort) -> Self {
        Self { thinking, effort }
    }

    /// Maintained route default: adaptive thinking and high effort.
    #[must_use]
    pub const fn sonnet_five_default() -> Self {
        Self::new(ClaudeVertexThinking::Adaptive, ClaudeVertexEffort::High)
    }

    /// Selected thinking mode.
    #[must_use]
    pub const fn thinking(self) -> ClaudeVertexThinking {
        self.thinking
    }

    /// Selected effort level.
    #[must_use]
    pub const fn effort(self) -> ClaudeVertexEffort {
        self.effort
    }
}

/// Safe route metadata for Claude Sonnet 5 on Google Cloud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeVertexProfile {
    project: String,
    project_confirmed: bool,
    location: String,
    credential: CredentialQuery,
    model: ModelDescriptor,
    endpoint: String,
}

impl ClaudeVertexProfile {
    /// Build the maintained Sonnet 5 profile from PGCP01's three health planes.
    ///
    /// A configured ADC source remains unverified, but it is a sufficient
    /// preflight to define a route whose operation-time credential is resolved
    /// separately. The current Sonnet 5 model card offers global and
    /// multi-region endpoints; PGCP01 does not yet represent `us`/`eu`
    /// multi-regions, so this boundary admits only exact `global` rather than
    /// claiming an unsupported regional route.
    ///
    /// # Errors
    /// Absent/faulted/undetermined ADC, a missing project or location, or a
    /// non-global location fails before a profile is published.
    pub fn from_gcp(
        auth: &GcpAuthProfile,
        credential: CredentialQuery,
    ) -> Result<Self, ClaudeVertexError> {
        if credential.kind.as_str() != "oauth-token" {
            return Err(ClaudeVertexError::CredentialKindMismatch);
        }
        match auth.account() {
            GcpAccountHealth::Configured { .. } => {}
            GcpAccountHealth::Undetermined { .. } => {
                return Err(ClaudeVertexError::AccountUndetermined);
            }
            GcpAccountHealth::Absent | GcpAccountHealth::Faulted { .. } => {
                return Err(ClaudeVertexError::AccountUnavailable);
            }
        }
        let (project, project_confirmed) = match auth.project() {
            GcpProjectHealth::Confirmed { project, .. } => (project.as_str().to_owned(), true),
            GcpProjectHealth::Unconfirmed { project, .. } => (project.as_str().to_owned(), false),
            GcpProjectHealth::Unset
            | GcpProjectHealth::Malformed { .. }
            | GcpProjectHealth::Undetermined { .. } => {
                return Err(ClaudeVertexError::ProjectUnavailable);
            }
        };
        let location = match auth.location() {
            GcpLocationHealth::Selected { location, .. } => location,
            GcpLocationHealth::Unset
            | GcpLocationHealth::Malformed { .. }
            | GcpLocationHealth::Undetermined { .. } => {
                return Err(ClaudeVertexError::LocationUnavailable);
            }
        };
        if location.kind() != GcpLocationKind::Global {
            return Err(ClaudeVertexError::UnsupportedLocation);
        }
        let location = location.as_str().to_owned();
        let endpoint = format!(
            "https://aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{CLAUDE_VERTEX_DEFAULT_MODEL}:streamRawPredict"
        );
        HttpSseRequest::post(&endpoint, Vec::new())
            .map_err(|_| ClaudeVertexError::InvalidEndpoint)?;
        Ok(Self {
            project,
            project_confirmed,
            location,
            credential,
            model: sonnet_five_descriptor(),
            endpoint,
        })
    }

    /// PGCP01's honest ADC preflight evidence.
    ///
    /// A constructed profile always has configured ADC, but no token exchange
    /// has run, so this is Unknown and never Supported.
    #[must_use]
    pub const fn auth_preflight(&self) -> GcpHealth {
        GcpHealth::Unknown
    }

    /// Selected project id or project number.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Whether the ambient host confirmed the selected project.
    #[must_use]
    pub const fn project_confirmed(&self) -> bool {
        self.project_confirmed
    }

    /// Selected Google Cloud location.
    #[must_use]
    pub fn location(&self) -> &str {
        &self.location
    }

    /// Exact streaming publisher-model endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Maintained model facts for this profile.
    #[must_use]
    pub const fn model(&self) -> &ModelDescriptor {
        &self.model
    }

    pub(crate) const fn credential_query(&self) -> &CredentialQuery {
        &self.credential
    }

    /// Safe setup/routing metadata.
    #[must_use]
    pub fn provider_profile(&self) -> ProviderProfile {
        ProviderProfile {
            registry_name: CLAUDE_VERTEX_PROVIDER.to_owned(),
            descriptor: provider_descriptor(),
            default_model: CLAUDE_VERTEX_DEFAULT_MODEL.to_owned(),
            credential_reference: Some(self.credential.reference.as_str().to_owned()),
        }
    }

    /// Convert a shared Messages body into the Google Cloud dialect.
    ///
    /// This compatibility entry point explicitly resolves the maintained
    /// Sonnet 5 default (adaptive thinking, high effort). Call
    /// [`Self::prepare_messages_body_with_controls`] when the request selected
    /// another documented value.
    ///
    /// The exact configured model must be present so a stale/misdirected shared
    /// body cannot be silently retargeted. It is removed after comparison, and
    /// the fixed Vertex version is inserted in the body. Tools, messages and
    /// unrelated Messages fields remain byte-semantic JSON; thinking and
    /// effort must match the explicitly resolved Sonnet 5 default.
    ///
    /// # Errors
    /// A non-object body, missing/mismatched model, conflicting version, or
    /// incompatible thinking/effort value is refused before transport.
    pub fn prepare_messages_body(
        &self,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, ClaudeVertexError> {
        self.prepare_messages_body_with_controls(body, ClaudeVertexControls::sonnet_five_default())
    }

    /// Convert a shared Messages body using one exact thinking/effort choice.
    ///
    /// Missing controls are materialized from `controls`; already-present
    /// controls must agree exactly. This makes the provider-owned selection the
    /// sole defaulting point and prevents a shared body from smuggling manual
    /// thinking or a future effort id onto Sonnet 5.
    ///
    /// # Errors
    /// In addition to route/body failures, conflicting thinking or effort
    /// values, any legacy sampling control, and a non-object `output_config`
    /// are refused before transport.
    pub fn prepare_messages_body_with_controls(
        &self,
        body: serde_json::Value,
        controls: ClaudeVertexControls,
    ) -> Result<serde_json::Value, ClaudeVertexError> {
        let mut body = body
            .as_object()
            .cloned()
            .ok_or(ClaudeVertexError::MalformedBody)?;
        let model = body
            .remove("model")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or(ClaudeVertexError::ModelMismatch)?;
        if model != self.model.id {
            return Err(ClaudeVertexError::ModelMismatch);
        }
        match body.get("anthropic_version") {
            None | Some(serde_json::Value::Null) => {
                body.insert(
                    "anthropic_version".to_owned(),
                    serde_json::json!(CLAUDE_VERTEX_ANTHROPIC_VERSION),
                );
            }
            Some(serde_json::Value::String(version))
                if version == CLAUDE_VERTEX_ANTHROPIC_VERSION => {}
            Some(_) => return Err(ClaudeVertexError::VersionMismatch),
        }
        if ["temperature", "top_p", "top_k"]
            .iter()
            .any(|field| body.contains_key(*field))
        {
            return Err(ClaudeVertexError::SamplingUnsupported);
        }
        let thinking = serde_json::json!({ "type": controls.thinking().as_str() });
        match body.get("thinking") {
            None => {
                body.insert("thinking".to_owned(), thinking);
            }
            Some(value) if value == &thinking => {}
            Some(_) => return Err(ClaudeVertexError::ThinkingMismatch),
        }
        let output_config = body
            .entry("output_config".to_owned())
            .or_insert_with(|| serde_json::json!({}));
        let output_config = output_config
            .as_object_mut()
            .ok_or(ClaudeVertexError::MalformedOutputConfig)?;
        match output_config.get("effort") {
            None => {
                output_config.insert(
                    "effort".to_owned(),
                    serde_json::json!(controls.effort().as_str()),
                );
            }
            Some(serde_json::Value::String(value)) if value == controls.effort().as_str() => {}
            Some(_) => return Err(ClaudeVertexError::EffortMismatch),
        }
        Ok(serde_json::Value::Object(body))
    }

    /// Run one authenticated model/tool/thinking evidence probe.
    ///
    /// The access token is resolved at operation time from the profile's exact
    /// credential query. A successful HTTP stream is not enough: evidence is
    /// constructed only after the response names Sonnet 5, opens a thinking
    /// block, invokes the forced probe tool, settles with `tool_use`, and emits
    /// `message_stop`.
    ///
    /// # Errors
    /// Credential, request, transport, response-shape, event-budget, or missing
    /// evidence failures return only stable body-free classes.
    pub async fn probe(
        &self,
        http: &HttpService,
        credentials: &CredentialsService,
        cancellation: CancellationToken,
    ) -> Result<ClaudeVertexLiveEvidence, ClaudeVertexError> {
        if cancellation.is_cancelled() {
            return Err(ClaudeVertexError::Cancelled);
        }
        let credential = credentials
            .resolve_route(&self.credential)
            .map_err(|_| ClaudeVertexError::CredentialUnavailable)?;
        if cancellation.is_cancelled() {
            return Err(ClaudeVertexError::Cancelled);
        }
        let body = self.prepare_messages_body_with_controls(
            serde_json::json!({
                "model": self.model.id,
                "max_tokens": 4096,
                "stream": true,
                "messages": [{
                    "role": "user",
                    "content": "Call heycode_vertex_probe exactly once with ok=true."
                }],
                "tools": [{
                    "name": PROBE_TOOL,
                    "description": "Return the requested boolean to the caller.",
                    "input_schema": {
                        "type": "object",
                        "properties": { "ok": { "type": "boolean" } },
                        "required": ["ok"],
                        "additionalProperties": false
                    }
                }],
                "tool_choice": { "type": "tool", "name": PROBE_TOOL },
                "thinking": { "type": "adaptive" },
                "output_config": { "effort": "max" }
            }),
            ClaudeVertexControls::new(ClaudeVertexThinking::Adaptive, ClaudeVertexEffort::Max),
        )?;
        let body = serde_json::to_vec(&body).map_err(|_| ClaudeVertexError::MalformedBody)?;
        let request = HttpSseRequest::post(&self.endpoint, body)
            .and_then(|request| request.header("content-type", "application/json"))
            .and_then(|request| {
                request.header("authorization", &format!("Bearer {}", credential.expose()))
            })
            .map_err(|_| ClaudeVertexError::InvalidRequest)?;
        let mut events = http.sse(request, cancellation.clone());
        let mut observed = ProbeObservation::default();
        let mut count = 0usize;
        while let Some(event) = events.next().await {
            count = count.saturating_add(1);
            if count > MAX_PROBE_EVENTS {
                return Err(ClaudeVertexError::EventBudgetExceeded);
            }
            let event = match event {
                Ok(event) => event,
                Err(_) if cancellation.is_cancelled() => {
                    return Err(ClaudeVertexError::Cancelled);
                }
                Err(_) => return Err(ClaudeVertexError::Transport),
            };
            observed.observe(&event, &self.model.id)?;
        }
        if cancellation.is_cancelled() {
            return Err(ClaudeVertexError::Cancelled);
        }
        if observed.complete() {
            Ok(ClaudeVertexLiveEvidence {
                model: self.model.id.clone(),
            })
        } else {
            Err(ClaudeVertexError::IncompleteLiveEvidence)
        }
    }
}

/// Evidence minted only by a successful authenticated live probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeVertexLiveEvidence {
    model: String,
}

impl ClaudeVertexLiveEvidence {
    /// Model identity observed in `message_start`.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// A successful stream arrived after bearer authentication.
    #[must_use]
    pub const fn authenticated(&self) -> bool {
        true
    }

    /// The selected model emitted the forced client tool call.
    #[must_use]
    pub const fn tool_use_observed(&self) -> bool {
        true
    }

    /// The selected model emitted a thinking block.
    #[must_use]
    pub const fn thinking_observed(&self) -> bool {
        true
    }
}

struct ProbeObservation {
    phase: ProbePhase,
    active: Option<ProbeBlock>,
    next_index: u32,
    model: bool,
    thinking: bool,
    tool: bool,
    tool_stop: bool,
}

impl Default for ProbeObservation {
    fn default() -> Self {
        Self {
            phase: ProbePhase::AwaitingMessageStart,
            active: None,
            next_index: 0,
            model: false,
            thinking: false,
            tool: false,
            tool_stop: false,
        }
    }
}

impl ProbeObservation {
    fn observe(
        &mut self,
        event: &heycode_http::SseEvent,
        expected_model: &str,
    ) -> Result<(), ClaudeVertexError> {
        let transport_event = event.event.as_str();
        let value: serde_json::Value =
            serde_json::from_str(&event.data).map_err(|_| ClaudeVertexError::InvalidEvent)?;
        let event = value.as_object().ok_or(ClaudeVertexError::InvalidEvent)?;
        let kind = event
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or(ClaudeVertexError::InvalidEvent)?;
        if transport_event != kind {
            return Err(ClaudeVertexError::InvalidEvent);
        }
        if self.phase == ProbePhase::Stopped {
            return Err(ClaudeVertexError::InvalidEvent);
        }
        match kind {
            "message_start" => {
                if self.phase != ProbePhase::AwaitingMessageStart {
                    return Err(ClaudeVertexError::InvalidEvent);
                }
                let model = event
                    .get("message")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|message| message.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                if model != expected_model {
                    return Err(ClaudeVertexError::ModelMismatch);
                }
                self.model = true;
                self.phase = ProbePhase::ContentBlocks;
            }
            "content_block_start" => {
                if self.phase != ProbePhase::ContentBlocks || self.active.is_some() {
                    return Err(ClaudeVertexError::InvalidEvent);
                }
                let index = probe_index(event)?;
                if index != self.next_index {
                    return Err(ClaudeVertexError::InvalidEvent);
                }
                self.next_index = self
                    .next_index
                    .checked_add(1)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                let block = event
                    .get("content_block")
                    .and_then(serde_json::Value::as_object)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                let block_type = block
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                let kind = match block_type {
                    "thinking" => {
                        let signature = block
                            .get("signature")
                            .and_then(serde_json::Value::as_str)
                            .ok_or(ClaudeVertexError::InvalidEvent)?;
                        if !block
                            .get("thinking")
                            .is_some_and(serde_json::Value::is_string)
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        ProbeBlockKind::Thinking {
                            signature: !signature.is_empty(),
                        }
                    }
                    "redacted_thinking" => {
                        if block
                            .get("data")
                            .and_then(serde_json::Value::as_str)
                            .filter(|data| !data.is_empty())
                            .is_none()
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        ProbeBlockKind::Thinking { signature: true }
                    }
                    "tool_use" => {
                        if self.tool {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        let name = block
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .ok_or(ClaudeVertexError::InvalidEvent)?;
                        if name != PROBE_TOOL
                            || block
                                .get("id")
                                .and_then(serde_json::Value::as_str)
                                .is_none_or(str::is_empty)
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        let input = block
                            .get("input")
                            .filter(|input| input.is_object())
                            .cloned()
                            .ok_or(ClaudeVertexError::InvalidEvent)?;
                        ProbeBlockKind::ProbeTool {
                            start_input: input,
                            partial_json: String::new(),
                        }
                    }
                    _ => ProbeBlockKind::Other,
                };
                self.active = Some(ProbeBlock { index, kind });
            }
            "content_block_delta" => {
                let index = probe_index(event)?;
                let active = self
                    .active
                    .as_mut()
                    .filter(|active| active.index == index)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                let delta = event
                    .get("delta")
                    .and_then(serde_json::Value::as_object)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                let delta_type = delta
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                match (&mut active.kind, delta_type) {
                    (ProbeBlockKind::Thinking { .. }, "thinking_delta") => {
                        if !delta
                            .get("thinking")
                            .is_some_and(serde_json::Value::is_string)
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                    }
                    (ProbeBlockKind::Thinking { signature }, "signature_delta") => {
                        if delta
                            .get("signature")
                            .and_then(serde_json::Value::as_str)
                            .filter(|value| !value.is_empty())
                            .is_none()
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        if *signature {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        *signature = true;
                    }
                    (ProbeBlockKind::ProbeTool { partial_json, .. }, "input_json_delta") => {
                        let partial = delta
                            .get("partial_json")
                            .and_then(serde_json::Value::as_str)
                            .ok_or(ClaudeVertexError::InvalidEvent)?;
                        if partial_json.len().saturating_add(partial.len())
                            > MAX_PROBE_TOOL_INPUT_BYTES
                        {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        partial_json.push_str(partial);
                    }
                    (ProbeBlockKind::Other, _) => {}
                    _ => return Err(ClaudeVertexError::InvalidEvent),
                }
            }
            "content_block_stop" => {
                let index = probe_index(event)?;
                let active = self
                    .active
                    .take()
                    .filter(|active| active.index == index)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                match active.kind {
                    ProbeBlockKind::Thinking { signature: true } => self.thinking = true,
                    ProbeBlockKind::Thinking { signature: false } => {
                        return Err(ClaudeVertexError::InvalidEvent);
                    }
                    ProbeBlockKind::ProbeTool {
                        start_input,
                        partial_json,
                    } => {
                        let input = if partial_json.trim().is_empty() {
                            start_input
                        } else {
                            serde_json::from_str(&partial_json)
                                .map_err(|_| ClaudeVertexError::InvalidEvent)?
                        };
                        if input != serde_json::json!({ "ok": true }) {
                            return Err(ClaudeVertexError::InvalidEvent);
                        }
                        self.tool = true;
                    }
                    ProbeBlockKind::Other => {}
                }
            }
            "message_delta" => {
                if self.phase != ProbePhase::ContentBlocks || self.active.is_some() {
                    return Err(ClaudeVertexError::InvalidEvent);
                }
                let stop_reason = event
                    .get("delta")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ClaudeVertexError::InvalidEvent)?;
                if stop_reason == "tool_use" {
                    self.tool_stop = true;
                }
                self.phase = ProbePhase::MessageDelta;
            }
            "message_stop" => {
                if self.phase != ProbePhase::MessageDelta || self.active.is_some() {
                    return Err(ClaudeVertexError::InvalidEvent);
                }
                self.phase = ProbePhase::Stopped;
            }
            "error" => return Err(ClaudeVertexError::InvalidEvent),
            _ => {}
        }
        Ok(())
    }

    fn complete(&self) -> bool {
        matches!(self.phase, ProbePhase::Stopped)
            && self.model
            && self.thinking
            && self.tool
            && self.tool_stop
    }
}

fn probe_index(
    event: &serde_json::Map<String, serde_json::Value>,
) -> Result<u32, ClaudeVertexError> {
    event
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(ClaudeVertexError::InvalidEvent)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbePhase {
    AwaitingMessageStart,
    ContentBlocks,
    MessageDelta,
    Stopped,
}

struct ProbeBlock {
    index: u32,
    kind: ProbeBlockKind,
}

enum ProbeBlockKind {
    Thinking {
        signature: bool,
    },
    ProbeTool {
        start_input: serde_json::Value,
        partial_json: String,
    },
    Other,
}

pub(crate) fn provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: CLAUDE_VERTEX_PROVIDER.to_owned(),
        display_name: "Claude on Google Cloud".to_owned(),
        protocols: vec![heycode_core::ProviderProtocol::AnthropicMessages],
    }
}

pub(crate) fn sonnet_five_descriptor() -> ModelDescriptor {
    let mut capabilities = ModelCapabilities::unknown();
    capabilities.tools = CapabilitySupport::Supported;
    capabilities.reasoning = CapabilitySupport::Supported;
    capabilities.prompt_cache = CapabilitySupport::Supported;
    capabilities.image_input = CapabilitySupport::Supported;
    capabilities.document_input = CapabilitySupport::Supported;
    ModelDescriptor {
        display_name: "Claude Sonnet 5".to_owned(),
        id: CLAUDE_VERTEX_DEFAULT_MODEL.to_owned(),
        aliases: Vec::new(),
        created_at_ms: None,
        context_window: Some(1_000_000),
        max_output_tokens: Some(128_000),
        lifecycle: ModelLifecycle::stable(),
        capabilities,
        pricing: ModelPricing::unknown(),
        performance: ModelPerformance::unknown(),
        reasoning: None,
    }
}

/// Claude on Google Cloud profile or live-probe failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaudeVertexError {
    /// No usable ADC source is configured.
    #[error("Google Cloud ADC is absent or unusable")]
    AccountUnavailable,
    /// ADC discovery could not reach a determinate answer.
    #[error("Google Cloud ADC state is undetermined")]
    AccountUndetermined,
    /// No well-formed project is selected.
    #[error("Google Cloud project is unavailable")]
    ProjectUnavailable,
    /// No well-formed location is selected.
    #[error("Google Cloud location is unavailable")]
    LocationUnavailable,
    /// The maintained model is not documented on this location form.
    #[error("Claude Sonnet 5 is not configured for this Google Cloud location")]
    UnsupportedLocation,
    /// Fixed route endpoint failed URL validation.
    #[error("Claude on Google Cloud endpoint is invalid")]
    InvalidEndpoint,
    /// Messages body is not an object.
    #[error("Claude on Google Cloud Messages body is malformed")]
    MalformedBody,
    /// Body and endpoint model identities disagree.
    #[error("Claude on Google Cloud model identity does not match the endpoint")]
    ModelMismatch,
    /// Body carries a non-Vertex Anthropic version.
    #[error("Claude on Google Cloud Anthropic version is incompatible")]
    VersionMismatch,
    /// Body carries a thinking mode different from the resolved Sonnet choice.
    #[error("Claude on Google Cloud thinking mode is incompatible")]
    ThinkingMismatch,
    /// Body carries an effort value different from the resolved Sonnet choice.
    #[error("Claude on Google Cloud effort is incompatible")]
    EffortMismatch,
    /// `output_config` is not an object that can carry the resolved effort.
    #[error("Claude on Google Cloud output configuration is malformed")]
    MalformedOutputConfig,
    /// Sonnet 5 accepts only provider-default sampling, represented by omission.
    #[error("Claude Sonnet 5 does not accept explicit sampling controls")]
    SamplingUnsupported,
    /// Operation-time access token could not be resolved.
    #[error("Claude on Google Cloud credential is unavailable")]
    CredentialUnavailable,
    /// Profile credential is not an OAuth access token.
    #[error("Claude on Google Cloud requires an oauth-token credential")]
    CredentialKindMismatch,
    /// Safe HTTP request construction failed.
    #[error("Claude on Google Cloud request is invalid")]
    InvalidRequest,
    /// HTTP/SSE transport failed without exposing its body.
    #[error("Claude on Google Cloud transport failed")]
    Transport,
    /// Caller cancelled before live evidence settled.
    #[error("Claude on Google Cloud probe was cancelled")]
    Cancelled,
    /// A provider event could not be normalized safely.
    #[error("Claude on Google Cloud returned an invalid event")]
    InvalidEvent,
    /// Probe exceeded its bounded event budget.
    #[error("Claude on Google Cloud probe exceeded its event budget")]
    EventBudgetExceeded,
    /// Stream succeeded but did not prove every acceptance fact.
    #[error("Claude on Google Cloud probe did not prove model, tool, and thinking support")]
    IncompleteLiveEvidence,
}
