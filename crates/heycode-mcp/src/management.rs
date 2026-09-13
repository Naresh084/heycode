//! MCP10 server management: the seven operations, defined once.
//!
//! The row asks for CLI/TUI **parity**, and parity maintained by hand is parity
//! that drifts. So the operation set is a closed enum that both surfaces match
//! exhaustively, and every operation is implemented exactly once here. Adding an
//! eighth operation then fails to compile in every surface that has not handled
//! it, which is the only kind of parity that stays true.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{McpDefinitionScope, McpTransportKind};

/// Settings-backed MCP management service published by `mcp-management`.
pub const SERVICE_MCP_MANAGEMENT: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("mcp-management");

/// Every MCP management operation heycode exposes.
///
/// Deliberately **not** `#[non_exhaustive]`. Most public enums in this codebase
/// are, so that adding a variant is not a breaking change — here the opposite is
/// wanted: adding a variant *must* break every surface until it is handled. That
/// is the mechanism the acceptance criterion asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpOperation {
    /// Register a new server definition.
    Add,
    /// Show every definition with its live state.
    List,
    /// Report the authorization state of one server.
    ///
    /// Inspect only. *Beginning* an authorization needs a browser and a local
    /// callback listener, which MCP04 supports but no surface owns yet; a
    /// closed operation set must not name a capability the layer behind it
    /// does not implement, or every consumer inherits the promise.
    Auth,
    /// Probe one server and report health.
    Test,
    /// Change fields of an existing definition.
    Edit,
    /// Turn one server on or off without removing it.
    Enable,
    /// Delete a definition.
    Remove,
}

impl McpOperation {
    /// Every operation, in the order the acceptance criterion names them.
    ///
    /// A surface can iterate this to prove it handles all of them; the array
    /// length is checked against the variant count by a test, so the list
    /// cannot silently fall behind the enum.
    pub const ALL: [Self; 7] = [
        Self::Add,
        Self::List,
        Self::Auth,
        Self::Test,
        Self::Edit,
        Self::Enable,
        Self::Remove,
    ];

    /// The subcommand word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::List => "list",
            Self::Auth => "auth",
            Self::Test => "test",
            Self::Edit => "edit",
            Self::Enable => "enable",
            Self::Remove => "remove",
        }
    }

    /// Parse a subcommand word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == word)
    }

    /// Whether this operation changes persisted state.
    ///
    /// Managed (administrator-locked) servers refuse exactly these.
    #[must_use]
    pub const fn mutates(self) -> bool {
        match self {
            Self::Add | Self::Edit | Self::Enable | Self::Remove => true,
            Self::List | Self::Auth | Self::Test => false,
        }
    }
}

impl std::fmt::Display for McpOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a management operation could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpManagementError {
    /// The owning plugin/context has shut down.
    Stopped,
    /// No server by that name.
    UnknownServer(String),
    /// A server by that name already exists.
    DuplicateServer(String),
    /// The definition is administrator-managed and cannot be changed here.
    Managed(String),
    /// The definition comes from this session's configuration file, so the
    /// settings store is not where it lives and cannot be where it changes.
    ConfigDeclared(String),
    /// The definition comes from a non-user settings/contribution layer that
    /// this manager cannot write.
    ReadOnlyScope {
        /// Exact visible server name.
        name: String,
        /// Layer that owns the definition.
        scope: McpDefinitionScope,
    },
    /// The name is empty, over-long, or carries control characters.
    InvalidName,
    /// Exactly one transport must be given.
    InvalidTransport(&'static str),
    /// The store could not be read or written; text is provider-supplied and
    /// already redacted by the settings layer.
    Store(String),
}

impl std::fmt::Display for McpManagementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("MCP management service is stopped"),
            Self::UnknownServer(name) => write!(f, "no MCP server named `{name}`"),
            Self::DuplicateServer(name) => write!(f, "an MCP server named `{name}` already exists"),
            Self::Managed(name) => write!(
                f,
                "`{name}` is managed by an administrator and cannot be changed here"
            ),
            Self::ConfigDeclared(name) => write!(
                f,
                "`{name}` is declared in this session's configuration; edit its `[mcp.servers.{name}]` entry instead"
            ),
            Self::ReadOnlyScope { name, scope } => write!(
                f,
                "`{name}` is owned by the {} layer; MCP management can write only user definitions",
                match scope {
                    McpDefinitionScope::User => "user",
                    McpDefinitionScope::Project => "project",
                    McpDefinitionScope::Local => "local workspace",
                    McpDefinitionScope::Managed => "administrator-managed",
                    McpDefinitionScope::Plugin => "plugin",
                }
            ),
            Self::InvalidName => {
                f.write_str("a server name must be 1-64 characters of letters, digits, `-` or `_`")
            }
            Self::InvalidTransport(detail) => write!(f, "invalid transport: {detail}"),
            Self::Store(message) => write!(f, "settings store: {message}"),
        }
    }
}

