//! Secret-safe definitions and immutable public snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::{McpConnectionProviderId, McpRegistryError, McpSecretReference, McpServerId};

const MAX_LITERAL_BYTES: usize = 16 * 1024;
const MAX_DISPLAY_BYTES: usize = 128;
const MAX_OPAQUE_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_URL_BYTES: usize = 2 * 1024;
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;
const MAX_ARGUMENTS: usize = 256;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_HEADERS: usize = 128;
const MAX_TOOL_POLICY_ROWS: usize = 4_096;

/// Stable schema version for serialized [`McpSnapshot`] diagnostics.
pub const MCP_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Trusted origin layer for one effective MCP definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDefinitionScope {
    /// User-wide definition.
    User,
    /// Trusted workspace definition.
    Project,
    /// Machine-local workspace override.
    Local,
    /// Organization-managed definition.
    Managed,
    /// Definition contributed by an installed plugin package.
    Plugin,
}

/// Transport family selected by a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransportKind {
    /// Exact-argv child process over newline-framed stdio.
    Stdio,
    /// MCP Streamable HTTP endpoint.
    StreamableHttp,
}

/// One exact stdio argument. Literal bytes remain private to transport
/// providers; public snapshots expose only its position and source kind.
#[derive(Clone, PartialEq, Eq)]
pub struct McpArgument {
    value: ExactValue,
}

impl McpArgument {
    /// Construct a literal argument retained only in the exact definition.
    ///
    /// # Errors
    /// NUL-bearing or over-16-KiB values are rejected. Other bytes, including
    /// newlines, remain exact and private for shell-free argv compatibility.
    pub fn literal(value: impl Into<String>) -> Result<Self, McpRegistryError> {
        let value = value.into();
        validate_literal(&value, "stdio argument")?;
        Ok(Self {
            value: ExactValue::Literal(value),
        })
    }

    /// Construct an argument resolved from a credential reference at launch.
    #[must_use]
    pub const fn credential(reference: McpSecretReference) -> Self {
        Self {
            value: ExactValue::Credential(reference),
        }
    }

    /// Literal value for a transport provider; absent for credential-backed arguments.
    #[must_use]
    pub fn literal_value(&self) -> Option<&str> {
        match &self.value {
            ExactValue::Literal(value) => Some(value),
            ExactValue::Credential(_) => None,
        }
    }

    /// Credential reference for a transport provider; absent for literals.
    #[must_use]
    pub const fn credential_reference(&self) -> Option<&McpSecretReference> {
        match &self.value {
            ExactValue::Literal(_) => None,
            ExactValue::Credential(reference) => Some(reference),
        }
    }
}

/// One exact stdio environment value. Literal bytes never enter snapshots.
#[derive(Clone, PartialEq, Eq)]
pub struct McpEnvironmentValue {
    value: ExactValue,
}

impl McpEnvironmentValue {
    /// Construct a literal environment value retained only in the exact definition.
    ///
    /// # Errors
    /// NUL-bearing or over-16-KiB values are rejected. Other bytes remain
    /// exact and private to the launch provider.
    pub fn literal(value: impl Into<String>) -> Result<Self, McpRegistryError> {
        let value = value.into();
        validate_literal(&value, "environment value")?;
        Ok(Self {
            value: ExactValue::Literal(value),
        })
    }

    /// Construct a value resolved from a credential reference at launch.
    #[must_use]
    pub const fn credential(reference: McpSecretReference) -> Self {
        Self {
            value: ExactValue::Credential(reference),
        }
    }

    /// Literal value for a transport provider; absent for credential-backed values.
    #[must_use]
    pub fn literal_value(&self) -> Option<&str> {
        match &self.value {
            ExactValue::Literal(value) => Some(value),
            ExactValue::Credential(_) => None,
        }
    }

