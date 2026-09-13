//! Strict, value-minimizing previews of competitor configuration metadata.
//!
//! The boundary accepts already-read text and performs no filesystem writes.
//! Known non-secret metadata is projected into types that can build a detached
//! [`crate::Config`] candidate. Credential-bearing fields and executable
//! extensions have no value slot in the public preview; unknown fields expose
//! only their path and structural kind.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

use crate::{Config, McpServerCfg};

const MAX_IMPORT_BYTES: usize = 1024 * 1024;
const MAX_IMPORT_FIELDS: usize = 1024;
const MAX_IMPORT_DEPTH: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 2048;
const MAX_ARGUMENTS: usize = 128;

/// Supported competitor document formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompetitorConfigKind {
    /// Codex `config.toml`.
    Codex,
    /// Claude Code strict `settings.json`.
    ClaudeSettings,
    /// Claude Code project/managed `.mcp.json` shape.
    ClaudeMcp,
    /// OpenCode `opencode.json` or `opencode.jsonc`.
    OpenCode,
}

/// Authority scope assigned by trusted discovery, never source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportScope {
    /// User-owned configuration.
    User,
    /// Project-owned configuration.
    Project,
}

/// Host-supplied authority for one preview.
///
/// This crate deliberately does not depend upward on `heycode-trust`. The product
/// host maps its canonical trust decision and executable policy into this
/// narrow capability value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportAuthority {
    scope: ImportScope,
    project_trusted: bool,
    executable_allowed: bool,
}

impl ImportAuthority {
    /// User-owned metadata, with an explicit executable-import decision.
    #[must_use]
    pub const fn user(executable_allowed: bool) -> Self {
        Self {
            scope: ImportScope::User,
            project_trusted: true,
            executable_allowed,
        }
    }

    /// Project metadata with separate trust and executable decisions.
    #[must_use]
    pub const fn project(project_trusted: bool, executable_allowed: bool) -> Self {
        Self {
            scope: ImportScope::Project,
            project_trusted,
            executable_allowed,
        }
    }

    fn readiness(self, executable: bool) -> ImportReadiness {
        if self.scope == ImportScope::Project && !self.project_trusted {
            ImportReadiness::ProjectTrustRequired
        } else if executable && !self.executable_allowed {
            ImportReadiness::ExecutableAuthorityRequired
        } else {
            ImportReadiness::Ready
        }
    }

    fn mcp_readiness(self, executable: bool, excluded_required: bool) -> ImportReadiness {
        let authority = self.readiness(executable);
        if authority == ImportReadiness::Ready && excluded_required {
            ImportReadiness::ExcludedMetadataRequired
        } else {
            authority
        }
    }
}

/// Whether an imported row can enter a typed candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportReadiness {
    /// Authority is sufficient.
    Ready,
    /// Project trust has not been granted.
    ProjectTrustRequired,
    /// Metadata may be viewed but a local executable may not be activated.
    ExecutableAuthorityRequired,
    /// Safe transport metadata was present, but credentials/environment or an
    /// unsupported execution field was deliberately omitted. MCP14 or another
    /// typed owner must complete it before application.
    ExcludedMetadataRequired,
}

/// Structural kind of an unknown field. Values are deliberately absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportValueKind {
    /// JSON null.
    Null,
    /// Boolean.
    Boolean,
    /// Integer or floating-point number.
    Number,
    /// Text.
    String,
    /// Array.
    Array,
    /// Object/table.
    Object,
}

/// Why a field's value is structurally absent from a preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportExclusionReason {
    /// Field name or role can carry credentials, environment, headers or auth.
    CredentialMaterial,
    /// Hook/command-style extension carries executable authority but has no
    /// typed heycode import boundary here.
    ExecutableAuthority,
    /// Known metadata value was unsafe, interpolated or credential-shaped.
    UnsafeValue,
    /// The competitor format does not honor this field at the discovered scope.
    UnsupportedSourceScope,
}

/// One unknown field visible without its source value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFieldNotice {
    path: String,
    kind: ImportValueKind,
}

impl ImportFieldNotice {
    /// Stable dotted field path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Structural value kind.
    #[must_use]
    pub const fn kind(&self) -> ImportValueKind {
        self.kind
    }
}

/// One excluded field. No source-value field exists by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedImportField {
    path: String,
    reason: ImportExclusionReason,
}

impl ExcludedImportField {
    /// Stable dotted field path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exclusion class.
    #[must_use]
    pub const fn reason(&self) -> ImportExclusionReason {
        self.reason
    }
}

/// Safe provider/model route metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedProviderRoute {
    provider: String,
    model: Option<String>,
    base_url: Option<String>,
    readiness: ImportReadiness,
}

impl ImportedProviderRoute {
    /// Provider id.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Explicit model id, if the source selected one.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Credential-free HTTP base URL, if explicitly configured and safe.
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// Authority required before application.
    #[must_use]
    pub const fn readiness(&self) -> ImportReadiness {
        self.readiness
    }
}

/// Safe MCP transport metadata. There is deliberately no environment, header,
/// OAuth or credential-reference variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedMcpTransport {
    /// Local stdio executable and exact screened arguments.
    Stdio {
        /// Executable name/path.
        command: String,
        /// Exact arguments.
        args: Vec<String>,
    },
    /// Credential-free Streamable HTTP endpoint.
    StreamableHttp {
        /// Absolute HTTP(S) endpoint without userinfo/query/fragment.
        url: String,
    },
}

/// One imported MCP server preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedMcpServer {
    name: String,
    transport: ImportedMcpTransport,
    enabled: bool,
    readiness: ImportReadiness,
}

impl ImportedMcpServer {
    /// Server id.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Safe transport metadata.
    #[must_use]
    pub const fn transport(&self) -> &ImportedMcpTransport {
        &self.transport
    }

    /// Source enablement. Disabled rows remain preview-only because the root
    /// `Config` MCP shape has no disabled state.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Authority required before application.
    #[must_use]
    pub const fn readiness(&self) -> ImportReadiness {
        self.readiness
    }
}