impl std::error::Error for McpManagementError {}

/// One persisted server definition, as management sees it.
///
/// Deliberately holds no secret material. A stdio server's environment values
/// and an HTTP server's headers are credential *references* resolved at connect
/// time, so `list` can print this whole struct without a redaction pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredServer {
    /// Server name, unique within the store.
    pub name: String,
    /// Which transport this server speaks.
    pub transport: McpTransportKind,
    /// Command for stdio, or URL for Streamable HTTP.
    pub target: String,
    /// Argument vector after a stdio command. Always empty for HTTP.
    pub args: Vec<String>,
    /// Extra environment for a stdio child. Values are literal but must not
    /// look like credentials — `heycode mcp add -e KEY` (no value) inherits the
    /// host's variable at launch instead, so secrets never enter this store.
    pub env: BTreeMap<String, String>,
    /// Whether the server participates in a session.
    pub enabled: bool,
    /// Where the definition came from.
    pub scope: McpDefinitionScope,
    /// Administrator-locked definitions refuse every mutating operation.
    pub managed: bool,
}

impl StoredServer {
    /// Validate and build a definition.
    ///
    /// An HTTP target is validated with the same rules the connection path
    /// applies, so `heycode mcp add --url` refuses exactly what a session would
    /// refuse — at the moment the user types it, not on every later launch.
    ///
    /// # Errors
    /// [`McpManagementError::InvalidName`] for a malformed name, and
    /// [`McpManagementError::InvalidTransport`] for an empty target or an HTTP
    /// target that is not an acceptable Streamable HTTP URL.
    pub fn new(
        name: impl Into<String>,
        transport: McpTransportKind,
        target: impl Into<String>,
    ) -> Result<Self, McpManagementError> {
        let name = name.into();
        let target = target.into();
        if !valid_name(&name) {
            return Err(McpManagementError::InvalidName);
        }
        if target.trim().is_empty() || target.chars().any(char::is_control) {
            return Err(McpManagementError::InvalidTransport(
                "a command or URL is required and must not contain control characters",
            ));
        }
        if transport == McpTransportKind::StreamableHttp
            && crate::McpStreamableHttpTransport::new(&target, BTreeMap::new()).is_err()
        {
            return Err(McpManagementError::InvalidTransport(
                "an http(s) URL without userinfo, query, fragment or control characters is required",
            ));
        }
        Ok(Self {
            name,
            transport,
            target,
            args: Vec::new(),
            env: BTreeMap::new(),
            enabled: true,
            scope: McpDefinitionScope::User,
            managed: false,
        })
    }

    /// Attach stdio arguments and environment.
    ///
    /// # Errors
    /// [`McpManagementError::InvalidTransport`] when the transport is not
    /// stdio, an argument or key carries control characters, a key is not a
    /// valid environment name, or a value looks like a credential.
    pub fn with_stdio_launch(
        mut self,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Result<Self, McpManagementError> {
        if self.transport != McpTransportKind::Stdio && (!args.is_empty() || !env.is_empty()) {
            return Err(McpManagementError::InvalidTransport(
                "arguments and environment apply only to a stdio command",
            ));
        }
        if args.iter().any(|arg| arg.chars().any(char::is_control)) {
            return Err(McpManagementError::InvalidTransport(
                "arguments must not contain control characters",
            ));
        }
        for (key, value) in &env {
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(McpManagementError::InvalidTransport(
                    "environment keys must be non-empty and [A-Za-z0-9_]",
                ));
            }
            if value.chars().any(char::is_control) {
                return Err(McpManagementError::InvalidTransport(
                    "environment values must not contain control characters",
                ));
            }
            if looks_like_credential(value) {
                return Err(McpManagementError::InvalidTransport(
                    "environment values that look like credentials are refused; use `-e KEY` to inherit the host variable instead",
                ));
            }
        }
        self.args = args;
        self.env = env;
        Ok(self)
    }
}

