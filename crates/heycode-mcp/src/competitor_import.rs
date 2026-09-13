//! MCP14 bridge from S13's value-minimized competitor preview.
//!
//! This module never parses source files and never receives a credential value.
//! It consumes only S13's public screened MCP rows plus excluded field paths.
//! Clean rows become exact [`crate::McpServerDefinition`] candidates. Excluded
//! authentication metadata becomes deterministic unresolved
//! [`crate::McpSecretReference`] requests; an enabled incomplete row can never
//! produce a definition.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use heycode_config::{
    CompetitorConfigKind, CompetitorImportPreview, ImportExclusionReason, ImportReadiness,
    ImportScope, ImportedMcpServer, ImportedMcpTransport,
};

use crate::{
    McpArgument, McpDefinitionScope, McpReconnectPolicy, McpSecretReference, McpServerDefinition,
    McpServerId, McpStdioTransport, McpStreamableHttpTransport, McpTransportDefinition,
    McpTransportKind,
};

/// Role an excluded competitor field needs a future binding for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpImportAuthRole {
    /// One or more stdio environment bindings were excluded.
    Environment,
    /// One or more HTTP header/bearer bindings were excluded.
    HttpHeaders,
    /// OAuth metadata or scopes were excluded.
    OAuth,
    /// Credential-shaped metadata whose exact binding role was erased.
    Credential,
}

impl McpImportAuthRole {
    /// Stable identifier used in generated non-secret references.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::HttpHeaders => "http-headers",
            Self::OAuth => "oauth",
            Self::Credential => "credential",
        }
    }
}

/// Authority missing from an enabled imported row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpImportAuthorityRequirement {
    /// Canonical project trust is required.
    ProjectTrust,
    /// Local process execution authority is required.
    Executable,
}

/// One safe reference request for source metadata S13 structurally erased.
///
/// No header name, environment name, OAuth client id or credential value is
/// retained. A later UI/management owner must bind this request explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpUnresolvedAuthReference {
    field_path: String,
    role: McpImportAuthRole,
    reference: McpSecretReference,
}

impl McpUnresolvedAuthReference {
    /// S13's safe path-only source evidence.
    #[must_use]
    pub fn field_path(&self) -> &str {
        &self.field_path
    }

    /// Binding family a later owner must choose.
    #[must_use]
    pub const fn role(&self) -> McpImportAuthRole {
        self.role
    }

    /// Deterministic unresolved reference id, never credential material.
    #[must_use]
    pub const fn reference(&self) -> &McpSecretReference {
        &self.reference
    }
}

/// One MCP row projected from an S13 preview.
#[derive(Clone, PartialEq, Eq)]
pub struct McpCompetitorImportRow {
    id: McpServerId,
    enabled: bool,
    scope: McpDefinitionScope,
    transport_kind: McpTransportKind,
    target: String,
    argument_count: usize,
    readiness: ImportReadiness,
    unresolved_auth: Vec<McpUnresolvedAuthReference>,
    definition: Option<McpServerDefinition>,
}

impl McpCompetitorImportRow {
    /// Validated MCP registry id.
    #[must_use]
    pub const fn id(&self) -> &McpServerId {
        &self.id
    }

    /// Whether the source row was enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// User or trusted-project definition scope from S13 discovery authority.
    #[must_use]
    pub const fn scope(&self) -> McpDefinitionScope {
        self.scope
    }

    /// Transport family.
    #[must_use]
    pub const fn transport_kind(&self) -> McpTransportKind {
        self.transport_kind
    }

    /// Safe command or credential-free URL.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Count of screened stdio arguments. Exact values stay inside the private
    /// definition candidate rather than ordinary preview `Debug` output.
    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }

    /// Exact authority/completeness decision from S13.
    #[must_use]
    pub const fn readiness(&self) -> ImportReadiness {
        self.readiness
    }

    /// Safe unresolved bindings derived from excluded credential paths.
    #[must_use]
    pub fn unresolved_auth(&self) -> &[McpUnresolvedAuthReference] {
        &self.unresolved_auth
    }
}

impl std::fmt::Debug for McpCompetitorImportRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpCompetitorImportRow")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .field("scope", &self.scope)
            .field("transport_kind", &self.transport_kind)
            .field("argument_count", &self.argument_count)
            .field("readiness", &self.readiness)
            .field("unresolved_auth", &self.unresolved_auth)
            .field("definition_ready", &self.definition.is_some())
            .finish()
    }
}

/// Value-minimized MCP-specific preview over one S13 result.
#[derive(Clone, PartialEq, Eq)]
pub struct McpCompetitorImportPreview {
    source_kind: CompetitorConfigKind,
    scope: ImportScope,
    rows: Vec<McpCompetitorImportRow>,
}

impl McpCompetitorImportPreview {
    /// Competitor format already validated by S13.
    #[must_use]
    pub const fn source_kind(&self) -> CompetitorConfigKind {
        self.source_kind
    }

