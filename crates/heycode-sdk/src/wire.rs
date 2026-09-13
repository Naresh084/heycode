//! Stable app-server v1 wire vocabulary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current stable heycode app-server protocol version.
pub const APP_SERVER_PROTOCOL_VERSION: u16 = 1;

/// Stable methods contributed by the default app-server control plugin.
pub const APP_CONTROL_METHODS: &[&str] = &[
    "authorization/list",
    "authorization/start",
    "authorization/answer",
    "authorization/cancel",
    "authorization/logout",
    "providers/list",
    "providers/select",
    "models/list",
    "models/select",
    "runtimes/list",
    "runtime/select",
    "workspace/select",
    "mcp/list",
    "plugins/list",
    "settings/list",
    "settings/get",
    "settings/replace",
];

/// Stable turn settlement reason on the heycode-owned wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppTurnReason {
    /// Normal completion.
    Stop,
    /// Provider/runtime output limit.
    Limit,
    /// Caller cancellation.
    Cancelled,
    /// Safe classified failure.
    Error,
}

/// Exact current-context evidence reported by a delegated runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRuntimeContextUsage {
    /// Tokens currently occupying the model-visible request context.
    pub tokens: u64,
    /// Provider-reported capacity for the request's model.
    pub context_window: u64,
    /// Concrete model identity reported for this request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
}

/// Closed stable event payload emitted through session/control notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppServerEvent {
    /// Durable user input accepted for this turn.
    UserInput {
        /// Exact user text.
        text: String,
        /// Resolved durable attachment selections.
        attachments: Vec<heycode_core::AttachmentMetadata>,
        /// Explicit document routes.
        document_routes: Vec<heycode_core::DocumentInputRoute>,
    },
    /// Turn began.
    TurnStarted {
        /// Runtime-native turn id.
        turn_id: String,
    },
    /// Assistant-visible text fragment.
    AssistantDelta {
        /// Fragment.
        text: String,
    },
    /// Assistant audio committed as immutable ATT01 metadata.
    AssistantAudio {
        /// One to four exact audio records; encoded bytes never cross JSON.
        attachments: Vec<heycode_core::AttachmentMetadata>,
    },
    /// Reasoning/thought fragment.
    ReasoningDelta {
        /// Fragment.
        text: String,
    },
    /// Tool execution began.
    ToolStarted {
        /// Durable call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Validated structured arguments.
        arguments: Value,
    },
    /// Tool execution settled.
    ToolFinished {
        /// Durable call id.
        call_id: String,
        /// Tool name retained from its start event.
        name: String,
        /// Structured result.
        result: Value,
        /// False for denied/failed operations.
        ok: bool,
        /// Typed external-content marker.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    },
    /// Current request budget, independent of billing usage.
    ContextBudgetChanged {
        /// Model capacity, measurement confidence and compaction state.
        budget: Box<heycode_core::ContextBudget>,
    },
    /// Provider/runtime usage for one model step.
    Usage {
        /// Reported usage.
        usage: heycode_core::TokenUsage,
        /// Exact latest-request occupancy and capacity, when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<AppRuntimeContextUsage>,
    },
    /// Plan-mode state changed.
    PlanChanged {
        /// Whether mutation remains blocked for planning.
        active: bool,
    },
    /// Safe non-model notice.
    Notice {
        /// Stable code.
        code: String,
        /// Safe human text.
        message: String,
    },
    /// A masked authorization input dialog should open.
    AuthorizationPromptRequested {
        /// Broker-owned prompt id used only for answer/cancel correlation.
        prompt_id: u64,
        /// Safe human prompt.
        prompt: String,
        /// Non-secret credential reference.
        reference: String,
        /// Semantic credential kind.
        kind: String,
        /// Must be true for secret entry.
        masked: bool,
    },
    /// A masked authorization dialog settled.
    AuthorizationPromptResolved {
        /// Broker-owned prompt id.
        prompt_id: u64,
        /// Whether the broker accepted an answer; the answer is never present.
        answered: bool,
    },
    /// The active runtime requests a permission decision.
    PermissionRequested {
        /// Runtime-native request correlation.
        request_id: String,
        /// Safe action title.
        action: String,
        /// Safe bounded detail.
        detail: String,
    },
    /// The active runtime asks one human question.
    QuestionRequested {
        /// Exact requesting session, absent on older servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_session_id: Option<String>,
        /// Explicit answer mode for shared host questions.
        #[serde(default)]
        mode: heycode_core::QuestionMode,
        /// One-based position and total; absent legacy events default to zero.
        #[serde(default)]
        progress: (usize, usize),
        /// Runtime-native request correlation.
        request_id: String,
        /// Optional short category label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        /// Safe prompt text.
        prompt: String,
        /// Ordered choices; empty permits free text.
        choices: Vec<String>,
        /// Explanations aligned one-for-one with `choices`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        choice_descriptions: Vec<Option<String>>,
    },
    /// Turn settled.
    TurnFinished {
        /// Runtime-native turn id.
        turn_id: String,
        /// Settlement reason.
        reason: AppTurnReason,
        /// Latest provider usage, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<heycode_core::TokenUsage>,
    },
}