/// Conservative screen for values that must never be persisted in this store.
fn looks_like_credential(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with("sk-")
        || trimmed.starts_with("AKIA")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("xox")
        || trimmed.starts_with("-----BEGIN")
        || (trimmed.matches('.').count() == 2 && trimmed.starts_with("eyJ"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One fallible mutation applied to a store-owned definition map.
pub type McpDefinitionMutation<'a> =
    dyn FnMut(&mut BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> + 'a;

/// Durable storage boundary for MCP definitions.
///
/// A trait so the operations are testable without a settings stack, and so the
/// CLI and a TUI panel are provably running the same code against the same
/// store rather than two similar implementations.
pub trait McpDefinitionStore: Send + Sync {
    /// Read every definition.
    ///
    /// # Errors
    /// Redacted, actionable store text only.
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError>;

    /// Replace the whole set.
    ///
    /// Whole-set rather than per-key because the settings layer persists a
    /// complete user section; a partial write would drop the servers it did not
    /// mention.
    ///
    /// # Errors
    /// Redacted, actionable store text only.
    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError>;

    /// Apply one pure mutation to the exact store generation it was read from.
    ///
    /// The default preserves compatibility for simple in-memory/custom stores.
    /// Production layered settings overrides this so a concurrent settings
    /// publication fails stale before any write instead of being overwritten.
    ///
    /// # Errors
    /// Store read/write failures or a refusal returned by `mutation`.
    fn update(&self, mutation: &mut McpDefinitionMutation<'_>) -> Result<(), McpManagementError> {
        let mut servers = self.load()?;
        mutation(&mut servers)?;
        self.persist(&servers)
    }
}

/// What a probe concluded about one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpHealth {
    /// The server answered `initialize`.
    Reachable,
    /// The server refused or could not be reached.
    Unreachable,
    /// The server requires authorization first.
    AuthorizationRequired,
    /// Not probed, or the probe could not conclude. Never read as reachable.
    Unknown,
}

/// Most servers probed at once. A handful of connections is the point; a
/// thousand threads is not.
const MAX_CONCURRENT_PROBES: usize = 8;

/// Probes a live server. Injected so `test` is deterministic under test.
pub trait McpProbe: Send + Sync {
    /// Probe one definition.
    fn probe(&self, server: &StoredServer) -> McpHealth;

    /// Probe many definitions, returning one health per input in input order.
    ///
    /// Probing is a network/spawn wait, so waits overlap: eight servers cost
    /// about one probe, not eight. Implementations that must serialize
    /// override this.
    fn probe_all(&self, servers: &[StoredServer]) -> Vec<McpHealth> {
        let mut healths = Vec::with_capacity(servers.len());
        for chunk in servers.chunks(MAX_CONCURRENT_PROBES) {
            std::thread::scope(|scope| {
                let handles = chunk
                    .iter()
                    .map(|server| scope.spawn(move || self.probe(server)))
                    .collect::<Vec<_>>();
                for handle in handles {
                    healths.push(handle.join().unwrap_or(McpHealth::Unknown));
                }
            });
        }
        healths
    }
}

/// One row of `list` output: the definition plus what is known about it live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStatusRow {
    /// The persisted definition.
    pub server: StoredServer,
    /// Live health, `Unknown` unless a probe ran.
    pub health: McpHealth,
}

/// The seven operations, implemented once for every surface.
///
/// Two sources feed one surface. The **store** is what `add`/`edit`/`remove`
/// persist; the **configuration overlay** is what the running session's
/// `[mcp.servers]` section declared, adopted at composition by the connection
/// provider. Listing them together is the whole point: before they were
/// joined, `add` reported success for a definition nothing ever ran, and the
/// servers a session was actually running could not be seen at all.
pub struct McpManagement {
    store: Arc<dyn McpDefinitionStore>,
    probe: Option<Arc<dyn McpProbe>>,
    active: Arc<AtomicBool>,
    /// Read-only rows contributed by the session's configuration file.
    configured: Arc<std::sync::Mutex<BTreeMap<String, StoredServer>>>,
}

impl std::fmt::Debug for McpManagement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpManagement")
            .field("probe_installed", &self.probe.is_some())
            .finish_non_exhaustive()
    }
}

impl McpManagement {
    /// Management over `store`, with no live probing.
    #[must_use]
    pub fn new(store: Arc<dyn McpDefinitionStore>) -> Self {
        Self {
            store,
            probe: None,
            active: Arc::new(AtomicBool::new(true)),
            configured: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }

    /// Install a probe so `test` and `list` can report live health.
    #[must_use]
    pub fn with_probe(mut self, probe: Arc<dyn McpProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    fn ensure_active(&self) -> Result<(), McpManagementError> {
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(McpManagementError::Stopped)
        }
    }

    fn lifecycle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }

    fn locate(
        servers: &BTreeMap<String, StoredServer>,
        name: &str,
    ) -> Result<StoredServer, McpManagementError> {
        servers
            .get(name)
            .cloned()
            .ok_or_else(|| McpManagementError::UnknownServer(name.to_owned()))
    }

    /// Refuse a mutating operation on any definition this user-layer manager
    /// does not own.
    fn check_mutable(server: &StoredServer) -> Result<(), McpManagementError> {
        if server.managed || server.scope == McpDefinitionScope::Managed {
            return Err(McpManagementError::Managed(server.name.clone()));
        }
        if server.scope == McpDefinitionScope::User {
            Ok(())
        } else {
            Err(McpManagementError::ReadOnlyScope {
                name: server.name.clone(),
                scope: server.scope,
            })
        }
    }

    /// Refuse a mutating operation on a row the session's configuration file
    /// declared: that file is the user's, and this surface does not edit it.
    fn check_not_configured(&self, name: &str) -> Result<(), McpManagementError> {
        let configured = self
            .configured
            .lock()
            .map_err(|_| McpManagementError::Store("configured rows unavailable".to_owned()))?;
        if configured.contains_key(name) {
            return Err(McpManagementError::ConfigDeclared(name.to_owned()));
        }
        Ok(())
    }

    /// Every row a surface should show: stored definitions with the session's
    /// configured rows layered over them.
    ///
    /// A configured name wins a collision, because its transport is exact
    /// where a stored row is a bare command or URL.
    fn visible(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        let mut servers = self.store.load()?;
        let configured = self
            .configured
            .lock()
            .map_err(|_| McpManagementError::Store("configured rows unavailable".to_owned()))?;
        for (name, server) in configured.iter() {
            servers.insert(name.clone(), server.clone());
        }
        Ok(servers)
    }

    /// Replace the read-only rows this session's configuration file declared.
    ///
    /// Called once by the connection provider at composition and again with an
    /// empty map at shutdown, so a surface never reports a session's servers
    /// after that session is gone.
    ///
    /// # Errors
    /// A poisoned overlay lock.
    pub fn adopt_configured_servers(
        &self,
        servers: BTreeMap<String, StoredServer>,
    ) -> Result<(), McpManagementError> {
        let mut configured = self
            .configured
            .lock()
            .map_err(|_| McpManagementError::Store("configured rows unavailable".to_owned()))?;
        *configured = servers;
        Ok(())
    }

    /// The stored definitions this session should connect: enabled rows whose
    /// name the configuration file does not already own.
    ///
    /// # Errors
    /// Store read failure or a poisoned overlay lock.
    pub fn connectable(&self) -> Result<Vec<StoredServer>, McpManagementError> {
        self.ensure_active()?;
        let stored = self.store.load()?;
        let configured = self
            .configured
            .lock()
            .map_err(|_| McpManagementError::Store("configured rows unavailable".to_owned()))?;
        Ok(stored
            .into_values()
            .filter(|server| server.enabled && !configured.contains_key(&server.name))
            .collect())
    }

    /// `add`: register a new definition.
    ///
    /// # Errors
    /// [`McpManagementError::DuplicateServer`] when the name is taken, or
    /// [`McpManagementError::ConfigDeclared`] when the configuration file
    /// already owns it. Adding is never a silent overwrite — `edit` is the
    /// operation that changes a server.
    pub fn add(&self, server: StoredServer) -> Result<(), McpManagementError> {
        self.ensure_active()?;
        self.check_not_configured(&server.name)?;
        Self::check_mutable(&server)?;
        self.store.update(&mut |servers| {
            if let Some(existing) = servers.get(&server.name) {
                Self::check_mutable(existing)?;
                return Err(McpManagementError::DuplicateServer(server.name.clone()));
            }
            servers.insert(server.name.clone(), server.clone());
            Ok(())
        })
    }

    /// `list`: every definition, as stored (plus the session's configured
    /// rows). Health is `Unknown`.
    ///
    /// Listing is a store read and must stay one: a surface that opens a
    /// panel, or a shell completing a name, cannot afford to start every
    /// configured server. [`Self::list_probed`] is the version that asks.
    ///
    /// # Errors
    /// Store read failure.
    pub fn list(&self) -> Result<Vec<McpStatusRow>, McpManagementError> {
        self.ensure_active()?;
        Ok(self
            .visible()?
            .into_values()
            .map(|server| McpStatusRow {
                server,
                health: McpHealth::Unknown,
            })
            .collect())
    }