/// Existing typed boundary an imported setting can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSettingTarget {
    /// [`crate::Config::compaction`] auto flag.
    ConfigCompactionAuto,
    /// Safe metadata with no equivalent `heycode-config` field.
    PreviewOnly,
}

#[derive(Debug, Clone, PartialEq)]
enum ImportedSettingValue {
    ConfigCompactionAuto(bool),
    PreviewBoolean(bool),
    PreviewText(String),
}

/// One safe settings metadata row.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSetting {
    path: String,
    value: ImportedSettingValue,
    readiness: ImportReadiness,
}

impl ImportedSetting {
    /// Source field path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Typed application target, or preview-only.
    #[must_use]
    pub const fn target(&self) -> ImportSettingTarget {
        match self.value {
            ImportedSettingValue::ConfigCompactionAuto(_) => {
                ImportSettingTarget::ConfigCompactionAuto
            }
            ImportedSettingValue::PreviewBoolean(_) | ImportedSettingValue::PreviewText(_) => {
                ImportSettingTarget::PreviewOnly
            }
        }
    }

    /// Boolean value when present.
    #[must_use]
    pub const fn boolean(&self) -> Option<bool> {
        match self.value {
            ImportedSettingValue::ConfigCompactionAuto(value)
            | ImportedSettingValue::PreviewBoolean(value) => Some(value),
            ImportedSettingValue::PreviewText(_) => None,
        }
    }

    /// Safe text value when present.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.value {
            ImportedSettingValue::PreviewText(value) => Some(value),
            ImportedSettingValue::ConfigCompactionAuto(_)
            | ImportedSettingValue::PreviewBoolean(_) => None,
        }
    }

    /// Authority required before application.
    #[must_use]
    pub const fn readiness(&self) -> ImportReadiness {
        self.readiness
    }
}

/// Complete, no-write competitor metadata preview.
#[derive(Debug, Clone, PartialEq)]
pub struct CompetitorImportPreview {
    kind: CompetitorConfigKind,
    scope: ImportScope,
    provider: Option<ImportedProviderRoute>,
    mcp_servers: Vec<ImportedMcpServer>,
    settings: Vec<ImportedSetting>,
    unknown_fields: Vec<ImportFieldNotice>,
    excluded_fields: Vec<ExcludedImportField>,
}

impl CompetitorImportPreview {
    /// Source format.
    #[must_use]
    pub const fn kind(&self) -> CompetitorConfigKind {
        self.kind
    }

    /// Authority scope supplied by discovery.
    #[must_use]
    pub const fn scope(&self) -> ImportScope {
        self.scope
    }

    /// Selected provider metadata.
    #[must_use]
    pub const fn provider(&self) -> Option<&ImportedProviderRoute> {
        self.provider.as_ref()
    }

    /// MCP server previews in stable name order.
    #[must_use]
    pub fn mcp_servers(&self) -> &[ImportedMcpServer] {
        &self.mcp_servers
    }

    /// Safe settings metadata in stable path order.
    #[must_use]
    pub fn settings(&self) -> &[ImportedSetting] {
        &self.settings
    }

    /// Unknown fields without source values.
    #[must_use]
    pub fn unknown_fields(&self) -> &[ImportFieldNotice] {
        &self.unknown_fields
    }

    /// Excluded fields without source values.
    #[must_use]
    pub fn excluded_fields(&self) -> &[ExcludedImportField] {
        &self.excluded_fields
    }

    /// Build a detached typed config candidate. This never writes either the
    /// competitor source or a heycode destination.
    ///
    /// Disabled MCP rows and preview-only settings remain visible but are not
    /// applied. Any enabled applicable row lacking authority refuses the whole
    /// candidate, so application is never partial because one executable was
    /// silently skipped.
    ///
    /// # Errors
    /// Missing project/executable authority, required excluded MCP metadata,
    /// or an MCP name collision with the base configuration.
    pub fn config_candidate(&self, base: &Config) -> Result<Config, CompetitorImportError> {
        let applicable_readiness = self
            .provider
            .iter()
            .map(|route| route.readiness)
            .chain(
                self.mcp_servers
                    .iter()
                    .filter(|server| server.enabled)
                    .map(|server| server.readiness),
            )
            .chain(
                self.settings
                    .iter()
                    .filter(|setting| setting.target() != ImportSettingTarget::PreviewOnly)
                    .map(|setting| setting.readiness),
            )
            .collect::<Vec<_>>();
        if applicable_readiness.iter().any(|readiness| {
            matches!(
                readiness,
                ImportReadiness::ProjectTrustRequired
                    | ImportReadiness::ExecutableAuthorityRequired
            )
        }) {
            return Err(CompetitorImportError::AuthorityRequired);
        }
        if applicable_readiness.contains(&ImportReadiness::ExcludedMetadataRequired) {
            return Err(CompetitorImportError::ExcludedMetadataRequired);
        }

        let mut candidate = base.clone();
        if let Some(route) = &self.provider {
            candidate.llm.provider.clone_from(&route.provider);
            if let Some(model) = &route.model {
                candidate.llm.model.clone_from(model);
            }
            candidate.llm.base_url.clone_from(&route.base_url);
            // A competitor route can never import or retain a credential
            // pointer as part of the route change.
            candidate.llm.api_key_env = None;
        }
        for server in self.mcp_servers.iter().filter(|server| server.enabled) {
            if candidate.mcp.servers.contains_key(&server.name) {
                return Err(CompetitorImportError::Conflict {
                    name: server.name.clone(),
                });
            }
            let config = match &server.transport {
                ImportedMcpTransport::Stdio { command, args } => McpServerCfg {
                    command: Some(command.clone()),
                    url: None,
                    args: args.clone(),
                    env: HashMap::new(),
                    required: false,
                },
                ImportedMcpTransport::StreamableHttp { url } => McpServerCfg {
                    command: None,
                    url: Some(url.clone()),
                    args: Vec::new(),
                    env: HashMap::new(),
                    required: false,
                },
            };
            candidate.mcp.servers.insert(server.name.clone(), config);
        }
        for setting in &self.settings {
            match &setting.value {
                ImportedSettingValue::ConfigCompactionAuto(enabled) => {
                    candidate.compaction.auto = *enabled;
                }
                ImportedSettingValue::PreviewBoolean(_) | ImportedSettingValue::PreviewText(_) => {}
            }
        }
        Ok(candidate)
    }
}