/// One monotonically sequenced stable event notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServerNotification {
    /// JSON-RPC version.
    pub jsonrpc: String,
    /// `session/event` for turn state or `control/event` for dialogs.
    pub method: String,
    /// Notification parameters.
    pub params: AppServerEventParams,
}

/// Parameters of a stable event notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServerEventParams {
    /// Durable session id for session events; absent for control events.
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// App-server-local sequence.
    pub sequence: u64,
    /// Closed event payload.
    pub event: AppServerEvent,
}

/// Current local session facts returned by `session/open`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSessionInfo {
    /// Durable heycode session id.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Runtime implementation id.
    #[serde(rename = "runtimeId")]
    pub runtime_id: String,
    /// Absolute workspace.
    pub cwd: std::path::PathBuf,
    /// Effective model-visible runtime configuration.
    #[serde(default)]
    pub configuration: AppRuntimeConfiguration,
    /// Runtime support evidence for each configuration field.
    #[serde(default, rename = "configurationCapabilities")]
    pub configuration_capabilities: AppRuntimeConfigurationCapabilities,
    /// Metadata advertised by this exact live session for the configured
    /// model control value, when available.
    #[serde(
        default,
        rename = "modelConfiguration",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_configuration: Option<AppRuntimeModelConfiguration>,
}

/// Optional model-visible controls accepted by `session/open` and
/// `session/configure`.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRuntimeConfiguration {
    /// Exact replacement system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Exact host tools to expose. `None` uses the host registry; an empty
    /// vector explicitly exposes no tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<heycode_core::ToolSpec>>,
    /// Provider-native model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider-native reasoning/thought effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl std::fmt::Debug for AppRuntimeConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppRuntimeConfiguration")
            .field(
                "system_prompt_bytes",
                &self.system_prompt.as_ref().map(String::len),
            )
            .field("tool_count", &self.tools.as_ref().map(Vec::len))
            .field("model", &self.model.as_ref().map(|_| "<redacted>"))
            .field(
                "reasoning_effort",
                &self.reasoning_effort.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Evidence for runtime configuration fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRuntimeConfigurationCapabilities {
    /// System-prompt replacement support.
    pub system_prompt: AppCapabilityEvidence,
    /// Host-defined tool support.
    pub tools: AppCapabilityEvidence,
    /// Model selection support.
    pub model: AppCapabilityEvidence,
    /// Reasoning-effort selection support.
    pub reasoning_effort: AppCapabilityEvidence,
}

impl Default for AppRuntimeConfigurationCapabilities {
    fn default() -> Self {
        Self {
            system_prompt: AppCapabilityEvidence::Unknown,
            tools: AppCapabilityEvidence::Unknown,
            model: AppCapabilityEvidence::Unknown,
            reasoning_effort: AppCapabilityEvidence::Unknown,
        }
    }
}

/// Terminal response of one app-server turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppTurnResult {
    /// Runtime-native turn id, when admitted.
    #[serde(rename = "turnId")]
    pub turn_id: Option<String>,
    /// Settlement reason.
    pub reason: AppTurnReason,
}

/// Permission decision sent back to an active runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPermissionDecision {
    /// Allow only this operation.
    AllowOnce,
    /// Allow matching operations for this runtime session.
    AllowSession,
    /// Deny while allowing the turn to continue.
    Deny,
}

