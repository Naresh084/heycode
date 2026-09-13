//! Credential-aware ordinary MCP connection activation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_credentials::CredentialsService;
use heycode_exec::SubprocessService;
use heycode_http::HttpService;
use heycode_tools::ToolRegistry;

use crate::{
    DriverRuntime, EstablishedGeneration, HttpGenerationRequest, McpBoundServerError::*,
    McpClientEventRouter, McpCredentialBindings, McpCredentialEncoding, McpError,
    McpServerDefinition, McpToolApprovalHandler, McpTransportDefinition, StdioGenerationRequest,
    drive_on_plain_thread, establish_http_generation, establish_stdio_generation,
    registry_core_error, unix_time_ms,
};
use crate::{McpConnectionProviderId, McpRegistry};

/// One provider-owned stdio environment source before operation-time binding.
#[derive(Clone)]
pub enum McpBoundEnvironmentSource {
    /// Reviewed non-secret literal retained only in the private definition.
    Literal(String),
    /// Credential reference plus its exact operation-time query.
    Credential {
        /// Safe transport reference.
        reference: crate::McpSecretReference,
        /// Exact credential registry query.
        query: heycode_credentials::CredentialQuery,
    },
}

impl std::fmt::Debug for McpBoundEnvironmentSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Literal(_) => formatter.write_str("Literal([REDACTED])"),
            Self::Credential { reference, query } => formatter
                .debug_struct("Credential")
                .field("reference", reference)
                .field("query", query)
                .finish(),
        }
    }
}

/// Exact definition plus every operation-time credential binding it requires.
///
/// Deliberately no `Debug`: the definition may contain private literal argv or
/// environment values. Its public registry snapshot remains the inspection
/// surface.
#[derive(Clone)]
pub struct McpBoundServer {
    definition: McpServerDefinition,
    bindings: McpCredentialBindings,
    client_events: Option<McpClientEventRouter>,
}

impl McpBoundServer {
    /// Validate one complete activation description without resolving a value.
    ///
    /// # Errors
    /// Missing/extra bindings, a non-raw stdio binding, or duplicate reference
    /// ambiguity.
    pub fn new(
        definition: McpServerDefinition,
        bindings: McpCredentialBindings,
    ) -> Result<Self, McpBoundServerError> {
        validate_bindings(&definition, &bindings)?;
        Ok(Self {
            definition,
            bindings,
            client_events: None,
        })
    }