/// Stable import failure. Source values are never retained in an error.
#[derive(Debug, thiserror::Error)]
pub enum CompetitorImportError {
    /// Document exceeds the preview ceiling.
    #[error("competitor config exceeds the preview size limit")]
    DocumentTooLarge,
    /// Document syntax does not match its selected format.
    #[error("competitor config is malformed")]
    Malformed,
    /// Root must be an object/table.
    #[error("competitor config root must be an object")]
    RootNotObject,
    /// Structural traversal limits were exceeded.
    #[error("competitor config has too many or too-deep fields")]
    StructureLimit,
    /// Known field had an incompatible shape.
    #[error("competitor config field `{path}` has an invalid shape")]
    InvalidField {
        /// Safe field path only.
        path: String,
    },
    /// A candidate requires trust or executable authority not supplied.
    #[error("competitor config candidate requires additional authority")]
    AuthorityRequired,
    /// Applying an MCP row would silently omit required source metadata.
    #[error("competitor config candidate requires excluded MCP metadata")]
    ExcludedMetadataRequired,
    /// Candidate would overwrite an existing MCP server.
    #[error("competitor config MCP server `{name}` conflicts with existing config")]
    Conflict {
        /// Safe server id.
        name: String,
    },
}

/// Parse one already-read source into a strict no-write preview.
///
/// # Errors
/// Oversize/malformed documents, invalid known field shapes, structural limits
/// or unusable safe metadata fail without exposing source values.
pub fn preview_competitor_config(
    kind: CompetitorConfigKind,
    authority: ImportAuthority,
    raw: &str,
) -> Result<CompetitorImportPreview, CompetitorImportError> {
    if raw.len() > MAX_IMPORT_BYTES {
        return Err(CompetitorImportError::DocumentTooLarge);
    }
    let root = parse_document(kind, raw)?;
    let object = root
        .as_object()
        .ok_or(CompetitorImportError::RootNotObject)?;
    let mut audit = Audit::default();
    let mut parsed = ParsedPreview::default();
    match kind {
        CompetitorConfigKind::Codex => parse_codex(object, authority, &mut audit, &mut parsed)?,
        CompetitorConfigKind::ClaudeSettings => {
            parse_claude_settings(object, authority, &mut audit, &mut parsed)?;
        }
        CompetitorConfigKind::ClaudeMcp => {
            parse_claude_mcp(object, authority, &mut audit, &mut parsed)?;
        }
        CompetitorConfigKind::OpenCode => {
            parse_opencode(object, authority, &mut audit, &mut parsed)?;
        }
    }
    let unknown_fields = audit.finish(&root)?;
    parsed
        .mcp_servers
        .sort_by(|left, right| left.name.cmp(&right.name));
    parsed
        .settings
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(CompetitorImportPreview {
        kind,
        scope: authority.scope,
        provider: parsed.provider,
        mcp_servers: parsed.mcp_servers,
        settings: parsed.settings,
        unknown_fields,
        excluded_fields: audit.excluded,
    })
}

#[derive(Default)]
struct ParsedPreview {
    provider: Option<ImportedProviderRoute>,
    mcp_servers: Vec<ImportedMcpServer>,
    settings: Vec<ImportedSetting>,
}

#[derive(Default)]
struct Audit {
    consumed: BTreeSet<Vec<String>>,
    excluded_prefixes: Vec<Vec<String>>,
    excluded: Vec<ExcludedImportField>,
}

impl Audit {
    fn consume(&mut self, path: Vec<String>) {
        self.consumed.insert(path);
    }

    fn exclude(&mut self, path: Vec<String>, reason: ImportExclusionReason) {
        if self
            .excluded_prefixes
            .iter()
            .any(|existing| existing == &path)
        {
            return;
        }
        self.excluded.push(ExcludedImportField {
            path: display_path(&path),
            reason,
        });
        self.excluded_prefixes.push(path);
    }

    fn hidden(&self, path: &[String]) -> bool {
        self.excluded_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
    }

    fn finish(&mut self, root: &Value) -> Result<Vec<ImportFieldNotice>, CompetitorImportError> {
        let mut unknown = Vec::new();
        collect_unknown(root, &mut Vec::new(), 0, self, &mut unknown)?;
        if unknown.len() + self.excluded.len() > MAX_IMPORT_FIELDS {
            return Err(CompetitorImportError::StructureLimit);
        }
        unknown.sort_by(|left, right| left.path.cmp(&right.path));
        unknown.dedup_by(|left, right| left.path == right.path);
        self.excluded.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.reason.cmp(&right.reason))
        });
        self.excluded
            .dedup_by(|left, right| left.path == right.path && left.reason == right.reason);
        Ok(unknown)
    }
}

#[derive(Debug)]
enum ParsedField<T> {
    Absent,
    Value(T),
    Excluded,
}

fn parse_document(kind: CompetitorConfigKind, raw: &str) -> Result<Value, CompetitorImportError> {
    match kind {
        CompetitorConfigKind::Codex => {
            let value: toml::Value =
                toml::from_str(raw).map_err(|_| CompetitorImportError::Malformed)?;
            serde_json::to_value(value).map_err(|_| CompetitorImportError::Malformed)
        }
        CompetitorConfigKind::ClaudeSettings | CompetitorConfigKind::ClaudeMcp => {
            serde_json::from_str(raw).map_err(|_| CompetitorImportError::Malformed)
        }
        CompetitorConfigKind::OpenCode => {
            let json = normalize_jsonc(raw)?;
            serde_json::from_str(&json).map_err(|_| CompetitorImportError::Malformed)
        }
    }
}