    /// Trusted discovery scope already assigned by S13's host authority.
    #[must_use]
    pub const fn scope(&self) -> ImportScope {
        self.scope
    }

    /// Stable server-id ordered rows.
    #[must_use]
    pub fn rows(&self) -> &[McpCompetitorImportRow] {
        &self.rows
    }

    /// Return exact enabled definitions after whole-set authority,
    /// completeness and collision validation.
    ///
    /// Disabled rows remain visible in the preview and produce no definition.
    /// No partial vector escapes when any enabled row fails.
    ///
    /// # Errors
    /// Project/executable authority gaps, excluded required metadata, invalid
    /// internal candidates or collisions with `existing` fail the whole set.
    pub fn definitions(
        &self,
        existing: &BTreeSet<McpServerId>,
    ) -> Result<Vec<McpServerDefinition>, McpCompetitorImportError> {
        for row in self.rows.iter().filter(|row| row.enabled) {
            match row.readiness {
                ImportReadiness::Ready => {
                    if existing.contains(&row.id) {
                        return Err(McpCompetitorImportError::DuplicateServer(row.id.clone()));
                    }
                    if row.definition.is_none() {
                        return Err(McpCompetitorImportError::InvalidDefinition);
                    }
                }
                ImportReadiness::ProjectTrustRequired => {
                    return Err(McpCompetitorImportError::AuthorityRequired {
                        server: row.id.clone(),
                        requirement: McpImportAuthorityRequirement::ProjectTrust,
                    });
                }
                ImportReadiness::ExecutableAuthorityRequired => {
                    return Err(McpCompetitorImportError::AuthorityRequired {
                        server: row.id.clone(),
                        requirement: McpImportAuthorityRequirement::Executable,
                    });
                }
                ImportReadiness::ExcludedMetadataRequired => {
                    return Err(McpCompetitorImportError::IncompleteEnabled(row.id.clone()));
                }
            }
        }
        self.rows
            .iter()
            .filter(|row| row.enabled)
            .map(|row| {
                row.definition
                    .clone()
                    .ok_or(McpCompetitorImportError::InvalidDefinition)
            })
            .collect()
    }
}

impl std::fmt::Debug for McpCompetitorImportPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpCompetitorImportPreview")
            .field("source_kind", &self.source_kind)
            .field("scope", &self.scope)
            .field("rows", &self.rows)
            .finish()
    }
}

/// Stable MCP14 failure with no competitor value or host path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpCompetitorImportError {
    /// S13 accepted a name that is not a valid MCP registry id.
    #[error("competitor MCP server id is invalid")]
    InvalidServerId,
    /// Screened metadata could not form an exact MCP definition.
    #[error("competitor MCP definition is invalid")]
    InvalidDefinition,
    /// S13 readiness and its excluded credential paths disagreed.
    #[error("competitor MCP preview is internally inconsistent")]
    InconsistentPreview,
    /// One enabled row still needs trust or executable authority.
    #[error("competitor MCP server `{server}` requires additional authority")]
    AuthorityRequired {
        /// Safe validated server id.
        server: McpServerId,
        /// Exact missing authority.
        requirement: McpImportAuthorityRequirement,
    },
    /// An enabled row cannot silently discard credential/execution metadata.
    #[error("competitor MCP server `{0}` requires excluded metadata")]
    IncompleteEnabled(McpServerId),
    /// An existing definition already owns this id.
    #[error("competitor MCP server `{0}` already exists")]
    DuplicateServer(McpServerId),
}