    /// Build a provider-owned stdio server with prompt-only exact tools and
    /// operation-time credential environment bindings.
    ///
    /// This is the shared root-factory seam for MiniMax and Z.AI vision launch
    /// descriptions. It accepts no credential value and disables resources,
    /// prompts and server instructions.
    ///
    /// # Errors
    /// Invalid identity/transport/policy, an unsafe executable/cwd, or a
    /// duplicate/incoherent credential binding.
    #[allow(clippy::too_many_arguments)]
    pub fn provider_stdio(
        id: impl Into<String>,
        display_name: impl Into<String>,
        executable: impl AsRef<std::path::Path>,
        cwd: impl AsRef<std::path::Path>,
        arguments: Vec<String>,
        environment: BTreeMap<String, McpBoundEnvironmentSource>,
        enabled_tools: BTreeSet<String>,
        required: bool,
    ) -> Result<Self, McpBoundServerError> {
        let executable = executable.as_ref();
        let cwd = cwd.as_ref();
        if !executable.is_absolute() || !cwd.is_absolute() || enabled_tools.is_empty() {
            return Err(InvalidDefinition);
        }
        let arguments = arguments
            .into_iter()
            .map(crate::McpArgument::literal)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| InvalidDefinition)?;
        let mut exact_environment = BTreeMap::new();
        let mut bindings = McpCredentialBindings::new();
        for (name, source) in environment {
            let value = match source {
                McpBoundEnvironmentSource::Literal(value) => {
                    crate::McpEnvironmentValue::literal(value).map_err(|_| InvalidDefinition)?
                }
                McpBoundEnvironmentSource::Credential { reference, query } => {
                    bindings
                        .insert(
                            reference.clone(),
                            crate::McpCredentialBinding::new(query, McpCredentialEncoding::Raw),
                        )
                        .map_err(|_| InvalidDefinition)?;
                    crate::McpEnvironmentValue::credential(reference)
                }
            };
            exact_environment.insert(name, value);
        }
        let command = executable.to_str().ok_or(InvalidDefinition)?.to_owned();
        let transport =
            crate::McpStdioTransport::new(command, cwd.to_path_buf(), arguments, exact_environment)
                .map_err(|_| InvalidDefinition)?;
        let definition = provider_definition(
            id,
            display_name,
            McpTransportDefinition::Stdio(transport),
            enabled_tools,
            required,
        )?;
        Self::new(definition, bindings)
    }

    /// Build a provider-owned bearer-authenticated Streamable HTTP server
    /// with prompt-only exact tools and no non-tool exposure.
    ///
    /// # Errors
    /// Invalid identity/endpoint/policy or an incoherent reference/query.
    #[allow(clippy::too_many_arguments)]
    pub fn provider_streamable_http_bearer(
        id: impl Into<String>,
        display_name: impl Into<String>,
        endpoint: impl Into<String>,
        header_name: impl Into<String>,
        reference: crate::McpSecretReference,
        query: heycode_credentials::CredentialQuery,
        enabled_tools: BTreeSet<String>,
        required: bool,
    ) -> Result<Self, McpBoundServerError> {
        if enabled_tools.is_empty() {
            return Err(InvalidDefinition);
        }
        let transport = crate::McpStreamableHttpTransport::new(
            endpoint,
            BTreeMap::from([(header_name.into(), reference.clone())]),
        )
        .map_err(|_| InvalidDefinition)?;
        let definition = provider_definition(
            id,
            display_name,
            McpTransportDefinition::StreamableHttp(transport),
            enabled_tools,
            required,
        )?;
        let mut bindings = McpCredentialBindings::new();
        bindings
            .insert(
                reference,
                crate::McpCredentialBinding::new(query, McpCredentialEncoding::Bearer),
            )
            .map_err(|_| InvalidDefinition)?;
        Self::new(definition, bindings)
    }

    /// Attach one exact session/UI route for MCP11.
    ///
    /// # Errors
    /// The route belongs to another server id.
    pub fn with_client_events(
        mut self,
        client_events: McpClientEventRouter,
    ) -> Result<Self, McpBoundServerError> {
        if client_events.server() != self.definition.id() {
            return Err(RouteMismatch);
        }
        self.client_events = Some(client_events);
        Ok(self)
    }

    /// Exact private definition.
    #[must_use]
    pub const fn definition(&self) -> &McpServerDefinition {
        &self.definition
    }

    /// Safe credential-reference bindings.
    #[must_use]
    pub const fn bindings(&self) -> &McpCredentialBindings {
        &self.bindings
    }
}

fn provider_definition(
    id: impl Into<String>,
    display_name: impl Into<String>,
    transport: McpTransportDefinition,
    enabled_tools: BTreeSet<String>,
    required: bool,
) -> Result<McpServerDefinition, McpBoundServerError> {
    let policy = crate::McpToolPolicy::new(
        Some(enabled_tools),
        BTreeSet::new(),
        crate::McpApprovalMode::Prompt,
        BTreeMap::new(),
    )
    .map_err(|_| InvalidDefinition)?;
    McpServerDefinition::new(id, display_name, crate::McpDefinitionScope::User, transport)
        .map(|definition| {
            definition
                .with_required(required)
                .with_tool_policy(policy)
                .with_exposure(crate::McpExposurePolicy {
                    resources: false,
                    prompts: false,
                    instructions: false,
                })
        })
        .map_err(|_| InvalidDefinition)
}

/// Build one effect-owned plugin over ordinary MCP connection, generation and
/// rich-result owners.
///
/// # Errors
/// Duplicate server ids or invalid bound definitions.
pub fn mcp_bound_servers_plugin(
    plugin_id: &'static str,
    servers: Vec<McpBoundServer>,
    approval: Arc<dyn McpToolApprovalHandler>,
) -> Result<Box<dyn Plugin>, McpBoundServerError> {
    build_bound_servers_plugin(plugin_id, servers, approval, None, false)
}