fn parse_codex(
    root: &Map<String, Value>,
    authority: ImportAuthority,
    audit: &mut Audit,
    parsed: &mut ParsedPreview,
) -> Result<(), CompetitorImportError> {
    let model_path = path(&["model"]);
    let provider_path = path(&["model_provider"]);
    let model = screened_identifier(root, "model", &model_path, audit, 256)?;
    if root.contains_key("projects") {
        audit.exclude(
            path(&["projects"]),
            ImportExclusionReason::UnsupportedSourceScope,
        );
    }
    let project_provider_unsupported = authority.scope == ImportScope::Project
        && (root.contains_key("model_provider") || root.contains_key("model_providers"));
    let provider = if project_provider_unsupported && root.contains_key("model_provider") {
        audit.exclude(
            provider_path.clone(),
            ImportExclusionReason::UnsupportedSourceScope,
        );
        ParsedField::Excluded
    } else {
        screened_identifier(root, "model_provider", &provider_path, audit, MAX_ID_BYTES)?
    };
    if project_provider_unsupported && root.contains_key("model_providers") {
        audit.exclude(
            path(&["model_providers"]),
            ImportExclusionReason::UnsupportedSourceScope,
        );
    }

    let provider_value = match (&provider, &model) {
        (ParsedField::Value(value), _) => Some(value.clone()),
        (ParsedField::Absent, ParsedField::Value(_)) => Some("openai".to_owned()),
        (ParsedField::Absent | ParsedField::Excluded, _) => None,
    };
    let model_value = match model {
        ParsedField::Value(value) => Some(value),
        ParsedField::Absent => None,
        ParsedField::Excluded => None,
    };
    let mut base_url = ParsedField::Absent;
    if !project_provider_unsupported
        && let Some(providers) = optional_object(root, "model_providers", audit)?
    {
        for (id, value) in providers {
            let provider_entry_path = path_dynamic(&["model_providers"], id);
            let Some(entry) = value.as_object() else {
                return Err(invalid(&provider_entry_path));
            };
            exclude_named_fields(
                entry,
                &provider_entry_path,
                &[
                    "auth",
                    "env_http_headers",
                    "env_key",
                    "env_key_instructions",
                    "experimental_bearer_token",
                    "http_headers",
                    "query_params",
                ],
                ImportExclusionReason::CredentialMaterial,
                audit,
            );
            if provider_value.as_deref() == Some(id.as_str()) && !project_provider_unsupported {
                let mut url_path = provider_entry_path.clone();
                url_path.push("base_url".to_owned());
                base_url = screened_url(entry, "base_url", &url_path, audit)?;
            }
        }
    }
    if !project_provider_unsupported
        && let (Some(provider), Some(model)) = (provider_value, model_value)
        && !matches!(base_url, ParsedField::Excluded)
    {
        parsed.provider = Some(ImportedProviderRoute {
            provider,
            model: Some(model),
            base_url: field_option(base_url),
            readiness: authority.readiness(false),
        });
    }

    if let Some(value) = optional_string(root, "sandbox_mode", audit)? {
        match value.as_str() {
            "read-only" | "workspace-write" | "danger-full-access" => {}
            _ => return Err(invalid(&path(&["sandbox_mode"]))),
        }
        // Codex sandbox modes also carry network/temp semantics that heycode's
        // three filesystem modes do not reproduce. Preserve the safe label for
        // preview rather than silently weakening the policy on application.
        parsed.settings.push(ImportedSetting {
            path: "sandbox_mode".to_owned(),
            value: ImportedSettingValue::PreviewText(value),
            readiness: authority.readiness(false),
        });
    }
    if let Some(value) = root.get("approval_policy") {
        match value {
            Value::String(value)
                if safe_metadata_text(value, MAX_TEXT_BYTES) && !value_looks_secret(value) =>
            {
                audit.consume(path(&["approval_policy"]));
                parsed.settings.push(ImportedSetting {
                    path: "approval_policy".to_owned(),
                    value: ImportedSettingValue::PreviewText(value.clone()),
                    readiness: authority.readiness(false),
                });
            }
            // Current Codex also accepts a granular table. heycode has no exact
            // equivalent, so its leaves remain visible as unknown metadata.
            Value::Object(_) => {}
            Value::String(_) => audit.exclude(
                path(&["approval_policy"]),
                ImportExclusionReason::UnsafeValue,
            ),
            _ => return Err(invalid(&path(&["approval_policy"]))),
        }
    }
    if let Some(servers) = optional_object(root, "mcp_servers", audit)? {
        parse_codex_mcp(servers, authority, audit, &mut parsed.mcp_servers)?;
    }
    Ok(())
}

fn parse_codex_mcp(
    servers: &Map<String, Value>,
    authority: ImportAuthority,
    audit: &mut Audit,
    output: &mut Vec<ImportedMcpServer>,
) -> Result<(), CompetitorImportError> {
    for (name, value) in servers {
        let entry_path = path_dynamic(&["mcp_servers"], name);
        if !safe_id(name) {
            audit.exclude(entry_path, ImportExclusionReason::UnsafeValue);
            continue;
        }
        let Some(entry) = value.as_object() else {
            return Err(invalid(&entry_path));
        };
        let credential_fields = [
            "auth",
            "bearer_token_env_var",
            "env",
            "env_http_headers",
            "env_vars",
            "http_headers",
            "oauth",
            "oauth_resource",
            "scopes",
        ];
        let excluded_required =
            has_named_fields(entry, &credential_fields) || entry.contains_key("cwd");
        exclude_named_fields(
            entry,
            &entry_path,
            &credential_fields,
            ImportExclusionReason::CredentialMaterial,
            audit,
        );
        exclude_named_fields(
            entry,
            &entry_path,
            &["cwd"],
            ImportExclusionReason::ExecutableAuthority,
            audit,
        );
        let enabled = optional_bool_in(entry, "enabled", &entry_path, audit)?.unwrap_or(true);
        let command = screened_text_in(entry, "command", &entry_path, audit, MAX_TEXT_BYTES)?;
        let url = screened_url_in(entry, "url", &entry_path, audit)?;
        let transport = match (command, url) {
            (ParsedField::Value(command), ParsedField::Absent) => {
                let args = match screened_array_in(entry, "args", &entry_path, audit)? {
                    ParsedField::Value(args) => args,
                    ParsedField::Absent => Vec::new(),
                    ParsedField::Excluded => continue,
                };
                ImportedMcpTransport::Stdio { command, args }
            }
            (ParsedField::Absent, ParsedField::Value(url)) => {
                ImportedMcpTransport::StreamableHttp { url }
            }
            (ParsedField::Excluded, _) | (_, ParsedField::Excluded) => continue,
            _ => return Err(invalid(&entry_path)),
        };
        let executable = matches!(transport, ImportedMcpTransport::Stdio { .. });
        output.push(ImportedMcpServer {
            name: name.clone(),
            transport,
            enabled,
            readiness: authority.mcp_readiness(executable, excluded_required),
        });
    }
    Ok(())
}