    /// `list` plus one live probe per definition, run concurrently.
    ///
    /// Without a probe installed every row is `Unknown` — "I did not check"
    /// is not "it is down".
    ///
    /// # Errors
    /// Store read failure.
    pub fn list_probed(&self) -> Result<Vec<McpStatusRow>, McpManagementError> {
        self.ensure_active()?;
        let servers = self.visible()?.into_values().collect::<Vec<_>>();
        let healths = self.probe.as_ref().map_or_else(
            || vec![McpHealth::Unknown; servers.len()],
            |probe| probe.probe_all(&servers),
        );
        Ok(servers
            .into_iter()
            .zip(healths)
            .map(|(server, health)| McpStatusRow { server, health })
            .collect())
    }

    /// `auth`: report what authorization state one server is in.
    ///
    /// Returns the health so a caller can distinguish "needs authorization"
    /// from "unreachable" — different problems with different next steps.
    /// This does not *start* a flow; see [`McpOperation::Auth`].
    ///
    /// # Errors
    /// [`McpManagementError::UnknownServer`].
    pub fn auth(&self, name: &str) -> Result<McpHealth, McpManagementError> {
        self.ensure_active()?;
        let server = Self::locate(&self.visible()?, name)?;
        Ok(self
            .probe
            .as_ref()
            .map_or(McpHealth::Unknown, |probe| probe.probe(&server)))
    }

    /// `test`: probe one server.
    ///
    /// Without a probe installed the answer is `Unknown`, never `Unreachable` —
    /// "I did not check" and "I checked and it is down" are different facts.
    ///
    /// # Errors
    /// [`McpManagementError::UnknownServer`].
    pub fn test(&self, name: &str) -> Result<McpHealth, McpManagementError> {
        self.ensure_active()?;
        let server = Self::locate(&self.visible()?, name)?;
        Ok(self
            .probe
            .as_ref()
            .map_or(McpHealth::Unknown, |probe| probe.probe(&server)))
    }

    /// `edit`: change the target of an existing definition.
    ///
    /// # Errors
    /// [`McpManagementError::UnknownServer`], [`McpManagementError::Managed`],
    /// or an invalid target.
    pub fn edit(
        &self,
        name: &str,
        transport: McpTransportKind,
        target: &str,
    ) -> Result<(), McpManagementError> {
        self.ensure_active()?;
        self.check_not_configured(name)?;
        self.store.update(&mut |servers| {
            let existing = Self::locate(servers, name)?;
            Self::check_mutable(&existing)?;
            let mut updated = StoredServer::new(name, transport, target)?;
            // Editing a target must not silently re-enable a disabled server
            // or move it between scopes: only what was asked for changes.
            updated.enabled = existing.enabled;
            updated.scope = existing.scope;
            updated.managed = existing.managed;
            servers.insert(name.to_owned(), updated);
            Ok(())
        })
    }

    /// `enable`: turn one server on or off, keeping its definition.
    ///
    /// # Errors
    /// [`McpManagementError::UnknownServer`] or [`McpManagementError::Managed`].
    pub fn enable(&self, name: &str, enabled: bool) -> Result<(), McpManagementError> {
        self.ensure_active()?;
        self.check_not_configured(name)?;
        self.store.update(&mut |servers| {
            let mut server = Self::locate(servers, name)?;
            Self::check_mutable(&server)?;
            server.enabled = enabled;
            servers.insert(name.to_owned(), server);
            Ok(())
        })
    }

    /// `remove`: delete a definition.
    ///
    /// # Errors
    /// [`McpManagementError::UnknownServer`] — removing something that is not
    /// there is reported, not silently succeeded, because a user who misspells
    /// a name must not believe they removed a server that is still running.
    pub fn remove(&self, name: &str) -> Result<(), McpManagementError> {
        self.ensure_active()?;
        self.check_not_configured(name)?;
        self.store.update(&mut |servers| {
            let server = Self::locate(servers, name)?;
            Self::check_mutable(&server)?;
            servers.remove(name);
            Ok(())
        })
    }
}

/// MCP server-definition settings namespace.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace()
-> Result<heycode_settings::SettingsNamespace, heycode_settings::SettingsError> {
    heycode_settings::SettingsNamespace::new("mcp-servers")
}