    /// Credential reference for a transport provider; absent for literals.
    #[must_use]
    pub const fn credential_reference(&self) -> Option<&McpSecretReference> {
        match &self.value {
            ExactValue::Literal(_) => None,
            ExactValue::Credential(reference) => Some(reference),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ExactValue {
    Literal(String),
    Credential(McpSecretReference),
}

/// Exact stdio transport definition. It deliberately implements neither
/// `Debug` nor serialization because arguments/environment may be sensitive.
#[derive(Clone, PartialEq, Eq)]
pub struct McpStdioTransport {
    command: String,
    cwd: PathBuf,
    arguments: Vec<McpArgument>,
    environment: BTreeMap<String, McpEnvironmentValue>,
}

impl McpStdioTransport {
    /// Validate an exact stdio definition before registry publication.
    ///
    /// # Errors
    /// Invalid command/cwd, excessive argument/environment counts or unsafe
    /// environment names fail without retaining a partial definition.
    pub fn new(
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        arguments: Vec<McpArgument>,
        environment: BTreeMap<String, McpEnvironmentValue>,
    ) -> Result<Self, McpRegistryError> {
        let command = command.into();
        validate_one_line(&command, "stdio command", 1, 1_024)?;
        let cwd = cwd.into();
        validate_absolute_path(&cwd, "stdio cwd")?;
        if arguments.len() > MAX_ARGUMENTS {
            return Err(McpRegistryError::invalid(
                "stdio arguments",
                "at most 256 entries",
            ));
        }
        if environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err(McpRegistryError::invalid(
                "stdio environment",
                "at most 256 entries",
            ));
        }
        for name in environment.keys() {
            validate_environment_name(name)?;
        }
        Ok(Self {
            command,
            cwd,
            arguments,
            environment,
        })
    }

    /// Executable name or absolute executable path.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Absolute working directory resolved before transport launch.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Ordered exact arguments for the transport provider.
    #[must_use]
    pub fn arguments(&self) -> &[McpArgument] {
        &self.arguments
    }

    /// Exact environment sources keyed deterministically by variable name.
    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, McpEnvironmentValue> {
        &self.environment
    }
}

/// Exact Streamable HTTP definition. Header values are references only; no
/// static credential value can enter this type or its public snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct McpStreamableHttpTransport {
    url: String,
    headers: BTreeMap<String, McpSecretReference>,
}

impl McpStreamableHttpTransport {
    /// Validate a Streamable HTTP endpoint and credential-reference headers.
    ///
    /// URL userinfo, query and fragment components are rejected so tokens
    /// cannot be hidden in a definition's inspectable endpoint.
    ///
    /// # Errors
    /// Invalid/ambiguous endpoints, header names or excessive headers.
    pub fn new(
        url: impl Into<String>,
        headers: BTreeMap<String, McpSecretReference>,
    ) -> Result<Self, McpRegistryError> {
        let url = url.into();
        validate_http_url(&url)?;
        if headers.len() > MAX_HEADERS {
            return Err(McpRegistryError::invalid(
                "HTTP headers",
                "at most 128 credential-reference headers",
            ));
        }
        let mut normalized = BTreeMap::new();
        for (name, reference) in headers {
            validate_header_name(&name)?;
            let name = name.to_ascii_lowercase();
            if normalized.insert(name, reference).is_some() {
                return Err(McpRegistryError::invalid(
                    "HTTP headers",
                    "case-insensitive names must be unique",
                ));
            }
        }
        Ok(Self {
            url,
            headers: normalized,
        })
    }

    /// Query/fragment/userinfo-free endpoint URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Header names mapped only to safe credential references.
    #[must_use]
    pub const fn headers(&self) -> &BTreeMap<String, McpSecretReference> {
        &self.headers
    }
}

/// Exact transport definition consumed by stdio/HTTP provider plugins.
#[derive(Clone, PartialEq, Eq)]
pub enum McpTransportDefinition {
    /// Child-process transport.
    Stdio(McpStdioTransport),
    /// Remote Streamable HTTP transport.
    StreamableHttp(McpStreamableHttpTransport),
}

impl McpTransportDefinition {
    /// Closed transport family.
    #[must_use]
    pub const fn kind(&self) -> McpTransportKind {
        match self {
            Self::Stdio(_) => McpTransportKind::Stdio,
            Self::StreamableHttp(_) => McpTransportKind::StreamableHttp,
        }
    }

    /// Safe credential references required by this exact transport.
    #[must_use]
    pub fn credential_references(&self) -> Vec<&McpSecretReference> {
        match self {
            Self::Stdio(transport) => transport
                .arguments
                .iter()
                .filter_map(McpArgument::credential_reference)
                .chain(
                    transport
                        .environment
                        .values()
                        .filter_map(McpEnvironmentValue::credential_reference),
                )
                .collect(),
            Self::StreamableHttp(transport) => transport.headers.values().collect(),
        }
    }

    fn has_credential_references(&self) -> bool {
        match self {
            Self::Stdio(transport) => {
                transport
                    .arguments
                    .iter()
                    .any(|argument| argument.credential_reference().is_some())
                    || transport
                        .environment
                        .values()
                        .any(|value| value.credential_reference().is_some())
            }
            Self::StreamableHttp(transport) => !transport.headers.is_empty(),
        }
    }
}

/// Explicit startup/request/idle/shutdown budgets in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct McpTimeouts {
    startup_ms: u64,
    request_ms: u64,
    idle_ms: u64,
    shutdown_ms: u64,
}