fn parse_claude_settings(
    root: &Map<String, Value>,
    authority: ImportAuthority,
    audit: &mut Audit,
    parsed: &mut ParsedPreview,
) -> Result<(), CompetitorImportError> {
    consume_schema(root, audit);
    exclude_named_fields(
        root,
        &[],
        &["apiKeyHelper", "env"],
        ImportExclusionReason::CredentialMaterial,
        audit,
    );
    exclude_named_fields(
        root,
        &[],
        &["hooks"],
        ImportExclusionReason::ExecutableAuthority,
        audit,
    );
    if let ParsedField::Value(model) =
        screened_identifier(root, "model", &path(&["model"]), audit, 256)?
    {
        parsed.provider = Some(ImportedProviderRoute {
            provider: "anthropic".to_owned(),
            model: Some(model),
            base_url: None,
            readiness: authority.readiness(false),
        });
    }
    for key in ["editorMode", "effortLevel"] {
        if let Some(value) = optional_string(root, key, audit)? {
            if !safe_metadata_text(&value, MAX_TEXT_BYTES) || value_looks_secret(&value) {
                audit.exclude(path(&[key]), ImportExclusionReason::UnsafeValue);
                continue;
            }
            parsed.settings.push(ImportedSetting {
                path: key.to_owned(),
                value: ImportedSettingValue::PreviewText(value),
                readiness: authority.readiness(false),
            });
        }
    }
    if let Some(value) = optional_bool(root, "enableAllProjectMcpServers", audit)? {
        parsed.settings.push(ImportedSetting {
            path: "enableAllProjectMcpServers".to_owned(),
            value: ImportedSettingValue::PreviewBoolean(value),
            readiness: authority.readiness(false),
        });
    }
    Ok(())
}

fn parse_claude_mcp(
    root: &Map<String, Value>,
    authority: ImportAuthority,
    audit: &mut Audit,
    parsed: &mut ParsedPreview,
) -> Result<(), CompetitorImportError> {
    consume_schema(root, audit);
    let servers = required_object(root, "mcpServers", audit)?;
    for (name, value) in servers {
        let entry_path = path_dynamic(&["mcpServers"], name);
        if !safe_id(name) {
            audit.exclude(entry_path, ImportExclusionReason::UnsafeValue);
            continue;
        }
        let Some(entry) = value.as_object() else {
            return Err(invalid(&entry_path));
        };
        let credential_fields = ["env", "headers", "headersHelper", "oauth"];
        let excluded_required = has_named_fields(entry, &credential_fields);
        exclude_named_fields(
            entry,
            &entry_path,
            &credential_fields,
            ImportExclusionReason::CredentialMaterial,
            audit,
        );
        let enabled = optional_bool_in(entry, "enabled", &entry_path, audit)?.unwrap_or(true);
        let kind = optional_string_in(entry, "type", &entry_path, audit)?;
        let command = screened_text_in(entry, "command", &entry_path, audit, MAX_TEXT_BYTES)?;
        let url = screened_url_in(entry, "url", &entry_path, audit)?;
        let transport = match (command, url) {
            (ParsedField::Value(command), ParsedField::Absent)
                if kind.as_deref().is_none_or(|kind| kind == "stdio") =>
            {
                let args = match screened_array_in(entry, "args", &entry_path, audit)? {
                    ParsedField::Value(args) => args,
                    ParsedField::Absent => Vec::new(),
                    ParsedField::Excluded => continue,
                };
                ImportedMcpTransport::Stdio { command, args }
            }
            (ParsedField::Absent, ParsedField::Value(url))
                if kind.as_deref().is_none_or(|kind| kind == "http") =>
            {
                ImportedMcpTransport::StreamableHttp { url }
            }
            (ParsedField::Excluded, _) | (_, ParsedField::Excluded) => continue,
            _ => return Err(invalid(&entry_path)),
        };
        let executable = matches!(transport, ImportedMcpTransport::Stdio { .. });
        parsed.mcp_servers.push(ImportedMcpServer {
            name: name.clone(),
            transport,
            enabled,
            readiness: authority.mcp_readiness(executable, excluded_required),
        });
    }
    Ok(())
}