/// Safe app-server operation failure code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppServerErrorCode {
    /// Malformed request or response.
    InvalidRequest,
    /// Unknown method.
    MethodNotFound,
    /// Session/turn/settings conflict.
    Conflict,
    /// Caller cancellation.
    Cancelled,
    /// The named thing exists and is spelled correctly, but the capability it
    /// needs is not installed in this composition. Distinct from
    /// [`Self::InvalidRequest`] because "unknown" sends a caller looking for a
    /// typo in a name that is not misspelled, and distinct from
    /// [`Self::Unavailable`] because nothing is degraded or down.
    Unsupported,
    /// Service/runtime unavailable.
    Unavailable,
    /// Closed service/session.
    Closed,
    /// Stable internal failure.
    Internal,
}

impl AppServerErrorCode {
    /// Fixed body-free human message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "app-server request is invalid",
            Self::MethodNotFound => "app-server method not found",
            Self::Conflict => "app-server operation conflicts",
            Self::Cancelled => "app-server operation was cancelled",
            Self::Unsupported => "app-server selection is known but not installed here",
            Self::Unavailable => "app-server is unavailable",
            Self::Closed => "app-server is closed",
            Self::Internal => "app-server operation failed",
        }
    }
}

/// Body-free stable app-server error.
///
/// The message is fixed per code. An optional `detail` names the cause in one
/// safe line — a runtime's own redacted diagnostic such as "Codex CLI is
/// unavailable" — and crosses the JSON-RPC wire as `error.data.detail`. It is
/// never a provider body or anything a caller did not declare safe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct AppServerError {
    code: AppServerErrorCode,
    detail: Option<String>,
}

impl std::fmt::Display for AppServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code.message())?;
        if let Some(detail) = &self.detail {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

/// Longest detail an error may carry.
pub const MAX_ERROR_DETAIL_BYTES: usize = 512;

impl AppServerError {
    /// Construct one classified body-free error.
    #[must_use]
    pub const fn classified(code: AppServerErrorCode) -> Self {
        Self { code, detail: None }
    }

    /// Attach a caller-declared safe, trimmed one-line cause.
    ///
    /// # Errors
    /// Empty, over-[`MAX_ERROR_DETAIL_BYTES`] or control-bearing text is
    /// rejected so provider bodies and tokens cannot become diagnostics.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Result<Self, Self> {
        let detail = detail.into();
        if detail.is_empty()
            || detail.trim() != detail
            || detail.len() > MAX_ERROR_DETAIL_BYTES
            || detail.chars().any(char::is_control)
        {
            return Err(self);
        }
        self.detail = Some(detail);
        Ok(self)
    }

    /// The safe cause attached to this error, if any.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Standard invalid-request/response failure.
    #[must_use]
    pub const fn invalid() -> Self {
        Self::classified(AppServerErrorCode::InvalidRequest)
    }

    /// Standard unavailable-service failure.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self::classified(AppServerErrorCode::Unavailable)
    }

    /// Standard caller-cancelled failure.
    #[must_use]
    pub const fn cancelled() -> Self {
        Self::classified(AppServerErrorCode::Cancelled)
    }

    /// Stable error code.
    #[must_use]
    pub const fn code(&self) -> AppServerErrorCode {
        self.code
    }

    /// Fixed safe message.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.code.message()
    }
}

/// Refresh policy accepted by `models/list`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCatalogRefresh {
    /// Reuse a still-fresh generation, otherwise refresh.
    #[default]
    PreferCache,
    /// Require a live refresh attempt.
    Force,
}

/// Stable initialization response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppInitializeResult {
    /// Negotiated heycode protocol version.
    pub protocol_version: u16,
    /// Safe server identity.
    pub server: AppServerIdentity,
    /// Installed protocol surfaces.
    pub capabilities: AppServerCapabilities,
}

/// Safe server identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppServerIdentity {
    /// Product id.
    pub name: String,
    /// Implementation version.
    pub version: String,
}