impl McpTimeouts {
    /// Validate all four non-zero budgets.
    ///
    /// # Errors
    /// Every value must be 1..=24 hours.
    pub fn new(
        startup_ms: u64,
        request_ms: u64,
        idle_ms: u64,
        shutdown_ms: u64,
    ) -> Result<Self, McpRegistryError> {
        for (field, value) in [
            ("startup timeout", startup_ms),
            ("request timeout", request_ms),
            ("idle timeout", idle_ms),
            ("shutdown timeout", shutdown_ms),
        ] {
            if !(1..=MAX_TIMEOUT_MS).contains(&value) {
                return Err(McpRegistryError::invalid(
                    field,
                    "1..=86400000 milliseconds",
                ));
            }
        }
        Ok(Self {
            startup_ms,
            request_ms,
            idle_ms,
            shutdown_ms,
        })
    }

    /// Startup budget.
    #[must_use]
    pub const fn startup_ms(self) -> u64 {
        self.startup_ms
    }

    /// Request budget.
    #[must_use]
    pub const fn request_ms(self) -> u64 {
        self.request_ms
    }

    /// Idle budget.
    #[must_use]
    pub const fn idle_ms(self) -> u64 {
        self.idle_ms
    }

    /// Quiescent shutdown budget.
    #[must_use]
    pub const fn shutdown_ms(self) -> u64 {
        self.shutdown_ms
    }
}

impl Default for McpTimeouts {
    /// Startup is bounded tighter than requests: every configured server is
    /// connected before the shell appears, so one server that never answers
    /// `initialize` must not hold the whole product blank for half a minute.
    /// A server that legitimately needs longer sets `startup_ms` explicitly.
    fn default() -> Self {
        Self {
            startup_ms: 10_000,
            request_ms: 600_000,
            idle_ms: 300_000,
            shutdown_ms: 5_000,
        }
    }
}

/// Bounded reconnect policy attached to one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct McpReconnectPolicy {
    enabled: bool,
    initial_delay_ms: u64,
    max_delay_ms: u64,
    max_attempts: u32,
}

impl McpReconnectPolicy {
    /// Validate a resolved reconnect policy.
    ///
    /// # Errors
    /// Delays must be non-zero, bounded and ordered; attempts must be non-zero.
    pub fn new(
        enabled: bool,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        max_attempts: u32,
    ) -> Result<Self, McpRegistryError> {
        if !(1..=MAX_TIMEOUT_MS).contains(&initial_delay_ms)
            || !(1..=MAX_TIMEOUT_MS).contains(&max_delay_ms)
            || initial_delay_ms > max_delay_ms
        {
            return Err(McpRegistryError::invalid(
                "reconnect delays",
                "ordered values in 1..=86400000 milliseconds",
            ));
        }
        if max_attempts == 0 || max_attempts > 1_000_000 {
            return Err(McpRegistryError::invalid(
                "reconnect attempts",
                "1..=1000000 attempts",
            ));
        }
        Ok(Self {
            enabled,
            initial_delay_ms,
            max_delay_ms,
            max_attempts,
        })
    }

    /// Whether automatic reconnect is permitted.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Initial backoff delay.
    #[must_use]
    pub const fn initial_delay_ms(self) -> u64 {
        self.initial_delay_ms
    }

    /// Backoff ceiling.
    #[must_use]
    pub const fn max_delay_ms(self) -> u64 {
        self.max_delay_ms
    }

    /// Consecutive-attempt budget.
    #[must_use]
    pub const fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// Backoff before the given one-based attempt of one recovery episode.
    ///
    /// The first attempt is immediate; every later attempt doubles the
    /// previous delay from `initial_delay_ms` and saturates at
    /// `max_delay_ms`, so a crash loop slows down instead of spinning.
    #[must_use]
    pub const fn delay_before_attempt(self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let steps = attempt - 2;
        // Both delays are bounded by MAX_TIMEOUT_MS < 2^27, so any shift at or
        // beyond the ceiling already exceeds the cap and cannot overflow here.
        if steps >= BACKOFF_SHIFT_CEILING {
            return Duration::from_millis(self.max_delay_ms);
        }
        let scaled = self.initial_delay_ms << steps;
        if scaled >= self.max_delay_ms {
            Duration::from_millis(self.max_delay_ms)
        } else {
            Duration::from_millis(scaled)
        }
    }
}

/// Shift count at which any valid initial delay already exceeds any valid cap.
const BACKOFF_SHIFT_CEILING: u32 = 27;

impl Default for McpReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: 500,
            max_delay_ms: 30_000,
            max_attempts: 10,
        }
    }
}

/// User policy applied after tool annotations; annotations never override it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpApprovalMode {
    /// Ask at action time.
    #[default]
    Prompt,
    /// Permit when all higher-level policy also permits.
    Allow,
    /// Deny regardless of server annotations.
    Deny,
}

/// Definition-level tool exposure and approval policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpToolPolicy {
    enabled_tools: Option<BTreeSet<String>>,
    disabled_tools: BTreeSet<String>,
    default_approval: McpApprovalMode,
    per_tool_approval: BTreeMap<String, McpApprovalMode>,
}