/// The settings definition management persists into.
///
/// # Errors
/// Static schema validation failure.
pub fn settings_definition()
-> Result<heycode_settings::SettingsDefinition, heycode_settings::SettingsError> {
    let namespace = settings_namespace()?;
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "properties": {
                "servers": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "transport": {"type": "string", "enum": ["stdio", "streamable-http"]},
                            "target": {"type": "string"},
                            "args": {"type": "array", "items": {"type": "string"}},
                            "env": {"type": "object", "additionalProperties": {"type": "string"}},
                            "enabled": {"type": "boolean"}
                        }
                    }
                }
            }
        }),
        serde_json::json!({"servers": {}}),
        validate_servers_section,
    )?
    // Safe to project on the wire: a definition holds a command or URL and a
    // flag. Environment values and headers are credential *references* resolved
    // at connect time, so nothing secret lives in this namespace by design.
    .with_wire_exposure();
    Ok(heycode_settings::SettingsDefinition::new(namespace, schema))
}

fn validate_servers_section(value: &serde_json::Value) -> Result<(), String> {
    let servers = value
        .get("servers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "servers must be an object".to_owned())?;
    for (name, entry) in servers {
        if !valid_name(name) {
            return Err(format!("`{name}` is not a valid server name"));
        }
        let entry = entry
            .as_object()
            .ok_or_else(|| format!("server `{name}` must be an object"))?;
        let transport = entry
            .get("transport")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("server `{name}` needs a transport"))?;
        if !matches!(transport, "stdio" | "streamable-http") {
            return Err(format!(
                "server `{name}` has unknown transport `{transport}`"
            ));
        }
        let target = entry
            .get("target")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("server `{name}` needs a target"))?;
        if target.trim().is_empty() {
            return Err(format!("server `{name}` has an empty target"));
        }
    }
    Ok(())
}

const fn transport_wire(kind: McpTransportKind) -> &'static str {
    match kind {
        McpTransportKind::Stdio => "stdio",
        McpTransportKind::StreamableHttp => "streamable-http",
    }
}

/// Durable store backed by the layered settings stack.
///
/// Definitions live in the user layer of `mcp-servers`. A definition arriving
/// from the **managed** layer is marked `managed` and refuses every mutating
/// operation — that is how an administrator pins a server, and it is enforced
/// here rather than trusted to a caller.
pub struct SettingsBackedStore {
    settings: std::sync::Arc<heycode_settings::SettingsService>,
}

impl std::fmt::Debug for SettingsBackedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SettingsBackedStore")
    }
}

impl SettingsBackedStore {
    /// Bind to a settings service.
    #[must_use]
    pub const fn new(settings: std::sync::Arc<heycode_settings::SettingsService>) -> Self {
        Self { settings }
    }