/// Project S13's public MCP rows into exact definitions and unresolved safe
/// references.
///
/// Provider/settings values and unknown field values are never copied. The
/// caller supplies only an absolute default cwd for already-authorized clean
/// stdio rows; there is no authority override at this layer.
///
/// # Errors
/// Invalid MCP ids, contradictory S13 evidence, or an exact clean definition
/// that fails MCP registry validation.
pub fn preview_competitor_mcp_import(
    preview: &CompetitorImportPreview,
    stdio_cwd: &Path,
) -> Result<McpCompetitorImportPreview, McpCompetitorImportError> {
    let mut rows = Vec::with_capacity(preview.mcp_servers().len());
    for server in preview.mcp_servers() {
        let id = McpServerId::new(server.name())
            .map_err(|_| McpCompetitorImportError::InvalidServerId)?;
        let scope = match preview.scope() {
            ImportScope::User => McpDefinitionScope::User,
            ImportScope::Project => McpDefinitionScope::Project,
        };
        let unresolved_auth = unresolved_auth(preview, server, &id)?;
        if server.readiness() == ImportReadiness::Ready && !unresolved_auth.is_empty() {
            return Err(McpCompetitorImportError::InconsistentPreview);
        }
        let (transport_kind, target, argument_count) = match server.transport() {
            ImportedMcpTransport::Stdio { command, args } => {
                (McpTransportKind::Stdio, command.clone(), args.len())
            }
            ImportedMcpTransport::StreamableHttp { url } => {
                (McpTransportKind::StreamableHttp, url.clone(), 0)
            }
        };
        let definition = if server.readiness() == ImportReadiness::Ready {
            Some(build_definition(server, scope, stdio_cwd)?)
        } else {
            None
        };
        rows.push(McpCompetitorImportRow {
            id,
            enabled: server.enabled(),
            scope,
            transport_kind,
            target,
            argument_count,
            readiness: server.readiness(),
            unresolved_auth,
            definition,
        });
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(McpCompetitorImportPreview {
        source_kind: preview.kind(),
        scope: preview.scope(),
        rows,
    })
}

fn build_definition(
    server: &ImportedMcpServer,
    scope: McpDefinitionScope,
    stdio_cwd: &Path,
) -> Result<McpServerDefinition, McpCompetitorImportError> {
    let transport = match server.transport() {
        ImportedMcpTransport::Stdio { command, args } => {
            let arguments = args
                .iter()
                .cloned()
                .map(McpArgument::literal)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| McpCompetitorImportError::InvalidDefinition)?;
            McpTransportDefinition::Stdio(
                McpStdioTransport::new(command, stdio_cwd, arguments, BTreeMap::new())
                    .map_err(|_| McpCompetitorImportError::InvalidDefinition)?,
            )
        }
        ImportedMcpTransport::StreamableHttp { url } => McpTransportDefinition::StreamableHttp(
            McpStreamableHttpTransport::new(url, BTreeMap::new())
                .map_err(|_| McpCompetitorImportError::InvalidDefinition)?,
        ),
    };
    let default_reconnect = McpReconnectPolicy::default();
    let reconnect = McpReconnectPolicy::new(
        false,
        default_reconnect.initial_delay_ms(),
        default_reconnect.max_delay_ms(),
        default_reconnect.max_attempts(),
    )
    .map_err(|_| McpCompetitorImportError::InvalidDefinition)?;
    McpServerDefinition::new(server.name(), server.name(), scope, transport)
        .map(|definition| {
            definition
                .with_enabled(server.enabled())
                .with_reconnect(reconnect)
        })
        .map_err(|_| McpCompetitorImportError::InvalidDefinition)
}

fn unresolved_auth(
    preview: &CompetitorImportPreview,
    server: &ImportedMcpServer,
    id: &McpServerId,
) -> Result<Vec<McpUnresolvedAuthReference>, McpCompetitorImportError> {
    let prefixes = server_prefixes(preview.kind(), server.name());
    let mut unresolved = Vec::new();
    for field in preview.excluded_fields().iter().filter(|field| {
        field.reason() == ImportExclusionReason::CredentialMaterial
            && prefixes
                .iter()
                .any(|prefix| field_belongs_to(field.path(), prefix))
    }) {
        let role = auth_role(field.path());
        let reference = McpSecretReference::new(format!(
            "import/{}/{}/{}/{}",
            source_id(preview.kind()),
            id.as_str(),
            role.as_str(),
            unresolved.len() + 1
        ))
        .map_err(|_| McpCompetitorImportError::InconsistentPreview)?;
        unresolved.push(McpUnresolvedAuthReference {
            field_path: field.path().to_owned(),
            role,
            reference,
        });
    }
    Ok(unresolved)
}

fn server_prefixes(kind: CompetitorConfigKind, server: &str) -> Vec<String> {
    match kind {
        CompetitorConfigKind::Codex => vec![format!("mcp_servers.{server}")],
        CompetitorConfigKind::ClaudeMcp => vec![format!("mcpServers.{server}")],
        CompetitorConfigKind::OpenCode => {
            vec![format!("mcp.{server}"), format!("mcp.servers.{server}")]
        }
        CompetitorConfigKind::ClaudeSettings => Vec::new(),
    }
}

fn field_belongs_to(field: &str, prefix: &str) -> bool {
    field == prefix
        || field
            .strip_prefix(prefix)
            .is_some_and(|remainder| remainder.starts_with('.'))
}

fn auth_role(path: &str) -> McpImportAuthRole {
    let path = path.to_ascii_lowercase();
    if path.contains("header") || path.contains("bearer_token") {
        McpImportAuthRole::HttpHeaders
    } else if path.ends_with(".env") || path.contains("environment") || path.contains("env_vars") {
        McpImportAuthRole::Environment
    } else if path.contains("oauth") || path.ends_with(".auth") || path.ends_with(".scopes") {
        McpImportAuthRole::OAuth
    } else {
        McpImportAuthRole::Credential
    }
}

const fn source_id(kind: CompetitorConfigKind) -> &'static str {
    match kind {
        CompetitorConfigKind::Codex => "codex",
        CompetitorConfigKind::ClaudeSettings => "claude-settings",
        CompetitorConfigKind::ClaudeMcp => "claude-mcp",
        CompetitorConfigKind::OpenCode => "opencode",
    }
}