impl McpToolPolicy {
    /// Validate deterministic allow/deny/approval rows.
    ///
    /// # Errors
    /// Invalid names, excessive rows or contradictory enabled/disabled rows.
    pub fn new(
        enabled_tools: Option<BTreeSet<String>>,
        disabled_tools: BTreeSet<String>,
        default_approval: McpApprovalMode,
        per_tool_approval: BTreeMap<String, McpApprovalMode>,
    ) -> Result<Self, McpRegistryError> {
        let enabled_len = enabled_tools.as_ref().map_or(0, BTreeSet::len);
        if enabled_len + disabled_tools.len() + per_tool_approval.len() > MAX_TOOL_POLICY_ROWS {
            return Err(McpRegistryError::invalid(
                "tool policy",
                "at most 4096 total rows",
            ));
        }
        if let Some(enabled) = &enabled_tools {
            for name in enabled {
                validate_tool_name(name)?;
                if disabled_tools.contains(name) {
                    return Err(McpRegistryError::invalid(
                        "tool policy",
                        "enabled and disabled sets must not overlap",
                    ));
                }
            }
        }
        for name in &disabled_tools {
            validate_tool_name(name)?;
        }
        for name in per_tool_approval.keys() {
            validate_tool_name(name)?;
            if disabled_tools.contains(name) {
                return Err(McpRegistryError::invalid(
                    "tool policy",
                    "disabled tools cannot carry approval overrides",
                ));
            }
            if enabled_tools
                .as_ref()
                .is_some_and(|enabled| !enabled.contains(name))
            {
                return Err(McpRegistryError::invalid(
                    "tool policy",
                    "approval overrides must belong to the exact allowlist",
                ));
            }
        }
        Ok(Self {
            enabled_tools,
            disabled_tools,
            default_approval,
            per_tool_approval,
        })
    }

    /// Optional exact allowlist; absence means every otherwise-allowed tool.
    #[must_use]
    pub const fn enabled_tools(&self) -> Option<&BTreeSet<String>> {
        self.enabled_tools.as_ref()
    }

    /// Explicit denylist.
    #[must_use]
    pub const fn disabled_tools(&self) -> &BTreeSet<String> {
        &self.disabled_tools
    }

    /// Default approval mode.
    #[must_use]
    pub const fn default_approval(&self) -> McpApprovalMode {
        self.default_approval
    }

    /// Per-tool approval overrides.
    #[must_use]
    pub const fn per_tool_approval(&self) -> &BTreeMap<String, McpApprovalMode> {
        &self.per_tool_approval
    }
}

impl Default for McpToolPolicy {
    fn default() -> Self {
        Self {
            enabled_tools: None,
            disabled_tools: BTreeSet::new(),
            default_approval: McpApprovalMode::Prompt,
            per_tool_approval: BTreeMap::new(),
        }
    }
}

/// Whether non-tool server capabilities may be projected to Consumers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct McpExposurePolicy {
    /// Resources may be listed/read by a registered Consumer.
    pub resources: bool,
    /// Prompts may be listed/retrieved by a registered Consumer.
    pub prompts: bool,
    /// Server instructions may enter the reviewed prompt Consumer.
    pub instructions: bool,
}

/// Exact server definition. Raw arguments/environment intentionally implement
/// neither `Debug` nor serialization; use [`McpServerDefinitionSnapshot`] for
/// UI/JSON diagnostics and this value only inside trusted provider code.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerDefinition {
    id: McpServerId,
    display_name: String,
    scope: McpDefinitionScope,
    transport: McpTransportDefinition,
    enabled: bool,
    required: bool,
    timeouts: McpTimeouts,
    reconnect: McpReconnectPolicy,
    tool_policy: McpToolPolicy,
    exposure: McpExposurePolicy,
}