/// Installed app-server protocol surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppServerCapabilities {
    /// Session turn methods.
    pub turns: bool,
    /// Durable attachment metadata on turns.
    pub attachments: bool,
    /// Turn cancellation.
    pub cancel: bool,
    /// Authorization catalog and interactive flow methods.
    pub authorization: bool,
    /// Provider/model catalog and selection methods.
    pub models: bool,
    /// Agent-runtime discovery and selection methods.
    ///
    /// `#[serde(default)]` because this field was added after the protocol
    /// shipped. An absent capability flag means the server did not advertise
    /// the surface, which for a capability is exactly `false` — fail closed.
    /// Without the default, a client built today could not parse an older
    /// server's initialize response at all, losing the whole session over one
    /// optional flag.
    #[serde(default)]
    pub runtimes: bool,
    /// Client-selected session workspace.
    ///
    /// Defaulted for the same reason as [`Self::runtimes`].
    #[serde(default)]
    pub workspace: bool,
    /// Redacted MCP registry inspection.
    pub mcp: bool,
    /// Exact plugin inventory inspection.
    pub plugins: bool,
    /// Explicitly wire-exposed settings inspection/mutation.
    pub settings: bool,
}

/// Wire-safe credential validation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AppCredentialValidation {
    /// No current validation proof.
    Unknown,
    /// Validation succeeded.
    Valid {
        /// Validation instant.
        checked_at_ms: u64,
    },
    /// Validation failed with a body-free code.
    Invalid {
        /// Validation instant.
        checked_at_ms: u64,
        /// Safe reason code.
        reason: String,
    },
    /// Prior validation proof expired.
    Stale {
        /// Original validation instant.
        checked_at_ms: u64,
    },
}

/// Wire-safe credential status with no value field by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppCredentialStatus {
    /// Non-secret reference.
    pub reference: String,
    /// Semantic credential kind.
    pub kind: String,
    /// Whether provider inspection ran for this response.
    pub inspected: bool,
    /// Whether an authoritative provider holds a value.
    pub configured: Option<bool>,
    /// Safe provenance id.
    pub source: Option<String>,
    /// Authoritative/preferred credential provider id.
    pub provider: Option<String>,
    /// Whether the provider can accept writes.
    pub writable: Option<bool>,
    /// Safe validation state.
    pub validation: AppCredentialValidation,
}

/// One contributed authorization flow plus credential status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppAuthorizationFlow {
    /// Flow id.
    pub id: String,
    /// Human label.
    pub label: String,
    /// Stable interaction mechanism.
    pub method: String,
    /// Whether human interaction is expected.
    pub interactive: bool,
    /// Unambiguous provider owner, when present.
    pub provider: Option<String>,
    /// Current safe credential state.
    pub credential: AppCredentialStatus,
}

/// Secret-free proof returned only after registry-owned commit/readback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppAuthorizationReceipt {
    /// Effective provider bound to the flow.
    pub provider: String,
    /// Flow id.
    pub flow: String,
    /// Credential provider that accepted the write.
    pub committed_by: String,
    /// Authoritative post-commit status.
    pub credential: AppCredentialStatus,
}

/// Logout result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppLogoutResult {
    /// Provider from which a record was deleted, or absent if already missing.
    pub deleted_from: Option<String>,
}

/// Current persisted route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRouteSelection {
    /// Agent runtime id.
    pub runtime: String,
    /// Inference provider id.
    pub provider: String,
    /// Provider-native model id.
    pub model: String,
    /// Provider-owned reasoning effort, when available.
    pub effort: Option<String>,
}

/// One registered provider profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppProviderRow {
    /// Registry/routing id.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// Provider-owned default model.
    pub default_model: String,
    /// Non-secret credential reference.
    pub credential_reference: Option<String>,
    /// Advertised protocol families.
    pub protocols: Vec<heycode_core::ProviderProtocol>,
}

/// Provider picker snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppProviderCatalog {
    /// Current durable route.
    pub current: AppRouteSelection,
    /// Registered providers in stable id order.
    pub providers: Vec<AppProviderRow>,
}

/// Whether a runtime can run against a client-selected workspace.
///
/// A native runtime *is* the in-process agent, whose tool roots were fixed
/// when the world was composed, so handing it a different workspace would be
/// read by nothing downstream. A delegated runtime receives the workspace on
/// every start/resume request and runs its own process there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppRuntimeWorkspace {
    /// The workspace is fixed by the host composition.
    Composed,
    /// `workspace/select` governs where this runtime runs.
    Selectable,
}