fn parse_opencode(
    root: &Map<String, Value>,
    authority: ImportAuthority,
    audit: &mut Audit,
    parsed: &mut ParsedPreview,
) -> Result<(), CompetitorImportError> {
    consume_schema(root, audit);
    let model = screened_identifier(root, "model", &path(&["model"]), audit, 512)?;
    if let ParsedField::Value(model) = model {
        let Some((provider, model_id)) = model.split_once('/') else {
            return Err(invalid(&path(&["model"])));
        };
        if !safe_id(provider) || !safe_model_id(model_id) {
            audit.exclude(path(&["model"]), ImportExclusionReason::UnsafeValue);
        } else {
            let mut base_url = ParsedField::Absent;
            if let Some(providers) = optional_object(root, "provider", audit)?
                && let Some(value) = providers.get(provider)
            {
                let provider_path = path_dynamic(&["provider"], provider);
                let Some(entry) = value.as_object() else {
                    return Err(invalid(&provider_path));
                };
                if let Some(options) = optional_object_in(entry, "options", &provider_path, audit)?
                {
                    let mut options_path = provider_path.clone();
                    options_path.push("options".to_owned());
                    exclude_credential_children(options, &options_path, audit);
                    if options.contains_key("baseURL") && options.contains_key("endpoint") {
                        return Err(invalid(&options_path));
                    }
                    let endpoint = if options.contains_key("baseURL") {
                        screened_url_in(options, "baseURL", &options_path, audit)?
                    } else {
                        screened_url_in(options, "endpoint", &options_path, audit)?
                    };
                    base_url = endpoint;
                }
            }
            if !matches!(base_url, ParsedField::Excluded) {
                parsed.provider = Some(ImportedProviderRoute {
                    provider: provider.to_owned(),
                    model: Some(model_id.to_owned()),
                    base_url: field_option(base_url),
                    readiness: authority.readiness(false),
                });
            }
        }
    } else if let Some(providers) = optional_object(root, "provider", audit)? {
        for (id, value) in providers {
            if let Some(entry) = value.as_object() {
                let provider_path = path_dynamic(&["provider"], id);
                if let Some(options) = optional_object_in(entry, "options", &provider_path, audit)?
                {
                    let mut options_path = provider_path;
                    options_path.push("options".to_owned());
                    exclude_credential_children(options, &options_path, audit);
                }
            }
        }
    }

    if let Some(compaction) = optional_object(root, "compaction", audit)?
        && let Some(auto) = optional_bool_in(compaction, "auto", &path(&["compaction"]), audit)?
    {
        parsed.settings.push(ImportedSetting {
            path: "compaction.auto".to_owned(),
            value: ImportedSettingValue::ConfigCompactionAuto(auto),
            readiness: authority.readiness(false),
        });
    }
    if let Some(auto_update) = optional_bool(root, "autoupdate", audit)? {
        parsed.settings.push(ImportedSetting {
            path: "autoupdate".to_owned(),
            value: ImportedSettingValue::PreviewBoolean(auto_update),
            readiness: authority.readiness(false),
        });
    }
    if let Some(mcp) = optional_object(root, "mcp", audit)? {
        let nested_servers = mcp
            .get("servers")
            .and_then(Value::as_object)
            .filter(|entry| {
                !entry.contains_key("type")
                    && !entry.contains_key("command")
                    && !entry.contains_key("url")
            });
        let (servers, prefix) = nested_servers.map_or((mcp, vec!["mcp"]), |servers| {
            (servers, vec!["mcp", "servers"])
        });
        parse_opencode_mcp(servers, &prefix, authority, audit, &mut parsed.mcp_servers)?;
    }
    Ok(())
}

fn parse_opencode_mcp(
    servers: &Map<String, Value>,
    prefix: &[&str],
    authority: ImportAuthority,
    audit: &mut Audit,
    output: &mut Vec<ImportedMcpServer>,
) -> Result<(), CompetitorImportError> {
    for (name, value) in servers {
        let entry_path = path_dynamic(prefix, name);
        if !safe_id(name) {
            audit.exclude(entry_path, ImportExclusionReason::UnsafeValue);
            continue;
        }
        let Some(entry) = value.as_object() else {
            return Err(invalid(&entry_path));
        };
        let credential_fields = ["environment", "headers", "oauth"];
        let excluded_required =
            has_named_fields(entry, &credential_fields) || entry.contains_key("cwd");
        exclude_named_fields(
            entry,
            &entry_path,
            &credential_fields,
            ImportExclusionReason::CredentialMaterial,
            audit,
        );
        exclude_named_fields(
            entry,
            &entry_path,
            &["cwd"],
            ImportExclusionReason::ExecutableAuthority,
            audit,
        );
        let kind = optional_string_in(entry, "type", &entry_path, audit)?;
        let enabled = optional_bool_in(entry, "enabled", &entry_path, audit)?;
        let disabled = optional_bool_in(entry, "disabled", &entry_path, audit)?;
        let enabled = enabled.unwrap_or(!disabled.unwrap_or(false));
        let transport = match kind.as_deref() {
            Some("local") => {
                let command = screened_array_in(entry, "command", &entry_path, audit)?;
                let ParsedField::Value(mut command) = command else {
                    continue;
                };
                if command.is_empty() {
                    return Err(invalid(&entry_path));
                }
                let executable = command.remove(0);
                ImportedMcpTransport::Stdio {
                    command: executable,
                    args: command,
                }
            }
            Some("remote") => {
                let url = screened_url_in(entry, "url", &entry_path, audit)?;
                let ParsedField::Value(url) = url else {
                    continue;
                };
                ImportedMcpTransport::StreamableHttp { url }
            }
            _ => return Err(invalid(&entry_path)),
        };
        let executable = matches!(transport, ImportedMcpTransport::Stdio { .. });
        output.push(ImportedMcpServer {
            name: name.clone(),
            transport,
            enabled,
            readiness: authority.mcp_readiness(executable, excluded_required),
        });
    }
    Ok(())
}

fn consume_schema(root: &Map<String, Value>, audit: &mut Audit) {
    if root.contains_key("$schema") {
        audit.consume(path(&["$schema"]));
    }
}

fn required_object<'a>(
    root: &'a Map<String, Value>,
    key: &str,
    _audit: &mut Audit,
) -> Result<&'a Map<String, Value>, CompetitorImportError> {
    let Some(value) = root.get(key) else {
        return Err(invalid(&path(&[key])));
    };
    value.as_object().ok_or_else(|| invalid(&path(&[key])))
}

fn optional_object<'a>(
    root: &'a Map<String, Value>,
    key: &str,
    _audit: &mut Audit,
) -> Result<Option<&'a Map<String, Value>>, CompetitorImportError> {
    match root.get(key) {
        Some(value) => value
            .as_object()
            .map(Some)
            .ok_or_else(|| invalid(&path(&[key]))),
        None => Ok(None),
    }
}

fn optional_object_in<'a>(
    root: &'a Map<String, Value>,
    key: &str,
    parent: &[String],
    _audit: &mut Audit,
) -> Result<Option<&'a Map<String, Value>>, CompetitorImportError> {
    let field = child(parent, key);
    match root.get(key) {
        Some(value) => value.as_object().map(Some).ok_or_else(|| invalid(&field)),
        None => Ok(None),
    }
}

fn optional_string(
    root: &Map<String, Value>,
    key: &str,
    audit: &mut Audit,
) -> Result<Option<String>, CompetitorImportError> {
    optional_string_in(root, key, &[], audit)
}

fn optional_string_in(
    root: &Map<String, Value>,
    key: &str,
    parent: &[String],
    audit: &mut Audit,
) -> Result<Option<String>, CompetitorImportError> {
    let field = child(parent, key);
    match root.get(key) {
        Some(Value::String(value)) => {
            audit.consume(field);
            Ok(Some(value.clone()))
        }
        Some(_) => Err(invalid(&field)),
        None => Ok(None),
    }
}