impl McpServerDefinition {
    /// Validate the stable identity and display metadata of a definition.
    ///
    /// # Errors
    /// Invalid id or display name.
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        scope: McpDefinitionScope,
        transport: McpTransportDefinition,
    ) -> Result<Self, McpRegistryError> {
        let id = McpServerId::new(id)?;
        let display_name = display_name.into();
        validate_one_line(&display_name, "server display name", 1, MAX_DISPLAY_BYTES)?;
        Ok(Self {
            id,
            display_name,
            scope,
            transport,
            enabled: true,
            required: false,
            timeouts: McpTimeouts::default(),
            reconnect: McpReconnectPolicy::default(),
            tool_policy: McpToolPolicy::default(),
            exposure: McpExposurePolicy::default(),
        })
    }

    /// Set whether a connection provider may activate this definition.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Set whether startup failure should block the owning profile.
    #[must_use]
    pub fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Replace all resolved operation budgets.
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: McpTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Replace the resolved reconnect policy.
    #[must_use]
    pub fn with_reconnect(mut self, reconnect: McpReconnectPolicy) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Replace tool exposure/approval policy.
    #[must_use]
    pub fn with_tool_policy(mut self, tool_policy: McpToolPolicy) -> Self {
        self.tool_policy = tool_policy;
        self
    }

    /// Replace non-tool capability exposure policy.
    #[must_use]
    pub fn with_exposure(mut self, exposure: McpExposurePolicy) -> Self {
        self.exposure = exposure;
        self
    }

    /// Stable server id.
    #[must_use]
    pub const fn id(&self) -> &McpServerId {
        &self.id
    }

    /// Human display name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Trusted definition layer.
    #[must_use]
    pub const fn scope(&self) -> McpDefinitionScope {
        self.scope
    }

    /// Exact transport consumed by a transport provider.
    #[must_use]
    pub const fn transport(&self) -> &McpTransportDefinition {
        &self.transport
    }

    /// Whether activation is enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether startup failure blocks the profile.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Explicit budgets.
    #[must_use]
    pub const fn timeouts(&self) -> McpTimeouts {
        self.timeouts
    }

    /// Explicit reconnect policy.
    #[must_use]
    pub const fn reconnect(&self) -> McpReconnectPolicy {
        self.reconnect
    }

    /// Tool allow/deny/approval policy.
    #[must_use]
    pub const fn tool_policy(&self) -> &McpToolPolicy {
        &self.tool_policy
    }

    /// Resource/prompt/instruction exposure policy.
    #[must_use]
    pub const fn exposure(&self) -> McpExposurePolicy {
        self.exposure
    }

    pub(super) fn has_credential_references(&self) -> bool {
        self.transport.has_credential_references()
    }

    pub(super) fn snapshot(&self, definition_revision: u64) -> McpServerDefinitionSnapshot {
        McpServerDefinitionSnapshot {
            definition_revision,
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            scope: self.scope,
            transport: McpTransportSnapshot::from_exact(&self.transport),
            enabled: self.enabled,
            required: self.required,
            timeouts: self.timeouts,
            reconnect: self.reconnect,
            tool_policy: self.tool_policy.clone(),
            exposure: self.exposure,
        }
    }
}

/// Whether one redacted transport value is literal or credential-backed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpValueSourceKind {
    /// Exact bytes exist only in the private definition.
    Literal,
    /// Value is resolved from the visible non-secret reference.
    CredentialReference,
}

/// Redacted argument/environment/header value source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpValueSourceSnapshot {
    kind: McpValueSourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_reference: Option<McpSecretReference>,
}

impl McpValueSourceSnapshot {
    fn literal() -> Self {
        Self {
            kind: McpValueSourceKind::Literal,
            credential_reference: None,
        }
    }

    fn credential(reference: &McpSecretReference) -> Self {
        Self {
            kind: McpValueSourceKind::CredentialReference,
            credential_reference: Some(reference.clone()),
        }
    }

    /// Whether the exact value is literal or credential-backed.
    #[must_use]
    pub const fn kind(&self) -> McpValueSourceKind {
        self.kind
    }

    /// Visible non-secret credential reference, when applicable.
    #[must_use]
    pub const fn credential_reference(&self) -> Option<&McpSecretReference> {
        self.credential_reference.as_ref()
    }
}

/// Redacted named environment/header binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpNamedValueSnapshot {
    name: String,
    source: McpValueSourceSnapshot,
}

impl McpNamedValueSnapshot {
    /// Environment/header name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Redacted value source.
    #[must_use]
    pub const fn source(&self) -> &McpValueSourceSnapshot {
        &self.source
    }
}

/// Public transport projection containing no literal argument/environment/header values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum McpTransportSnapshot {
    /// Stdio command metadata plus redacted value sources.
    Stdio {
        /// Executable name/path.
        command: String,
        /// Absolute working directory.
        cwd: String,
        /// Ordered argument source kinds; literal bytes are absent.
        arguments: Vec<McpValueSourceSnapshot>,
        /// Deterministically sorted environment names/source kinds.
        environment: Vec<McpNamedValueSnapshot>,
    },
    /// Streamable endpoint with reference-only headers.
    StreamableHttp {
        /// Query/fragment/userinfo-free endpoint.
        url: String,
        /// Deterministically sorted header names and credential references.
        headers: Vec<McpNamedValueSnapshot>,
    },
}