/// Evidence-backed optional operations of one agent runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRuntimeCapabilities {
    /// Runtime-owned model discovery.
    pub models: AppCapabilityEvidence,
    /// Resuming a provider-native session.
    pub resume: AppCapabilityEvidence,
    /// Forking a provider-native session.
    pub fork: AppCapabilityEvidence,
    /// Injecting text into the active turn.
    pub steer: AppCapabilityEvidence,
    /// Queueing text for the next turn.
    pub follow_up: AppCapabilityEvidence,
    /// Correlated permission requests.
    pub permissions: AppCapabilityEvidence,
    /// Correlated human questions.
    pub questions: AppCapabilityEvidence,
    /// Runtime-native compaction.
    pub compaction: AppCapabilityEvidence,
}

/// One registered agent runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRuntimeRow {
    /// Registry/routing id.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// `native` when this runtime is the in-process heycode loop, `delegated`
    /// when it owns its own turn loop.
    pub kind: String,
    /// Whether `workspace/select` reaches this runtime.
    pub workspace: AppRuntimeWorkspace,
    /// Exact capability evidence. `unknown` never becomes `supported`.
    pub capabilities: AppRuntimeCapabilities,
    /// Model-visible configuration support.
    #[serde(default)]
    pub configuration: AppRuntimeConfigurationCapabilities,
}

/// One runtime-owned model with exact provider-advertised effort choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRuntimeModelConfiguration {
    /// Exact provider-native value accepted by the runtime control wire.
    pub model: String,
    /// Provider-advertised human label. Empty only when decoding an older
    /// peer that did not expose model metadata.
    #[serde(default)]
    pub display_name: String,
    /// Provider-advertised resolved/canonical identity, when distinct.
    #[serde(default)]
    pub resolved_model: Option<String>,
    /// Provider-advertised description, when present.
    #[serde(default)]
    pub description: Option<String>,
    /// Structured input-context limit, when advertised.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Provider-advertised default effort, when present.
    pub default_reasoning_effort: Option<String>,
    /// Ordered provider-native effort ids.
    pub reasoning_efforts: Vec<String>,
}

/// Runtime picker snapshot.
///
/// Every registered runtime is listed. Whether `runtime/select` admits one is
/// the routing owner's decision and is reported by that call, not guessed
/// here: a second copy of the admission predicate would be a second
/// behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRuntimeCatalog {
    /// Current durable route.
    pub current: AppRouteSelection,
    /// Registered runtimes in stable id order.
    pub runtimes: Vec<AppRuntimeRow>,
}

/// Effective workspace after one `workspace/select`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppWorkspaceSelection {
    /// Canonical absolute workspace every later runtime session opens against.
    pub cwd: std::path::PathBuf,
    /// Runtime id that receives it.
    pub runtime: String,
    /// False when this is the host's own workspace rather than a selection.
    pub selected: bool,
}

/// Explicit tri-state model capability evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCapabilityEvidence {
    /// Trustworthy support evidence.
    Supported,
    /// Trustworthy unsupported evidence.
    Unsupported,
    /// No trustworthy evidence.
    Unknown,
}

/// Model capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppModelCapabilities {
    /// Function/tool calling.
    pub tools: AppCapabilityEvidence,
    /// Reasoning state.
    pub reasoning: AppCapabilityEvidence,
    /// Image input.
    pub image_input: AppCapabilityEvidence,
    /// Native document input.
    pub document_input: AppCapabilityEvidence,
    /// Structured output.
    pub structured_output: AppCapabilityEvidence,
    /// Provider-hosted web.
    pub native_web: AppCapabilityEvidence,
    /// Provider-native compaction.
    pub native_compaction: AppCapabilityEvidence,
    /// Prompt caching.
    pub prompt_cache: AppCapabilityEvidence,
}

/// Effective model lifecycle evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppModelLifecycle {
    /// No trustworthy lifecycle evidence.
    Unknown,
    /// Generally available.
    Stable,
    /// Preview/experimental.
    Preview,
    /// Selectable but retiring.
    Deprecated,
    /// Not selectable for a new request.
    Retired,
}