    fn parse_layer(
        section: Option<&serde_json::Value>,
        scope: McpDefinitionScope,
        managed: bool,
        into: &mut BTreeMap<String, StoredServer>,
    ) {
        let Some(servers) = section
            .and_then(|section| section.get("servers"))
            .and_then(serde_json::Value::as_object)
        else {
            return;
        };
        for (name, entry) in servers {
            let transport = match entry.get("transport").and_then(serde_json::Value::as_str) {
                Some("stdio") => McpTransportKind::Stdio,
                Some("streamable-http") => McpTransportKind::StreamableHttp,
                // An unreadable entry is skipped rather than failing the whole
                // load: one malformed row must not hide every other server.
                _ => continue,
            };
            let Some(target) = entry.get("target").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Ok(server) = StoredServer::new(name, transport, target) else {
                continue;
            };
            let args = entry
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let env = entry
                .get("env")
                .and_then(serde_json::Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            let Ok(mut server) = server.with_stdio_launch(args, env) else {
                continue;
            };
            server.enabled = entry
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            server.scope = scope;
            server.managed = managed;
            into.insert(name.clone(), server);
        }
    }

    fn parse_snapshot(
        snapshot: &heycode_settings::SettingsSnapshot,
    ) -> BTreeMap<String, StoredServer> {
        let mut servers = BTreeMap::new();
        // Lowest precedence first; managed is applied last so an administrator
        // definition wins and carries its lock with it.
        Self::parse_layer(
            snapshot.user(),
            McpDefinitionScope::User,
            false,
            &mut servers,
        );
        Self::parse_layer(
            snapshot.project(),
            McpDefinitionScope::Project,
            false,
            &mut servers,
        );
        Self::parse_layer(
            snapshot.managed(),
            McpDefinitionScope::Managed,
            true,
            &mut servers,
        );
        servers
    }

    fn server_value(server: &StoredServer) -> serde_json::Value {
        let mut row = serde_json::json!({
            "transport": transport_wire(server.transport),
            "target": server.target,
            "enabled": server.enabled,
        });
        if !server.args.is_empty() {
            row["args"] = serde_json::json!(server.args);
        }
        if !server.env.is_empty() {
            row["env"] = serde_json::json!(server.env);
        }
        row
    }

    /// Derive one user-layer candidate without copying a higher-precedence
    /// definition down. A project/managed row can shadow an existing user row
    /// of the same name; that raw user row remains byte-for-byte equivalent in
    /// the candidate during unrelated user mutations.
    fn user_candidate(
        snapshot: &heycode_settings::SettingsSnapshot,
        servers: &BTreeMap<String, StoredServer>,
    ) -> serde_json::Value {
        let mut user = snapshot
            .user()
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let current = user
            .get("servers")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut next = serde_json::Map::new();

        // Absence from the mutated effective map deletes an owned user row.
        // A higher layer at the same name instead preserves the hidden raw
        // user value: serializing the visible Project/Managed row would either
        // promote it or erase the user's original provenance.
        for (name, raw) in current {
            match servers.get(&name) {
                None => {}
                Some(server) if server.scope == McpDefinitionScope::User && !server.managed => {
                    next.insert(name, Self::server_value(server));
                }
                Some(_) => {
                    next.insert(name, raw);
                }
            }
        }
        for (name, server) in servers {
            if server.scope == McpDefinitionScope::User && !server.managed {
                next.insert(name.clone(), Self::server_value(server));
            }
        }
        user.insert("servers".to_owned(), serde_json::Value::Object(next));
        serde_json::Value::Object(user)
    }

    fn persist_against(
        &self,
        namespace: &heycode_settings::SettingsNamespace,
        snapshot: &Arc<heycode_settings::SettingsSnapshot>,
        servers: &BTreeMap<String, StoredServer>,
    ) -> Result<(), McpManagementError> {
        let section = Self::user_candidate(snapshot, servers);
        self.settings
            .replace_user_automatically(namespace, section, snapshot, |_| Ok(()))
            .map(|_| ())
            .map_err(|error| McpManagementError::Store(error.to_string()))
    }
}

impl McpDefinitionStore for SettingsBackedStore {
    fn load(&self) -> Result<BTreeMap<String, StoredServer>, McpManagementError> {
        let namespace =
            settings_namespace().map_err(|error| McpManagementError::Store(error.to_string()))?;
        let Some(snapshot) = self
            .settings
            .get(&namespace)
            .map_err(|error| McpManagementError::Store(error.to_string()))?
        else {
            return Ok(BTreeMap::new());
        };
        Ok(Self::parse_snapshot(&snapshot))
    }

    fn persist(&self, servers: &BTreeMap<String, StoredServer>) -> Result<(), McpManagementError> {
        let namespace =
            settings_namespace().map_err(|error| McpManagementError::Store(error.to_string()))?;
        let snapshot = self
            .settings
            .get(&namespace)
            .map_err(|error| McpManagementError::Store(error.to_string()))?
            .ok_or_else(|| {
                McpManagementError::Store("MCP settings namespace is not registered".to_owned())
            })?;
        self.persist_against(&namespace, &snapshot, servers)
    }

    fn update(&self, mutation: &mut McpDefinitionMutation<'_>) -> Result<(), McpManagementError> {
        let namespace =
            settings_namespace().map_err(|error| McpManagementError::Store(error.to_string()))?;
        let snapshot = self
            .settings
            .get(&namespace)
            .map_err(|error| McpManagementError::Store(error.to_string()))?
            .ok_or_else(|| {
                McpManagementError::Store("MCP settings namespace is not registered".to_owned())
            })?;
        let mut servers = Self::parse_snapshot(&snapshot);
        mutation(&mut servers)?;
        self.persist_against(&namespace, &snapshot, &servers)
    }
}

/// Register the MCP server-definition settings namespace.
///
/// A separate plugin from `mcp-registry` on purpose: the base registry must not
/// grow a settings dependency, and a world that never manages servers should
/// not pay for the namespace.
#[must_use]
pub fn mcp_management_plugin() -> Box<dyn heycode_core::Plugin> {
    mcp_management_plugin_with_probe(None)
}

/// [`mcp_management_plugin`] with the probe `test`, `auth` and the explicit
/// refresh use.
///
/// The composition root chooses: a shell that can ask the user to probe wants
/// [`LiveMcpProbe`]; a world that must never spawn passes `None` and every
/// health reads `unknown` rather than a guess.
#[must_use]
pub fn mcp_management_plugin_with_probe(
    probe: Option<Arc<dyn McpProbe>>,
) -> Box<dyn heycode_core::Plugin> {
    struct McpManagementPlugin(Option<Arc<dyn McpProbe>>);

    impl heycode_core::Plugin for McpManagementPlugin {
        fn name(&self) -> &'static str {
            "mcp-management"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MCP_MANAGEMENT]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "mcp-servers",
            )]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| heycode_core::CoreError::other("settings missing"))?;
            settings
                .register(
                    context,
                    settings_definition()
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let mut management = McpManagement::new(
                Arc::new(SettingsBackedStore::new(settings)) as Arc<dyn McpDefinitionStore>
            );
            if let Some(probe) = self.0.clone() {
                management = management.with_probe(probe);
            }
            let active = management.lifecycle();
            context.effect(move || {
                active.store(false, Ordering::Release);
            });
            context.provide(SERVICE_MCP_MANAGEMENT, self.name(), management)
        }
    }

    Box::new(McpManagementPlugin(probe))
}