impl McpTransportSnapshot {
    fn from_exact(transport: &McpTransportDefinition) -> Self {
        match transport {
            McpTransportDefinition::Stdio(stdio) => Self::Stdio {
                command: stdio.command.clone(),
                cwd: stdio.cwd.to_string_lossy().into_owned(),
                arguments: stdio
                    .arguments
                    .iter()
                    .map(|argument| match argument.credential_reference() {
                        Some(reference) => McpValueSourceSnapshot::credential(reference),
                        None => McpValueSourceSnapshot::literal(),
                    })
                    .collect(),
                environment: stdio
                    .environment
                    .iter()
                    .map(|(name, value)| McpNamedValueSnapshot {
                        name: name.clone(),
                        source: match value.credential_reference() {
                            Some(reference) => McpValueSourceSnapshot::credential(reference),
                            None => McpValueSourceSnapshot::literal(),
                        },
                    })
                    .collect(),
            },
            McpTransportDefinition::StreamableHttp(http) => Self::StreamableHttp {
                url: http.url.clone(),
                headers: http
                    .headers
                    .iter()
                    .map(|(name, reference)| McpNamedValueSnapshot {
                        name: name.clone(),
                        source: McpValueSourceSnapshot::credential(reference),
                    })
                    .collect(),
            },
        }
    }

    /// Closed transport family.
    #[must_use]
    pub const fn kind(&self) -> McpTransportKind {
        match self {
            Self::Stdio { .. } => McpTransportKind::Stdio,
            Self::StreamableHttp { .. } => McpTransportKind::StreamableHttp,
        }
    }
}

/// Safe immutable projection of one exact definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerDefinitionSnapshot {
    definition_revision: u64,
    id: McpServerId,
    display_name: String,
    scope: McpDefinitionScope,
    transport: McpTransportSnapshot,
    enabled: bool,
    required: bool,
    timeouts: McpTimeouts,
    reconnect: McpReconnectPolicy,
    tool_policy: McpToolPolicy,
    exposure: McpExposurePolicy,
}

impl McpServerDefinitionSnapshot {
    /// Registry revision that committed this exact definition owner.
    #[must_use]
    pub const fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    /// Stable server id.
    #[must_use]
    pub const fn id(&self) -> &McpServerId {
        &self.id
    }

    /// Human display name.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Trusted definition layer.
    #[must_use]
    pub const fn scope(&self) -> McpDefinitionScope {
        self.scope
    }

    /// Redacted transport summary.
    #[must_use]
    pub const fn transport(&self) -> &McpTransportSnapshot {
        &self.transport
    }

    /// Whether transport activation is enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether startup failure blocks the profile.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }

    /// Resolved operation budgets.
    #[must_use]
    pub const fn timeouts(&self) -> McpTimeouts {
        self.timeouts
    }

    /// Resolved reconnect policy.
    #[must_use]
    pub const fn reconnect(&self) -> McpReconnectPolicy {
        self.reconnect
    }

    /// Tool exposure/approval policy.
    #[must_use]
    pub const fn tool_policy(&self) -> &McpToolPolicy {
        &self.tool_policy
    }

    /// Non-tool exposure policy.
    #[must_use]
    pub const fn exposure(&self) -> McpExposurePolicy {
        self.exposure
    }
}

/// Safe authentication state independent of connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAuthenticationState {
    /// Definition contains no credential references.
    NotRequired,
    /// References exist but no provider has proved their state.
    Unknown,
    /// User authorization is required before connection.
    Required,
    /// Provider proved usable authorization.
    Connected,
    /// Credential expired.
    Expired,
    /// Provider explicitly requires a new authorization flow.
    ReauthenticationRequired,
}

/// Stable failure classes; no external body/message can enter a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureCode {
    /// Child/HTTP transport is unavailable.
    Transport,
    /// MCP or JSON-RPC protocol contract failed.
    Protocol,
    /// Authorization is absent or rejected.
    Unauthorized,
    /// An explicit operation budget expired.
    TimedOut,
    /// A registry/contribution identity conflict occurred.
    Conflict,
    /// Definition/provider combination is invalid.
    InvalidDefinition,
    /// Bounded reconnect attempts were exhausted.
    ReconnectExhausted,
    /// Fixed internal failure class.
    Internal,
}

/// Whether terminal failure retains the last successful public generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpGenerationRetention {
    /// Keep the last good generation visible with degraded state.
    KeepLastGood,
    /// Remove the generation, for disable/removal/exhausted reconnect.
    Remove,
}

/// Public lifecycle state for one definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpConnectionState {
    /// Definition is explicitly disabled.
    Disabled,
    /// Enabled definition has no active connection provider.
    Inactive,
    /// Provider is establishing/initializing a connection.
    Starting {
        /// Caller-observed Unix timestamp.
        since_ms: u64,
    },
    /// Latest successful generation is active.
    Ready {
        /// Candidate commit timestamp.
        since_ms: u64,
    },
    /// Authorization must complete before readiness.
    AuthenticationRequired {
        /// Caller-observed Unix timestamp.
        since_ms: u64,
    },
    /// Provider is retrying with a bounded attempt budget.
    Reconnecting {
        /// Current one-based attempt.
        attempt: u32,
        /// Configured maximum attempts.
        max_attempts: u32,
        /// Absolute next-attempt timestamp.
        next_retry_at_ms: u64,
    },
    /// Last good generation remains inspectable after a classified failure.
    Degraded {
        /// Stable body-free failure class.
        code: McpFailureCode,
        /// Caller-observed Unix timestamp.
        observed_at_ms: u64,
    },
    /// No active/retained generation remains.
    Failed {
        /// Stable body-free failure class.
        code: McpFailureCode,
        /// Caller-observed Unix timestamp.
        observed_at_ms: u64,
    },
}