fn optional_bool(
    root: &Map<String, Value>,
    key: &str,
    audit: &mut Audit,
) -> Result<Option<bool>, CompetitorImportError> {
    optional_bool_in(root, key, &[], audit)
}

fn optional_bool_in(
    root: &Map<String, Value>,
    key: &str,
    parent: &[String],
    audit: &mut Audit,
) -> Result<Option<bool>, CompetitorImportError> {
    let field = child(parent, key);
    match root.get(key) {
        Some(Value::Bool(value)) => {
            audit.consume(field);
            Ok(Some(*value))
        }
        Some(_) => Err(invalid(&field)),
        None => Ok(None),
    }
}

fn screened_identifier(
    root: &Map<String, Value>,
    key: &str,
    field: &[String],
    audit: &mut Audit,
    maximum: usize,
) -> Result<ParsedField<String>, CompetitorImportError> {
    match root.get(key) {
        Some(Value::String(value)) => {
            if !safe_metadata_text(value, maximum)
                || value_looks_secret(value)
                || (key == "model_provider" && !safe_id(value))
                || (key == "model" && !safe_model_id(value))
            {
                audit.exclude(field.to_vec(), ImportExclusionReason::UnsafeValue);
                Ok(ParsedField::Excluded)
            } else {
                audit.consume(field.to_vec());
                Ok(ParsedField::Value(value.clone()))
            }
        }
        Some(_) => Err(invalid(field)),
        None => Ok(ParsedField::Absent),
    }
}

fn screened_text_in(
    root: &Map<String, Value>,
    key: &str,
    parent: &[String],
    audit: &mut Audit,
    maximum: usize,
) -> Result<ParsedField<String>, CompetitorImportError> {
    let field = child(parent, key);
    match root.get(key) {
        Some(Value::String(value)) => {
            if !safe_metadata_text(value, maximum) || value_looks_secret(value) {
                audit.exclude(field, ImportExclusionReason::UnsafeValue);
                Ok(ParsedField::Excluded)
            } else {
                audit.consume(field);
                Ok(ParsedField::Value(value.clone()))
            }
        }
        Some(_) => Err(invalid(&field)),
        None => Ok(ParsedField::Absent),
    }
}

fn screened_array_in(
    root: &Map<String, Value>,
    key: &str,
    parent: &[String],
    audit: &mut Audit,
) -> Result<ParsedField<Vec<String>>, CompetitorImportError> {
    let field = child(parent, key);
    let Some(value) = root.get(key) else {
        return Ok(ParsedField::Absent);
    };
    let Some(values) = value.as_array() else {
        return Err(invalid(&field));
    };
    if values.len() > MAX_ARGUMENTS {
        audit.exclude(field, ImportExclusionReason::UnsafeValue);
        return Ok(ParsedField::Excluded);
    }
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let Some(value) = value.as_str() else {
            return Err(invalid(&field));
        };
        if !safe_metadata_text(value, MAX_TEXT_BYTES) || value_looks_secret(value) {
            audit.exclude(field, ImportExclusionReason::UnsafeValue);
            return Ok(ParsedField::Excluded);
        }
        output.push(value.to_owned());
    }
    audit.consume(field);
    Ok(ParsedField::Value(output))
}

fn screened_url(
    root: &Map<String, Value>,
    key: &str,
    field: &[String],
    audit: &mut Audit,
) -> Result<ParsedField<String>, CompetitorImportError> {
    screened_url_inner(root, key, field, audit)
}

fn screened_url_in(
    root: &Map<String, Value>,
    key: &str,
    parent: &[String],
    audit: &mut Audit,
) -> Result<ParsedField<String>, CompetitorImportError> {
    let field = child(parent, key);
    screened_url_inner(root, key, &field, audit)
}

fn screened_url_inner(
    root: &Map<String, Value>,
    key: &str,
    field: &[String],
    audit: &mut Audit,
) -> Result<ParsedField<String>, CompetitorImportError> {
    match root.get(key) {
        Some(Value::String(value)) => {
            if url_credential_bearing(value) {
                audit.exclude(field.to_vec(), ImportExclusionReason::CredentialMaterial);
                Ok(ParsedField::Excluded)
            } else if !safe_http_url(value) {
                audit.exclude(field.to_vec(), ImportExclusionReason::UnsafeValue);
                Ok(ParsedField::Excluded)
            } else {
                audit.consume(field.to_vec());
                Ok(ParsedField::Value(value.clone()))
            }
        }
        Some(_) => Err(invalid(field)),
        None => Ok(ParsedField::Absent),
    }
}

fn field_option<T>(field: ParsedField<T>) -> Option<T> {
    match field {
        ParsedField::Value(value) => Some(value),
        ParsedField::Absent | ParsedField::Excluded => None,
    }
}

fn exclude_named_fields(
    object: &Map<String, Value>,
    parent: &[String],
    fields: &[&str],
    reason: ImportExclusionReason,
    audit: &mut Audit,
) {
    for field in fields {
        if object.contains_key(*field) {
            audit.exclude(child(parent, field), reason);
        }
    }
}

fn has_named_fields(object: &Map<String, Value>, fields: &[&str]) -> bool {
    fields.iter().any(|field| object.contains_key(*field))
}

fn exclude_credential_children(object: &Map<String, Value>, parent: &[String], audit: &mut Audit) {
    for key in object.keys() {
        if credential_key(key) {
            audit.exclude(
                child(parent, key),
                ImportExclusionReason::CredentialMaterial,
            );
        }
    }
}