/// Build the product-complete credential-aware MCP plugin.
///
/// Every enabled connection must carry one exact client-event router. The
/// lifecycle adapter surrounds each generated tool's actual server call.
///
/// # Errors
/// Duplicate ids, invalid definitions, or a missing enabled-server route.
pub fn mcp_bound_servers_product_plugin(
    plugin_id: &'static str,
    servers: Vec<McpBoundServer>,
    approval: Arc<dyn McpToolApprovalHandler>,
    lifecycle_hooks: Arc<dyn crate::McpLifecycleHookPort>,
) -> Result<Box<dyn Plugin>, McpBoundServerError> {
    build_bound_servers_plugin(plugin_id, servers, approval, Some(lifecycle_hooks), true)
}

fn build_bound_servers_plugin(
    plugin_id: &'static str,
    mut servers: Vec<McpBoundServer>,
    approval: Arc<dyn McpToolApprovalHandler>,
    lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
    require_routes: bool,
) -> Result<Box<dyn Plugin>, McpBoundServerError> {
    servers.sort_by(|left, right| left.definition.id().cmp(right.definition.id()));
    if servers
        .windows(2)
        .any(|pair| pair[0].definition.id() == pair[1].definition.id())
    {
        return Err(DuplicateServer);
    }
    if require_routes
        && servers
            .iter()
            .any(|server| server.definition.enabled() && server.client_events.is_none())
    {
        return Err(MissingRoute);
    }
    Ok(Box::new(BoundPlugin {
        plugin_id,
        servers,
        approval,
        lifecycle_hooks,
    }))
}

struct BoundPlugin {
    plugin_id: &'static str,
    servers: Vec<McpBoundServer>,
    approval: Arc<dyn McpToolApprovalHandler>,
    lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
}