/// Capabilities negotiated during MCP initialize.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct McpCapabilitySet {
    /// Server advertised tools.
    pub tools: bool,
    /// Server advertised resources.
    pub resources: bool,
    /// Server advertised prompts.
    pub prompts: bool,
    /// Server advertised logging.
    pub logging: bool,
    /// Server advertised roots negotiation.
    pub roots: bool,
    /// Server advertised elicitation.
    pub elicitation: bool,
    /// Server advertised sampling requests.
    pub sampling: bool,
}

/// Complete discovered contribution counts for one successful generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct McpContributionCounts {
    /// Tool definitions.
    pub tools: u32,
    /// Resource definitions.
    pub resources: u32,
    /// Prompt definitions.
    pub prompts: u32,
}

/// Fully validated successful connection candidate before atomic publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpGenerationCandidate {
    protocol_version: String,
    server_name: String,
    server_version: String,
    capabilities: McpCapabilitySet,
    contributions: McpContributionCounts,
    observed_at_ms: u64,
}

impl McpGenerationCandidate {
    /// Validate negotiated server metadata before publication.
    ///
    /// # Errors
    /// Blank/control-bearing/oversized protocol or identity fields.
    pub fn new(
        protocol_version: impl Into<String>,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
        capabilities: McpCapabilitySet,
        contributions: McpContributionCounts,
        observed_at_ms: u64,
    ) -> Result<Self, McpRegistryError> {
        let protocol_version = protocol_version.into();
        validate_one_line(&protocol_version, "protocol version", 1, 64)?;
        let server_name = server_name.into();
        validate_one_line(&server_name, "negotiated server name", 1, MAX_OPAQUE_BYTES)?;
        let server_version = server_version.into();
        validate_one_line(
            &server_version,
            "negotiated server version",
            1,
            MAX_OPAQUE_BYTES,
        )?;
        if (!capabilities.tools && contributions.tools > 0)
            || (!capabilities.resources && contributions.resources > 0)
            || (!capabilities.prompts && contributions.prompts > 0)
        {
            return Err(McpRegistryError::invalid(
                "generation contributions",
                "nonzero counts require the matching negotiated capability",
            ));
        }
        Ok(Self {
            protocol_version,
            server_name,
            server_version,
            capabilities,
            contributions,
            observed_at_ms,
        })
    }

    pub(super) fn into_generation(
        self,
        number: u64,
        definition_revision: u64,
        provider: McpConnectionProviderId,
    ) -> McpConnectionGeneration {
        McpConnectionGeneration {
            number,
            definition_revision,
            provider,
            protocol_version: self.protocol_version,
            server_name: self.server_name,
            server_version: self.server_version,
            capabilities: self.capabilities,
            contributions: self.contributions,
            committed_at_ms: self.observed_at_ms,
        }
    }

    pub(super) const fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }
}

/// Immutable successful connection generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpConnectionGeneration {
    number: u64,
    definition_revision: u64,
    provider: McpConnectionProviderId,
    protocol_version: String,
    server_name: String,
    server_version: String,
    capabilities: McpCapabilitySet,
    contributions: McpContributionCounts,
    committed_at_ms: u64,
}

impl McpConnectionGeneration {
    /// Monotonic successful-generation number for this definition identity.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Exact definition revision used by the connection.
    #[must_use]
    pub const fn definition_revision(&self) -> u64 {
        self.definition_revision
    }

    /// Transport provider that published this generation.
    #[must_use]
    pub const fn provider(&self) -> &McpConnectionProviderId {
        &self.provider
    }

    /// Negotiated MCP protocol version.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Negotiated server name.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Negotiated server version.
    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    /// Negotiated capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> McpCapabilitySet {
        self.capabilities
    }

    /// Complete contribution counts.
    #[must_use]
    pub const fn contributions(&self) -> McpContributionCounts {
        self.contributions
    }

    /// Candidate observation/commit timestamp.
    #[must_use]
    pub const fn committed_at_ms(&self) -> u64 {
        self.committed_at_ms
    }
}

/// Safe immutable server row in one registry snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerSnapshot {
    pub(super) definition: McpServerDefinitionSnapshot,
    pub(super) state: McpConnectionState,
    pub(super) authentication: McpAuthenticationState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) connection_provider: Option<McpConnectionProviderId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) last_good_generation: Option<McpConnectionGeneration>,
}