fn collect_unknown(
    value: &Value,
    path: &mut Vec<String>,
    depth: usize,
    audit: &mut Audit,
    output: &mut Vec<ImportFieldNotice>,
) -> Result<(), CompetitorImportError> {
    if depth > MAX_IMPORT_DEPTH || output.len() + audit.excluded.len() > MAX_IMPORT_FIELDS {
        return Err(CompetitorImportError::StructureLimit);
    }
    if audit.hidden(path) || audit.consumed.contains(path) {
        return Ok(());
    }
    match value {
        Value::Object(object) if !object.is_empty() => {
            for (key, child_value) in object {
                path.push(key.clone());
                if credential_key(key) {
                    audit.exclude(path.clone(), ImportExclusionReason::CredentialMaterial);
                } else if executable_key(key) {
                    audit.exclude(path.clone(), ImportExclusionReason::ExecutableAuthority);
                } else {
                    collect_unknown(child_value, path, depth + 1, audit, output)?;
                }
                path.pop();
            }
        }
        other => output.push(ImportFieldNotice {
            path: display_path(path),
            kind: value_kind(other),
        }),
    }
    Ok(())
}

fn value_kind(value: &Value) -> ImportValueKind {
    match value {
        Value::Null => ImportValueKind::Null,
        Value::Bool(_) => ImportValueKind::Boolean,
        Value::Number(_) => ImportValueKind::Number,
        Value::String(_) => ImportValueKind::String,
        Value::Array(_) => ImportValueKind::Array,
        Value::Object(_) => ImportValueKind::Object,
    }
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value_looks_secret(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn safe_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
        })
}

fn safe_metadata_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn value_looks_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.starts_with("xox")
        || lower.starts_with("ghp_")
        || lower.starts_with("github_pat_")
        || lower.starts_with("akia")
        || lower.starts_with("bearer ")
        || lower.contains("sk-proj-")
        || lower.contains("sk-ant-")
        || lower.contains("github_pat_")
        || lower.contains("ghp_")
        || lower.contains("xoxb-")
        || lower.contains("-----begin ")
        || lower.contains("${")
        || lower.contains("{env:")
        || lower.contains("{file:")
        || credential_argument(&lower)
        || jwt_shaped(value)
}

fn credential_argument(lower: &str) -> bool {
    [
        "--api-key",
        "--apikey",
        "--token",
        "--secret",
        "--password",
        "authorization=",
        "bearer=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn jwt_shaped(value: &str) -> bool {
    let segments = value.split('.').collect::<Vec<_>>();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| segment.len() >= 8 && segment.bytes().all(base64url_byte))
}

fn base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn credential_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let normalized = String::from_utf8_lossy(&normalized);
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
        || normalized.contains("privatekey")
        || normalized.contains("cookie")
        || normalized.contains("header")
        || normalized == "env"
        || normalized.ends_with("environment")
        || normalized == "oauth"
        || normalized.ends_with("auth")
        || normalized == "scopes"
}

fn executable_key(key: &str) -> bool {
    matches!(key, "hooks" | "hook" | "apiKeyHelper" | "statusLine")
}

fn url_credential_bearing(value: &str) -> bool {
    if value_looks_secret(value) || value.contains(['?', '#']) || value.contains('%') {
        return true;
    }
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    rest.split('/')
        .next()
        .is_some_and(|authority| authority.contains('@'))
}

fn safe_http_url(value: &str) -> bool {
    if !safe_metadata_text(value, MAX_TEXT_BYTES)
        || value.contains(['\\', '{', '}'])
        || !(value.starts_with("http://") || value.starts_with("https://"))
    {
        return false;
    }
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    !authority.is_empty()
        && authority != ":"
        && !authority.chars().any(char::is_whitespace)
        && !authority.starts_with(':')
}

fn display_path(path: &[String]) -> String {
    if path.is_empty() {
        return "<root>".to_owned();
    }
    path.iter()
        .enumerate()
        .map(|(index, segment)| {
            if safe_path_segment(segment) {
                segment.clone()
            } else {
                format!("<field-{index}>")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn safe_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value_looks_secret(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'$'))
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn path_dynamic(parts: &[&str], dynamic: &str) -> Vec<String> {
    let mut path = path(parts);
    path.push(dynamic.to_owned());
    path
}

fn child(parent: &[String], field: &str) -> Vec<String> {
    let mut path = parent.to_vec();
    path.push(field.to_owned());
    path
}

fn invalid(path: &[String]) -> CompetitorImportError {
    CompetitorImportError::InvalidField {
        path: display_path(path),
    }
}

fn normalize_jsonc(raw: &str) -> Result<String, CompetitorImportError> {
    let stripped = strip_jsonc_comments(raw)?;
    Ok(strip_jsonc_trailing_commas(&stripped))
}

fn strip_jsonc_comments(raw: &str) -> Result<String, CompetitorImportError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Normal,
        String,
        LineComment,
        BlockComment,
    }
    let bytes = raw.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut state = State::Normal;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match state {
            State::Normal if byte == b'"' => {
                state = State::String;
                output.push(byte);
            }
            State::Normal if byte == b'/' && bytes.get(index + 1).copied() == Some(b'/') => {
                state = State::LineComment;
                output.extend_from_slice(b"  ");
                index += 1;
            }
            State::Normal if byte == b'/' && bytes.get(index + 1).copied() == Some(b'*') => {
                state = State::BlockComment;
                output.extend_from_slice(b"  ");
                index += 1;
            }
            State::Normal => output.push(byte),
            State::String => {
                output.push(byte);
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    state = State::Normal;
                }
            }
            State::LineComment if matches!(byte, b'\n' | b'\r') => {
                state = State::Normal;
                output.push(byte);
            }
            State::LineComment => output.push(b' '),
            State::BlockComment if byte == b'*' && bytes.get(index + 1).copied() == Some(b'/') => {
                state = State::Normal;
                output.extend_from_slice(b"  ");
                index += 1;
            }
            State::BlockComment if matches!(byte, b'\n' | b'\r') => output.push(byte),
            State::BlockComment => output.push(b' '),
        }
        index += 1;
    }
    if matches!(state, State::String | State::BlockComment) {
        return Err(CompetitorImportError::Malformed);
    }
    String::from_utf8(output).map_err(|_| CompetitorImportError::Malformed)
}

fn strip_jsonc_trailing_commas(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push(byte);
            continue;
        }
        if byte == b','
            && bytes[index + 1..]
                .iter()
                .copied()
                .find(|next| !next.is_ascii_whitespace())
                .is_some_and(|next| matches!(next, b'}' | b']'))
        {
            output.push(b' ');
        } else {
            output.push(byte);
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}