/// The production probe: actually run `initialize` against the definition.
///
/// `heycode mcp test` used to answer `unknown` for every server because nothing
/// installed a probe — a health command that cannot fail is not a health
/// command. A stdio definition is spawned exactly as a session would spawn
/// it and dropped again (which kills the child); an HTTP definition gets one
/// `initialize`. Both are bounded by `timeout`, so a server that never
/// answers is `Unreachable` in bounded time rather than a hang.
#[derive(Debug, Clone)]
pub struct LiveMcpProbe {
    timeout: std::time::Duration,
}

impl Default for LiveMcpProbe {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(10),
        }
    }
}

impl LiveMcpProbe {
    /// Probe with an explicit overall budget per server.
    #[must_use]
    pub const fn with_timeout(timeout: std::time::Duration) -> Self {
        Self { timeout }
    }

    /// The health, plus the live stdio connection when one was made: it owns
    /// a driver runtime and must be dropped on a plain thread, not inside
    /// `block_on`.
    async fn probe_async(server: &StoredServer) -> (McpHealth, Option<crate::McpConnection>) {
        match server.transport {
            McpTransportKind::Stdio => {
                let config = crate::McpServerConfig {
                    command: server.target.clone(),
                    args: server.args.clone(),
                    env: server
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    required: false,
                };
                match crate::McpConnection::spawn(&server.name, &config).await {
                    Ok(connection) => (McpHealth::Reachable, Some(connection)),
                    Err(_) => (McpHealth::Unreachable, None),
                }
            }
            McpTransportKind::StreamableHttp => {
                let Ok(definition) =
                    crate::McpStreamableHttpTransport::new(server.target.clone(), BTreeMap::new())
                else {
                    return (McpHealth::Unreachable, None);
                };
                let Ok(transport) = heycode_http::ReqwestHttpTransport::new() else {
                    return (McpHealth::Unreachable, None);
                };
                let http = heycode_http::HttpService::new(Arc::new(transport));
                let Ok(client) = crate::McpStreamableHttpClient::new(
                    http,
                    &definition,
                    crate::McpNotificationRouter::new(),
                    crate::McpTimeouts::default(),
                ) else {
                    return (McpHealth::Unreachable, None);
                };
                let cancellation = tokio_util::sync::CancellationToken::new();
                let health = match client.initialize(&cancellation).await {
                    Ok(_) => {
                        let _ = client.terminate(&cancellation).await;
                        McpHealth::Reachable
                    }
                    Err(
                        crate::McpHttpError::Unauthorized
                        | crate::McpHttpError::Status { status: 401 | 403 },
                    ) => McpHealth::AuthorizationRequired,
                    Err(_) => McpHealth::Unreachable,
                };
                (health, None)
            }
        }
    }
}

impl McpProbe for LiveMcpProbe {
    fn probe(&self, server: &StoredServer) -> McpHealth {
        // The trait is synchronous and may be called from inside another
        // runtime (the TUI), so the probe owns a thread and a runtime of its
        // own; the join makes the whole thing a bounded blocking call.
        let server = server.clone();
        let timeout = self.timeout;
        std::thread::Builder::new()
            .name("mcp-probe".to_owned())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return McpHealth::Unknown;
                };
                let (health, connection) = runtime.block_on(async {
                    tokio::time::timeout(timeout, Self::probe_async(&server))
                        .await
                        .unwrap_or((McpHealth::Unreachable, None))
                });
                // Off the async context: the connection's driver runtime and
                // the probe runtime both shut down on this plain thread.
                drop(connection);
                drop(runtime);
                health
            })
            .map_or(McpHealth::Unknown, |handle| {
                handle.join().unwrap_or(McpHealth::Unknown)
            })
    }
}