impl McpServerSnapshot {
    /// Redacted exact definition metadata.
    #[must_use]
    pub const fn definition(&self) -> &McpServerDefinitionSnapshot {
        &self.definition
    }

    /// Current lifecycle state.
    #[must_use]
    pub const fn state(&self) -> &McpConnectionState {
        &self.state
    }

    /// Current safe authentication state.
    #[must_use]
    pub const fn authentication(&self) -> McpAuthenticationState {
        self.authentication
    }

    /// Active connection-provider id, when registered.
    #[must_use]
    pub const fn connection_provider(&self) -> Option<&McpConnectionProviderId> {
        self.connection_provider.as_ref()
    }

    /// Last complete successful generation. State determines whether it is
    /// ready, reconnecting or retained only for degraded diagnostics.
    #[must_use]
    pub fn last_good_generation(&self) -> Option<&McpConnectionGeneration> {
        self.last_good_generation.as_ref()
    }
}

/// Immutable, deterministic whole-registry snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpSnapshot {
    pub(super) schema_version: u32,
    pub(super) active: bool,
    pub(super) revision: u64,
    pub(super) servers: Vec<McpServerSnapshot>,
}

impl McpSnapshot {
    /// Stable serialized diagnostic schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Whether the owning registry plugin is still active.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    /// Monotonic registry commit revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Server rows sorted by stable id.
    #[must_use]
    pub fn servers(&self) -> &[McpServerSnapshot] {
        &self.servers
    }
}

fn validate_one_line(
    value: &str,
    field: &'static str,
    min_bytes: usize,
    max_bytes: usize,
) -> Result<(), McpRegistryError> {
    if !(min_bytes..=max_bytes).contains(&value.len())
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(McpRegistryError::invalid(
            field,
            "bounded trimmed control-free one-line text",
        ));
    }
    Ok(())
}

fn validate_literal(value: &str, field: &'static str) -> Result<(), McpRegistryError> {
    if value.len() > MAX_LITERAL_BYTES || value.contains('\0') {
        return Err(McpRegistryError::invalid(
            field,
            "at most 16384 bytes without NUL",
        ));
    }
    Ok(())
}

fn validate_absolute_path(path: &Path, field: &'static str) -> Result<(), McpRegistryError> {
    let Some(text) = path.to_str() else {
        return Err(McpRegistryError::invalid(
            field,
            "absolute UTF-8 path no longer than 4096 bytes",
        ));
    };
    if !path.is_absolute()
        || text.is_empty()
        || text.len() > MAX_PATH_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(McpRegistryError::invalid(
            field,
            "absolute UTF-8 path no longer than 4096 bytes",
        ));
    }
    Ok(())
}

fn validate_environment_name(name: &str) -> Result<(), McpRegistryError> {
    let bytes = name.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    if !valid {
        return Err(McpRegistryError::invalid(
            "environment name",
            "portable ASCII environment identifier",
        ));
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<(), McpRegistryError> {
    let valid = (1..=128).contains(&name.len())
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        });
    if !valid {
        return Err(McpRegistryError::invalid(
            "HTTP header name",
            "1..=128 RFC token bytes",
        ));
    }
    Ok(())
}

fn validate_http_url(url: &str) -> Result<(), McpRegistryError> {
    if url.is_empty()
        || url.len() > MAX_URL_BYTES
        || url.trim() != url
        || url.chars().any(char::is_control)
        || url.bytes().any(|byte| byte.is_ascii_whitespace())
        || url.contains('?')
        || url.contains('#')
    {
        return Err(McpRegistryError::invalid(
            "Streamable HTTP URL",
            "http(s) URL without userinfo, query, fragment or controls",
        ));
    }
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .and_then(|tail| tail.split('/').next());
    if authority.is_none_or(invalid_http_authority) {
        return Err(McpRegistryError::invalid(
            "Streamable HTTP URL",
            "http(s) URL without userinfo, query, fragment or controls",
        ));
    }
    Ok(())
}

fn invalid_http_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.contains(['@', '%']) {
        return true;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((address, suffix)) = rest.split_once(']') else {
            return true;
        };
        return address.parse::<std::net::Ipv6Addr>().is_err()
            || !(suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_http_port));
    }
    let mut parts = authority.split(':');
    let host = parts.next().unwrap_or_default();
    let port = parts.next();
    parts.next().is_some()
        || !valid_http_host(host)
        || port.is_some_and(|value| !valid_http_port(value))
}

fn valid_http_host(host: &str) -> bool {
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn valid_http_port(port: &str) -> bool {
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
}

fn validate_tool_name(name: &str) -> Result<(), McpRegistryError> {
    validate_one_line(name, "tool policy name", 1, MAX_OPAQUE_BYTES)
}