/// One model picker row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppModelRow {
    /// Provider-native id.
    pub id: String,
    /// Human display name.
    pub display_name: String,
    /// Provider aliases.
    pub aliases: Vec<String>,
    /// Input context size.
    pub context_window: Option<u64>,
    /// Output ceiling.
    pub max_output_tokens: Option<u64>,
    /// Effective lifecycle.
    pub lifecycle: AppModelLifecycle,
    /// Whether the row can be selected now.
    pub selectable: bool,
    /// Provider-recommended replacements.
    pub replacement_ids: Vec<String>,
    /// Exact capability evidence.
    pub capabilities: AppModelCapabilities,
}

/// How a model snapshot was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCatalogFreshness {
    /// This request completed/joined a live refresh.
    Live,
    /// A still-fresh cached generation was reused.
    FreshCache,
    /// A failed refresh retained stale last-good data.
    StaleFallback,
    /// No generation existed; provider default is visible.
    DefaultFallback,
}

/// Safe recoverable control warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppControlWarning {
    /// Stable reason code.
    pub code: String,
    /// Fixed safe human message.
    pub message: String,
}

/// One model catalog result suitable for a picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppModelCatalog {
    /// Provider owning these ids.
    pub provider: String,
    /// Current selected model.
    pub current_model: String,
    /// Provider-owned fallback/default id.
    pub default_model: String,
    /// Generation revision.
    pub revision: Option<u64>,
    /// Successful fetch timestamp.
    pub fetched_at_ms: Option<u64>,
    /// Snapshot provenance.
    pub freshness: AppCatalogFreshness,
    /// Visible warning.
    pub warning: Option<AppControlWarning>,
    /// Stable model rows.
    pub models: Vec<AppModelRow>,
}

/// One applied plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPluginRow {
    /// Stable plugin id.
    pub id: String,
    /// Implementation version.
    pub version: String,
    /// Implementation provenance.
    pub source: String,
    /// Effective activation scope.
    pub scope: String,
}

/// One exact plugin-owned contribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPluginContribution {
    /// Owning plugin id.
    pub plugin: String,
    /// Exact contribution namespace.
    pub kind: String,
    /// Exact row name.
    pub name: String,
}

/// Exact live plugin inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPluginInventory {
    /// Applied plugins in composition order.
    pub plugins: Vec<AppPluginRow>,
    /// Exact committed rows in declaration order.
    pub contributions: Vec<AppPluginContribution>,
}

/// One settings namespace. Values are absent unless `exposed` is true.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSettingsSnapshot {
    /// Namespace id.
    pub namespace: String,
    /// Whether exposure was **proved**, not merely attested: every projected
    /// path is classified and no unclassified credential material remains.
    pub exposed: bool,
    /// Whether a user section can be durably replaced.
    pub writable: bool,
    /// `live` or `restart` application timing.
    pub applies: String,
    /// User-section CAS revision.
    pub revision: u64,
    /// JSON schema metadata when exposed.
    pub schema: Option<Value>,
    /// Schema defaults when exposed.
    pub defaults: Option<Value>,
    /// Composition base when exposed.
    pub base: Option<Value>,
    /// Raw persisted user section when exposed.
    pub user: Option<Value>,
    /// Trusted project section when exposed.
    pub project: Option<Value>,
    /// Administrator-managed section when exposed. Highest precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<Value>,
    /// Effective merged value when exposed.
    pub resolved: Option<Value>,
    /// Paths the managed layer locks. A user write naming one is refused, so a
    /// client must not offer them as editable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_locks: Vec<String>,
    /// Paths whose values were replaced by the redaction placeholder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_paths: Vec<String>,
}

#[cfg(test)]
mod question_owner_compatibility_tests {
    #[test]
    fn old_question_events_have_no_invented_sender_and_new_source_round_trips()
    -> Result<(), serde_json::Error> {
        let legacy = serde_json::json!({"type":"question_requested","request_id":"opaque","prompt":"Q?","choices":[]});
        let event: super::AppServerEvent = serde_json::from_value(legacy.clone())?;
        assert!(matches!(
            event,
            super::AppServerEvent::QuestionRequested {
                owner_session_id: None,
                ..
            }
        ));
        let mut sourced = legacy;
        sourced["owner_session_id"] = serde_json::json!("exact-origin");
        let event: super::AppServerEvent = serde_json::from_value(sourced)?;
        assert_eq!(
            serde_json::to_value(event)?["owner_session_id"],
            "exact-origin"
        );
        Ok(())
    }
}