impl Plugin for BoundPlugin {
    fn name(&self) -> &'static str {
        self.plugin_id
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            self.name(),
            env!("CARGO_PKG_VERSION"),
            &[
                heycode_core::PluginContributionKind::Tool,
                heycode_core::PluginContributionKind::ExternalProcess,
            ],
        )
    }

    fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
        self.servers
            .iter()
            .map(|server| {
                heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::ExternalProcess,
                    server.definition.id().as_str(),
                )
            })
            .collect()
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[
            heycode_tools::SERVICE_TOOLS,
            heycode_exec::SERVICE_SUBPROCESS,
            heycode_http::SERVICE_HTTP,
            heycode_credentials::SERVICE_CREDENTIALS,
            crate::SERVICE_MCP,
        ]
    }

    fn apply(&self, context: &mut Context) -> CoreResult<()> {
        let registry = context
            .get::<McpRegistry>(crate::SERVICE_MCP)
            .ok_or_else(|| CoreError::other("mcp registry service missing"))?;
        let tools = context
            .get::<ToolRegistry>(heycode_tools::SERVICE_TOOLS)
            .ok_or_else(|| CoreError::other("tools service missing"))?;
        let subprocess = context
            .get::<SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
            .ok_or_else(|| CoreError::other("subprocess service missing"))?;
        let http = context
            .get::<HttpService>(heycode_http::SERVICE_HTTP)
            .ok_or_else(|| CoreError::other("http service missing"))?;
        let credentials = context
            .get::<CredentialsService>(heycode_credentials::SERVICE_CREDENTIALS)
            .ok_or_else(|| CoreError::other("credentials service missing"))?;
        if self.servers.is_empty() {
            return Ok(());
        }
        let runtime_guard = DriverRuntime::new()?;
        let runtime = runtime_guard.handle()?;

        for server in &self.servers {
            registry
                .register_definition(context, server.definition.clone())
                .map_err(registry_core_error)?;
            if !server.definition.enabled() {
                continue;
            }
            let exact = registry
                .definition(server.definition.id())
                .map_err(registry_core_error)?
                .ok_or_else(|| CoreError::other("mcp definition disappeared"))?;
            let started_at_ms = unix_time_ms();
            let provider = match exact.transport() {
                McpTransportDefinition::Stdio(_) => "bound-stdio",
                McpTransportDefinition::StreamableHttp(_) => "bound-http",
            };
            let publisher = registry
                .register_connection(
                    context,
                    exact.id(),
                    McpConnectionProviderId::new(provider).map_err(registry_core_error)?,
                    started_at_ms,
                )
                .map_err(registry_core_error)?;
            let request = BoundRequest {
                definition: exact,
                bindings: server.bindings.clone(),
                client_events: server.client_events.clone(),
                approval: Arc::clone(&self.approval),
                lifecycle_hooks: self.lifecycle_hooks.clone(),
                tools: Arc::clone(&tools),
                subprocess: (*subprocess).clone(),
                http: (*http).clone(),
                credentials: (*credentials).clone(),
                publisher,
                runtime: Arc::clone(&runtime),
                started_at_ms,
            };
            let established = drive_on_plain_thread(&runtime, establish_bound(request));
            match established {
                Ok(established) => {
                    let names = established.generation.tool_names();
                    let shutdown_runtime = Arc::clone(&runtime);
                    context.effect(move || {
                        established.dispose(&shutdown_runtime);
                    });
                    for name in names {
                        context.contribute(heycode_core::ContributionKind::Tool, name)?;
                    }
                }
                Err(error) if !server.definition.required() => {
                    let _classified = error;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

struct BoundRequest {
    definition: Arc<McpServerDefinition>,
    bindings: McpCredentialBindings,
    client_events: Option<McpClientEventRouter>,
    approval: Arc<dyn McpToolApprovalHandler>,
    lifecycle_hooks: Option<Arc<dyn crate::McpLifecycleHookPort>>,
    tools: Arc<ToolRegistry>,
    subprocess: SubprocessService,
    http: HttpService,
    credentials: CredentialsService,
    publisher: crate::McpConnectionPublisher,
    runtime: Arc<tokio::runtime::Runtime>,
    started_at_ms: u64,
}

async fn establish_bound(request: BoundRequest) -> Result<EstablishedGeneration, McpError> {
    match request.definition.transport() {
        McpTransportDefinition::Stdio(_) => {
            establish_stdio_generation(StdioGenerationRequest {
                definition: request.definition,
                driver: request.runtime,
                subprocess: request.subprocess,
                tools: request.tools,
                publisher: request.publisher,
                started_at_ms: request.started_at_ms,
                approval: Some(request.approval),
                client_events: request.client_events,
                lifecycle_hooks: request.lifecycle_hooks,
                credentials: Some(request.credentials),
                credential_bindings: request.bindings,
            })
            .await
        }
        McpTransportDefinition::StreamableHttp(_) => {
            establish_http_generation(HttpGenerationRequest {
                definition: request.definition,
                http: request.http,
                tools: request.tools,
                publisher: request.publisher,
                started_at_ms: request.started_at_ms,
                approval: Some(request.approval),
                client_events: request.client_events,
                lifecycle_hooks: request.lifecycle_hooks,
                credentials: Some((request.credentials, request.bindings)),
            })
            .await
        }
    }
}

fn validate_bindings(
    definition: &McpServerDefinition,
    bindings: &McpCredentialBindings,
) -> Result<(), McpBoundServerError> {
    let required: BTreeSet<_> = definition
        .transport()
        .credential_references()
        .into_iter()
        .collect();
    let supplied: BTreeSet<_> = bindings.references().collect();
    if required
        .iter()
        .any(|reference| !bindings.contains(reference))
    {
        return Err(MissingBinding);
    }
    if supplied
        .iter()
        .any(|reference| !required.contains(reference))
    {
        return Err(ExtraBinding);
    }
    if matches!(definition.transport(), McpTransportDefinition::Stdio(_))
        && required.iter().any(|reference| {
            bindings
                .binding(reference)
                .is_some_and(|binding| binding.encoding() != McpCredentialEncoding::Raw)
        })
    {
        return Err(InvalidStdioEncoding);
    }
    Ok(())
}

/// Safe bound-server construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpBoundServerError {
    /// Exact definition reference has no binding.
    #[error("MCP bound server is missing a credential binding")]
    MissingBinding,
    /// Binding is not used by the exact definition.
    #[error("MCP bound server has an extra credential binding")]
    ExtraBinding,
    /// Stdio values must receive raw credentials.
    #[error("MCP stdio credential binding must use raw encoding")]
    InvalidStdioEncoding,
    /// MCP11 route belongs to another server.
    #[error("MCP client-event route belongs to another server")]
    RouteMismatch,
    /// Server ids collide inside one activation plugin.
    #[error("MCP bound server ids collide")]
    DuplicateServer,
    /// Product activation omitted a route for an enabled connection.
    #[error("MCP enabled server is missing its client-event route")]
    MissingRoute,
    /// Provider launch facts could not form one exact bound definition.
    #[error("MCP provider launch definition is invalid")]
    InvalidDefinition,
}
