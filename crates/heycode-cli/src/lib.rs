//! Composition root: config → plugin list → live world.
//!
//! The binary owns exactly one privilege: knowing every crate. Libraries
//! never reach up (AGENTS.md §2).

#[cfg(unix)]
pub mod config_import_integration;
/// Generated provider/model capability reference (DOC03).
pub mod docs_cli;
pub mod headless;
/// Shared production-loader composition harnesses for downstream tests.
pub mod mcp_cli;
pub mod plugin_cli;
mod provider_activation;
pub mod release_cli;
mod setup;
pub mod testing;
mod workspace_integration;

pub use setup::{
    SetupCatalog, SetupModelChoice, SetupModelChoices, SetupModelSource, SetupProviderChoice,
    SetupWorld, SetupWorldOptions, compose_setup_world, resolve_setup_model,
    resolve_setup_provider,
};

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, ApprovalPolicy, AutoApprove, CompactionPolicy, DenyAll, InteractiveApproval,
    RuntimeSubagentConfig, agent_attachments_plugin, agent_documents_plugin, agent_options_plugin,
    approval_plugin, commands_plugin, compactions_plugin, durable_schedules_plugin, goal_plugin,
    native_runtime_plugin, native_workflow_plugin, provider_telemetry_plugin, review_plugin,
    runtime_subagent_plugin, subagent_jobs_plugin, team_plugin, telemetry_metrics_plugin,
};
use heycode_app_server::{app_server_controls_plugin, app_server_plugin};
use heycode_attachments::{AttachmentStoreConfig, local_attachment_plugin};
use heycode_authorization::{AuthorizationFlowId, authorization_plugin};
use heycode_authorization_api_key::{
    ApiKeyAuthorizationFlow, ApiKeyFlowConfig, ApiKeyValidationFailure, ApiKeyValidator,
    HttpApiKeyValidator, InteractiveSecretPrompt, SERVICE_SECRET_PROMPT, SecretPrompt,
    api_key_authorization_plugin, secret_prompt_plugin,
};
use heycode_catalog_file::{FileCatalogConfig, file_catalog_persistence_plugin};
use heycode_config::{
    ApprovalMode, Config, ConfigMigrationNotice, EffectivePluginSelection, LlmProtocolCfg,
    PluginFactories, ProfileLayer, config_migration_doctor_plugin, named_profiles_plugin,
    resolve_profile_tree,
};
use heycode_core::{Context, Plugin, PluginActivationOutcome, PluginScope, ScopedPlugin};
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialsService,
    SERVICE_CREDENTIALS, credentials_doctor_plugin, credentials_plugin,
};
use heycode_credentials_command::{CommandCredentialProvider, command_credentials_plugin};
use heycode_credentials_env::{EnvironmentCredentialProvider, environment_credentials_plugin};
use heycode_credentials_file::{
    FileCredentialConfig, FileCredentialProvider, file_credentials_plugin,
};
use heycode_doctor::{
    DoctorRegistry, DoctorReport, SERVICE_DOCTOR, composition_doctor_plugin, doctor_plugin,
};
use heycode_exec::{LocalShellConfig, local_subprocess_plugin};
use heycode_http::{HttpService, SERVICE_HTTP, http_plugin};
use heycode_init::init_plugin;
use heycode_llm::{
    LlmSelection, Provider, ProviderRegistry, RouteCredential, llm_plugin, model_catalog_plugin,
    request_transforms_plugin,
};
use heycode_mcp::{McpServerConfig, McpServerSpec, mcp_registry_plugin};
use heycode_native_tools::{native_tool_policy_plugin, native_tools_plugin};
use heycode_provider_anthropic::{
    ANTHROPIC_API_KEY_REFERENCE, ANTHROPIC_CLAUDE_OPUS_5, AnthropicCatalogConfig,
    AnthropicConfiguredServerToolPolicy, AnthropicPluginConfig, AnthropicSettingsPolicies,
    anthropic_catalog_plugin, anthropic_configured_native_tools_plugin, anthropic_plugin,
};
use heycode_provider_deepseek::{DeepSeekCatalogConfig, deepseek_catalog_plugin};
use heycode_provider_openai::{
    OPENAI_API_KEY_REFERENCE, OPENAI_GPT_5_6_SOL, OpenAiCatalogConfig,
    OpenAiConfiguredHostedToolPolicy, OpenAiPluginConfig, openai_catalog_plugin,
    openai_configured_native_tools_plugin, openai_plugin, resolve_openai_prompt_cache_policy,
};
use heycode_provider_openrouter::{
    OpenRouterCatalogConfig, OpenRouterPluginConfig, openrouter_catalog_plugin,
    openrouter_native_tools_plugin, openrouter_plugin, openrouter_request_transforms_plugin,
};
use heycode_routing::routing_auth_plugin;
use heycode_runtime::runtime_registry_plugin;
use heycode_runtime_claude::{ClaudeRuntimeConfig, claude_runtime_plugin};
use heycode_runtime_codex::{
    CodexAppServerConfig, CodexClientInfo, codex_environment_snapshot, codex_runtime_plugin,
};
use heycode_runtime_deepseek_harness::{
    DeepSeekHarnessRuntimeConfig, deepseek_harness_runtime_plugin,
};
use heycode_runtime_opencode::{OpenCodeRuntimeConfig, opencode_runtime_plugin};
use heycode_settings::settings_doctor_plugin;
use heycode_settings_file::{FileSettingsConfig, FileSettingsProvider, file_settings_plugin};
use heycode_skills::skills_plugin;
use heycode_status::{
    context_status_plugin, health::health_history_plugin, status_plugin_with_config,
    web_status_plugin,
};
use heycode_tools::tools_plugin;
use heycode_trust::{WorkspaceTrustService, trust_service_plugin};
use heycode_ui::ui_registry_plugin;
use heycode_web::{
    PortableWebConfig, portable_web_plugin, web_extract_plugin, web_policy_plugin,
    web_registry_plugin,
};

/// Built-in plugin order before availability filtering.
///
/// This is the single current-profile source used by composition and config
/// migration previews. `sandbox` is mandatory; effective off mode has no OS backend.
pub const BUILTIN_PLUGIN_ORDER: &[&str] = &[
    "doctor",
    "doctor-config",
    "trust",
    "ui",
    "settings",
    "doctor-settings",
    "settings-aws-bedrock",
    "settings-google-inference",
    "http",
    "sandbox",
    "subprocess-local",
    "session",
    "workspace-scope",
    "terminal-registry",
    "shell-local",
    "hooks",
    "filesystem-local",
    "retained-output-local",
    "lsp-registry",
    "credentials",
    "credentials-env",
    "credentials-command",
    "credentials-file",
    "doctor-credentials",
    "authorization",
    "secret-prompt",
    "authorization-api-key",
    "provider-openrouter",
    "provider-anthropic",
    "provider-openai",
    "authorization-aws",
    "authorization-gcp",
    "provider-lmstudio",
    "onboarding",
    "profiles",
    "attachments-local",
    "session-query-jsonl",
    "prompt",
    "native-tools",
    "native-openai",
    "native-anthropic",
    "native-openrouter",
    "web",
    "web-portable",
    "web-extract",
    "web-policy",
    "tools",
    "interactive-tools",
    "lsp-tools",
    "native-tool-policy",
    "models",
    "provider-ollama",
    "runtimes",
    "runtime-claude",
    "runtime-codex",
    "runtime-opencode",
    "runtime-grok",
    "runtime-deepseek-harness",
    "mcp-registry",
    "mcp-management",
    "plugin-lifecycle",
    "telemetry-local-off",
    "telemetry-metrics",
    "catalog-cache-file",
    "catalog-overrides",
    "catalog-deepseek",
    "catalog-openrouter",
    "catalog-compatible",
    "catalog-ollama",
    "catalog-anthropic",
    "catalog-openai",
    "catalog-google",
    "catalog-google-vertex",
    "catalog-google-claude-vertex",
    "catalog-azure-openai",
    "catalog-custom-openai",
    "catalog-minimax",
    "catalog-minimax-token-plan",
    "catalog-zai",
    "catalog-zai-coding",
    "catalog-lmstudio",
    "catalog-bedrock",
    "catalog-bedrock-mantle",
    "token-counters",
    "token-count-anthropic",
    "llm",
    "inference-bedrock-converse",
    "inference-bedrock-mantle-responses",
    "inference-bedrock-mantle-messages",
    "inference-google-gemini",
    "inference-google-vertex",
    "inference-google-claude-vertex",
    "inference-azure-openai",
    "inference-custom-openai",
    "provider-activation",
    "request-transforms",
    "request-transforms-openrouter",
    "provider-telemetry",
    "agent-options",
    "approval",
    "commands",
    "lmstudio-control",
    "status",
    "status-context",
    "status-web",
    "health-history",
    "init",
    "skills",
    "mcp",
    "compactions",
    "subagent",
    "subagent-codex",
    "subagent-claude",
    "subagent-worktree-codex",
    "subagent-worktree-claude",
    "subagent-worktree-opencode",
    "product-extensions",
    "config-import-resources",
    "plan",
    "agent",
    "advisor",
    "product-hook-attachments",
    "subagent-jobs",
    "execution-jobs",
    "goals",
    "workflows",
    "schedules",
    "teams",
    "work",
    "reviewer",
    "deferred-tools",
    "code-mode",
    "loop-budget-settings",
    "agent-attachments",
    "agent-documents",
    "runtime-native",
    "app-server",
    "routing",
    "routing-auth",
    "app-server-controls",
    "workspace-transitions",
    "config-import",
    "memory-commands",
    "autocompact",
    "tui",
    "panel-commands",
];

/// Read only the persisted routing runtime before full composition.
///
/// The caller supplies an optional already-trusted project settings path.
/// Full route tuple and live-registry validation still occur in the routing
/// plugin; this early value controls only credential preflight/session metadata.
///
/// # Errors
/// Settings file admission/parsing or runtime-id validation failure.
pub fn requested_runtime_from_files(
    user_path: std::path::PathBuf,
    project_path: Option<std::path::PathBuf>,
) -> anyhow::Result<String> {
    let mut config = FileSettingsConfig::user(user_path).without_watch();
    if let Some(project_path) = project_path {
        config = config.with_project(project_path);
    }
    let documents = FileSettingsProvider::new(config).load_documents()?;
    Ok(heycode_routing::requested_runtime(&documents)?.unwrap_or_else(|| "native".to_owned()))
}

/// Read the durable connection-setup latch before full composition.
///
/// The latch is inspected in its owning user/project layers, so a project or
/// command-line route cannot mask a user-wide logout.
///
/// # Errors
/// Settings file admission/parsing or malformed latch state.
pub fn startup_requires_setup_from_files(
    user_path: std::path::PathBuf,
    project_path: Option<std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let mut config = FileSettingsConfig::user(user_path).without_watch();
    if let Some(project_path) = project_path {
        config = config.with_project(project_path);
    }
    let documents = FileSettingsProvider::new(config).load_documents()?;
    Ok(heycode_routing::requires_setup(&documents)?)
}

/// Apply persisted routing before constructing provider clients or checking credentials.
///
/// Command-line pins retain precedence. Crossing providers discards unpinned
/// endpoint, credential-reference and protocol values from the previous provider.
///
/// # Errors
/// Settings admission or persisted route validation failure.
pub fn apply_startup_connection_from_files(
    config: &mut Config,
    user_path: std::path::PathBuf,
    project_path: Option<std::path::PathBuf>,
) -> anyhow::Result<heycode_routing::RoutingSelection> {
    resolve_startup_connection_from_files(config, user_path, project_path)
        .map(|(selection, _, _)| selection)
}

fn resolve_startup_connection_from_files(
    config: &mut Config,
    user_path: std::path::PathBuf,
    project_path: Option<std::path::PathBuf>,
) -> anyhow::Result<(heycode_routing::RoutingSelection, bool, bool)> {
    let mut files = FileSettingsConfig::user(user_path).without_watch();
    if let Some(project) = project_path {
        files = files.with_project(project);
    }
    let documents = FileSettingsProvider::new(files).load_documents()?;
    let selection = apply_startup_connection(config, &documents)?;
    Ok((
        selection,
        heycode_routing::has_persisted_connection(&documents)?,
        heycode_routing::requires_setup(&documents)?,
    ))
}

fn apply_startup_connection(
    config: &mut Config,
    documents: &heycode_settings::SettingsDocuments,
) -> anyhow::Result<heycode_routing::RoutingSelection> {
    let base = heycode_routing::RoutingSelection::new(
        "native",
        &config.llm.provider,
        &config.llm.model,
        None,
    )?;
    let requested = heycode_routing::requested_connection(documents, &base)?;
    let namespace = heycode_settings::SettingsNamespace::new("routing")?;
    let field_origin = |field: &str| {
        [
            ("project", documents.project_section(&namespace)),
            ("user", documents.user_section(&namespace)),
        ]
        .into_iter()
        .find_map(|(layer, section)| {
            section
                .and_then(|section| section.get(field))
                .map(|value| (layer, value))
        })
    };
    let setup_required = heycode_routing::requires_setup(documents)?;
    let pending_origin = field_origin("pending_connection")
        .filter(|(_, value)| !value.is_null())
        .map(|(layer, _)| layer);
    let source = |field: &str| {
        if setup_required {
            return None;
        }
        let (layer, path) = if let Some(layer) = pending_origin {
            (layer, format!("routing.pending_connection.{field}"))
        } else {
            let (layer, _) = field_origin(field)?;
            (layer, format!("routing.{field}"))
        };
        Some(heycode_config::ConfigValueSource::Settings(format!(
            "{layer} settings ({path})"
        )))
    };
    if !config.is_patched("llm.provider") && config.llm.provider != requested.provider() {
        config.llm.provider = requested.provider().to_owned();
        if !config.is_patched("llm.base_url") {
            config.llm.base_url = None;
        }
        if !config.is_patched("llm.api_key_env") {
            config.llm.api_key_env = None;
        }
        if !config.is_patched("llm.protocol") {
            config.llm.protocol = LlmProtocolCfg::Auto;
            if let Some(origin) = source("provider") {
                config.set_effective_source("llm.protocol", origin);
            }
        }
    }
    if !config.is_patched("llm.provider")
        && let Some(origin) = source("provider")
    {
        config.set_effective_source("llm.provider", origin);
    }
    if !config.is_patched("llm.model") && config.llm.provider == requested.provider() {
        config.llm.model = requested.model().to_owned();
        if let Some(origin) = source("model") {
            config.set_effective_source("llm.model", origin);
        }
    }
    if config.llm.provider == requested.provider()
        && !config.is_patched("llm.base_url")
        && let Some(endpoint) = requested.endpoint()
    {
        config.llm.base_url = Some(endpoint.into());
        if let Some(origin) = source("endpoint") {
            config.set_effective_source("llm.base_url", origin);
        }
    }
    if config.llm.provider == requested.provider()
        && !config.is_patched("llm.api_key_env")
        && (requested.credential_reference().is_some()
            || (requested.endpoint().is_some() && !config.is_patched("llm.base_url")))
    {
        config.llm.api_key_env = requested
            .credential_reference()
            .map(|reference| reference.as_str().into());
        if let Some(origin) = source("credential_reference") {
            config.set_effective_source("llm.api_key_env", origin);
        }
    }
    Ok(requested)
}

/// Authoritative built-in service/seam key registry for diagnostics and drift
/// tests. Each key itself is owned by the crate defining the service type.
pub const BUILTIN_SERVICE_KEYS: &[heycode_core::ServiceKey] = &[
    SERVICE_DOCTOR,
    heycode_trust::SERVICE_TRUST,
    heycode_ui::SERVICE_UI,
    heycode_ui::SERVICE_SETTINGS_UI,
    heycode_settings::SERVICE_SETTINGS,
    SERVICE_HTTP,
    heycode_exec::SERVICE_SANDBOX,
    heycode_exec::SERVICE_SUBPROCESS,
    heycode_exec::SERVICE_TERMINAL,
    heycode_exec::SERVICE_SHELL,
    heycode_hooks::SERVICE_HOOKS,
    heycode_exec::SERVICE_FILESYSTEM,
    heycode_exec::SERVICE_RETAINED_OUTPUT,
    heycode_exec::SERVICE_LSP,
    heycode_credentials::SERVICE_CREDENTIALS,
    heycode_authorization::SERVICE_AUTHORIZATION,
    SERVICE_SECRET_PROMPT,
    heycode_authorization_aws::SERVICE_AWS_AUTH,
    heycode_authorization_gcp::SERVICE_GCP_AUTH,
    heycode_provider_lmstudio::SERVICE_LM_STUDIO,
    heycode_provider_lmstudio::SERVICE_LM_STUDIO_MODEL_CONTROL,
    heycode_provider_lmstudio::SERVICE_LM_STUDIO_MODELS,
    heycode_provider_lmstudio::SERVICE_OLLAMA_INSPECTOR,
    heycode_provider_lmstudio::SERVICE_OLLAMA_CATALOG,
    heycode_provider_lmstudio::SERVICE_OLLAMA_PROFILE,
    heycode_provider_lmstudio::SERVICE_OLLAMA_INFERENCE,
    heycode_onboarding::SERVICE_ONBOARDING,
    heycode_config::SERVICE_PROFILES,
    heycode_session::SERVICE_SESSION,
    heycode_agent::workspace_transition::SERVICE_WORKSPACE_TRANSITION,
    heycode_tui::memory_commands::SERVICE_MEMORY_SOURCES,
    heycode_attachments::SERVICE_ATTACHMENTS,
    heycode_session::SERVICE_SESSION_QUERY,
    heycode_prompt::SERVICE_PROMPT,
    heycode_native_tools::SERVICE_NATIVE_TOOLS,
    heycode_web::SERVICE_WEB,
    heycode_web::SERVICE_DOCUMENT_EXTRACTOR,
    heycode_llm::SERVICE_PROVIDERS,
    heycode_llm::SERVICE_LLM,
    heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
    heycode_llm::SERVICE_REQUEST_TRANSFORMS,
    heycode_llm::SERVICE_MODELS,
    heycode_catalog_file::SERVICE_CATALOG_OVERRIDES,
    heycode_runtime::SERVICE_RUNTIMES,
    heycode_mcp::SERVICE_MCP,
    heycode_mcp::SERVICE_MCP_MANAGEMENT,
    heycode_extensions::lifecycle::SERVICE_PLUGIN_LIFECYCLE,
    heycode_telemetry::SERVICE_TELEMETRY,
    heycode_tools::SERVICE_TOOLS,
    heycode_tools::SEAM_PRE_TOOL,
    heycode_agent::SERVICE_APPROVAL,
    heycode_agent::SERVICE_APPROVAL_INTERACTIVE,
    heycode_agent::SERVICE_APPROVAL_SWITCH,
    heycode_agent::SERVICE_COMMANDS,
    heycode_status::health::SERVICE_HEALTH_HISTORY,
    heycode_routing::SERVICE_ROUTING,
    heycode_agent::SERVICE_AGENT_OPTIONS,
    heycode_agent::SERVICE_COMPACTIONS,
    heycode_agent::SERVICE_AGENT,
    heycode_agent::SERVICE_ADVISOR,
    heycode_app_server::SERVICE_APP_SERVER,
    heycode_agent::SERVICE_PLAN,
    heycode_agent::SERVICE_SUBAGENTS,
    heycode_agent::SERVICE_JOBS,
    heycode_agent::SERVICE_EXECUTION_JOBS,
    heycode_agent::SERVICE_GOALS,
    heycode_agent::SERVICE_WORKFLOWS,
    heycode_agent::SERVICE_SCHEDULES,
    heycode_agent::SERVICE_TEAMS,
    heycode_agent::SERVICE_REVIEWS,
    heycode_llm::SERVICE_TOKEN_COUNTERS,
    heycode_skills::SERVICE_SKILLS,
    heycode_install::SERVICE_RELEASE_MANAGER,
    heycode_tui::SERVICE_TUI,
];

/// Resolve the owner-controlled heycode home directory from an absolute
/// `$HEYCODE_HOME` or the operating-system user home.
///
/// # Errors
/// A relative override or unavailable operating-system home fails loud. The
/// product never falls back to a project-relative `.heycode` authority root.
pub fn heycode_home() -> anyhow::Result<std::path::PathBuf> {
    heycode_config::home_root()?
        .ok_or_else(|| anyhow::anyhow!("operating-system user home is unavailable"))
}

/// Explicit policy for project content in a Restricted workspace.
///
/// Unknown blocks every project input. Restricted permits non-executable
/// instructions read-only, while project settings and executable authority
/// remain deferred. Trusted permits every current project input.
#[must_use]
pub const fn project_content_policy() -> heycode_trust::ProjectContentPolicy {
    heycode_trust::ProjectContentPolicy::new(
        heycode_trust::UntrustedProjectAccess::ReadOnly,
        heycode_trust::UntrustedProjectAccess::Block,
    )
}

/// Default sessions root under the heycode home.
pub fn sessions_dir() -> anyhow::Result<std::path::PathBuf> {
    Ok(heycode_home()?.join("sessions"))
}

/// Path of the credentials file (`KEY=value` lines, 0600).
///
/// # Errors
/// See [`heycode_home`].
pub fn credentials_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(heycode_home()?.join("credentials"))
}

/// Parse `KEY=value` lines from a credentials file. Absent file ⇒ empty map.
///
/// # Errors
/// A malformed line (missing `=` or empty key) fails loud with its 1-based
/// line number.
pub fn parse_credentials(path: &std::path::Path) -> anyhow::Result<Vec<(String, String)>> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(Vec::new()); // absent credentials file is normal on first run
    };
    let mut out = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            anyhow::bail!("{}:{idx}: expected KEY=value", path.display());
        };
        let key = key.trim();
        if key.is_empty() {
            anyhow::bail!("{}:{idx}: empty variable name", path.display());
        }
        out.push((key.to_owned(), value.trim().to_owned()));
    }
    Ok(out)
}

/// Resolve one credential through environment → file providers.
///
/// # Errors
/// Provider, migration, and registry failures.
pub fn lookup_credential(env_name: &str) -> anyhow::Result<Option<String>> {
    lookup_credential_at(env_name, &heycode_home()?)
}

/// Resolve one credential against an explicit root (isolated tests/embedders).
///
/// # Errors
/// Provider, migration, and registry failures.
pub fn lookup_credential_at(
    reference: &str,
    root: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    let query = api_key_query(reference)?;
    let (mut context, service) = bootstrap_credentials(root)?;
    let resolved = service
        .resolve(&query)?
        .map(|secret| secret.expose().to_owned());
    context.shutdown();
    Ok(resolved)
}

/// Persist through the authoritative/writable credential provider stack.
///
/// New and existing references use `root/credentials.toml`. Environment
/// shadowing fails loud; no native secret store is initialized.
///
/// # Errors
/// Provider, migration, registry, or shadow failures.
pub fn write_credential(reference: &str, value: &str) -> anyhow::Result<String> {
    let query = api_key_query(reference)?;
    let (mut context, service) = bootstrap_credentials(&heycode_home()?)?;
    let result = service.write(&query, &CredentialSecret::new(value));
    context.shutdown();
    Ok(result?.as_str().to_owned())
}

/// Persist through the credential stack rooted at `root` (the wizard's
/// explicit home; isolated tests).
///
/// # Errors
/// Provider, migration, registry, or shadow failures.
pub fn write_credential_at(
    reference: &str,
    value: &str,
    root: &std::path::Path,
) -> anyhow::Result<String> {
    let query = api_key_query(reference)?;
    let (mut context, service) = bootstrap_credentials(root)?;
    let result = service.write(&query, &CredentialSecret::new(value));
    context.shutdown();
    Ok(result?.as_str().to_owned())
}

/// Remove one stored credential from whichever writable provider holds it.
/// Returns the provider id, or `None` when nothing was stored.
///
/// # Errors
/// Provider/registry failures, or a read-only (environment) record shadowing
/// the reference.
pub fn delete_credential_at(
    reference: &str,
    root: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    let query = api_key_query(reference)?;
    let (mut context, service) = bootstrap_credentials(root)?;
    let result = service.delete(&query);
    context.shutdown();
    Ok(result?.map(|provider| provider.as_str().to_owned()))
}

/// Where the configured provider's credential comes from right now
/// (`environment`, `file`, …), for messages that must tell the
/// user *which* store holds the key they need to fix.
///
/// # Errors
/// Provider, migration, or registry failures.
pub fn credential_source_at(
    cfg: &Config,
    root: &std::path::Path,
) -> anyhow::Result<Option<heycode_credentials::CredentialSource>> {
    let reference = match cfg.llm.api_key_env.as_deref() {
        Some(reference) => reference,
        None => default_provider_reference(&cfg.llm.provider)?,
    };
    let query = api_key_query(reference)?;
    let (mut context, service) = bootstrap_credentials(root)?;
    let described = service.describe(&query);
    context.shutdown();
    Ok(described?.source)
}

/// Lower-case label for a credential source, for user-facing text.
#[must_use]
pub fn credential_source_label(
    source: Option<heycode_credentials::CredentialSource>,
) -> &'static str {
    match source {
        Some(heycode_credentials::CredentialSource::Environment) => "environment",
        Some(heycode_credentials::CredentialSource::Keychain) => "keychain",
        Some(heycode_credentials::CredentialSource::File) => "credentials file",
        Some(heycode_credentials::CredentialSource::Command) => "command",
        Some(heycode_credentials::CredentialSource::AmbientRuntime) => "ambient runtime",
        Some(heycode_credentials::CredentialSource::SubscriptionRuntime) => "subscription runtime",
        Some(_) | None => "unknown",
    }
}

/// Outcome of checking a freshly pasted key before it is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewKeyCheck {
    /// The provider accepted the key.
    Accepted,
    /// No reviewed live probe exists for this provider; the key is stored
    /// unverified and checked on first use.
    NotCheckable,
}

/// Check a pasted key against the provider (or its configured gateway)
/// before `heycode setup` stores it, so a typo is caught at the prompt instead of
/// on the first turn — and never persisted.
///
/// # Errors
/// [`ApiKeyValidationFailure`] from the live probe; `Network` means the key
/// could not be checked, not that it is wrong.
pub async fn check_new_key(
    provider: &str,
    base_url: Option<&str>,
    key: &str,
) -> Result<NewKeyCheck, ApiKeyValidationFailure> {
    let http = || {
        heycode_http::ReqwestHttpTransport::new()
            .map(|transport| HttpService::new(Arc::new(transport)))
            .map_err(|_| ApiKeyValidationFailure::Host)
    };
    // Each provider authenticates its own way — bearer for the OpenAI-shaped
    // routes, `x-api-key` for Anthropic, `x-goog-api-key` for Gemini — so the
    // check is the provider's own validator, never a guess.
    let validator: Arc<dyn ApiKeyValidator> = match (provider, base_url) {
        ("openrouter", Some(base_url)) => Arc::new(
            HttpApiKeyValidator::openrouter_at(base_url, None)
                .map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("openrouter", None) => Arc::new(
            HttpApiKeyValidator::openrouter(None).map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("deepseek", Some(base_url)) => Arc::new(
            HttpApiKeyValidator::deepseek_at(base_url, None)
                .map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("deepseek", None) => Arc::new(
            HttpApiKeyValidator::deepseek(None).map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("openai", base_url) => Arc::new(
            match base_url {
                Some(base_url) => heycode_provider_openai::OpenAiApiKeyValidator::with_base_url(
                    http()?,
                    base_url,
                    None,
                ),
                None => heycode_provider_openai::OpenAiApiKeyValidator::new(http()?, None),
            }
            .map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("anthropic", base_url) => Arc::new(
            match base_url {
                Some(base_url) => {
                    heycode_provider_anthropic::AnthropicApiKeyValidator::with_base_url(
                        http()?,
                        base_url,
                        None,
                    )
                }
                None => heycode_provider_anthropic::AnthropicApiKeyValidator::new(http()?, None),
            }
            .map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        ("google", base_url) => Arc::new(
            match base_url {
                Some(base_url) => heycode_provider_google::GoogleApiKeyValidator::with_transport(
                    http()?,
                    base_url,
                ),
                None => heycode_provider_google::GoogleApiKeyValidator::new(),
            }
            .map_err(|_| ApiKeyValidationFailure::Host)?,
        ),
        _ => return Ok(NewKeyCheck::NotCheckable),
    };
    validator
        .validate(
            &CredentialSecret::new(key),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    Ok(NewKeyCheck::Accepted)
}

/// Whether the configured provider has any key reachable right now.
///
/// # Errors
/// Propagates credential-file parse failures.
pub fn provider_key_present(cfg: &Config) -> anyhow::Result<bool> {
    provider_key_present_at(cfg, &heycode_home()?)
}

/// Whether the configured provider has a credential reachable under an
/// explicit isolated root.
///
/// The configured reference is authoritative. Provider defaults are used
/// only when configuration did not select a custom reference.
///
/// # Errors
/// Provider, migration, registry, or credential-file failures.
pub fn provider_key_present_at(cfg: &Config, root: &std::path::Path) -> anyhow::Result<bool> {
    if !provider_uses_credential(cfg) {
        return Ok(true);
    }
    let reference = match cfg.llm.api_key_env.as_deref() {
        Some(reference) => reference,
        None => match default_provider_reference(&cfg.llm.provider) {
            Ok(reference) => reference,
            Err(_) => return Ok(false),
        },
    };
    Ok(lookup_credential_at(reference, root)?.is_some())
}

/// Whether this inference provider requires the credential startup lane.
#[must_use]
pub fn provider_requires_credential(provider: &str) -> bool {
    !matches!(
        provider,
        heycode_provider_lmstudio::OLLAMA_PROVIDER
            | heycode_provider_lmstudio::LM_STUDIO_PROVIDER
            | heycode_provider_openai_compatible::CUSTOM_OPENAI_PROVIDER
    )
}

/// Whether startup must resolve a credential for this exact configured route.
///
/// An optional-auth provider does not require a key by default, but an
/// explicitly retained reference is authoritative and must not silently fall
/// back to unauthenticated dispatch when its value is missing.
#[must_use]
pub fn provider_uses_credential(cfg: &Config) -> bool {
    provider_requires_credential(&cfg.llm.provider) || cfg.llm.api_key_env.is_some()
}

/// Whether startup has a reviewed credential-only live validator for this provider.
#[must_use]
pub fn provider_has_preflight_validation(provider: &str) -> bool {
    matches!(provider, "deepseek" | "openrouter")
}

/// The startup credential probe for the configured provider, aimed at
/// `llm.base_url` when one is configured: a key issued for a proxy or gateway
/// must never be sent to the official host to be "checked".
///
/// # Errors
/// A provider without a reviewed preflight probe, or an unusable base URL,
/// classifies as [`ApiKeyValidationFailure::Host`].
pub fn preflight_validator(cfg: &Config) -> Result<HttpApiKeyValidator, ApiKeyValidationFailure> {
    let model = Some(cfg.llm.model.clone());
    match (cfg.llm.provider.as_str(), cfg.llm.base_url.as_deref()) {
        ("openrouter", Some(base_url)) => HttpApiKeyValidator::openrouter_at(base_url, model),
        ("openrouter", None) => HttpApiKeyValidator::openrouter(model),
        ("deepseek", Some(base_url)) => HttpApiKeyValidator::deepseek_at(base_url, model),
        ("deepseek", None) => HttpApiKeyValidator::deepseek(model),
        _ => return Err(ApiKeyValidationFailure::Host),
    }
    .map_err(|_| ApiKeyValidationFailure::Host)
}

/// Validate the configured provider credential before normal use.
///
/// Returns the safe validation timestamp for seeding the composed cache.
pub async fn validate_provider_credential(cfg: &Config) -> Result<u64, ApiKeyValidationFailure> {
    let reference = match cfg.llm.api_key_env.as_deref() {
        Some(reference) => reference,
        None => default_provider_reference(&cfg.llm.provider)
            .map_err(|_| ApiKeyValidationFailure::Host)?,
    };
    let secret = lookup_credential(reference)
        .map_err(|_| ApiKeyValidationFailure::Network)?
        .ok_or(ApiKeyValidationFailure::Unauthorized)?;
    let secret = CredentialSecret::new(secret);
    preflight_validator(cfg)?
        .validate(&secret, tokio_util::sync::CancellationToken::new())
        .await?;
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0))
}

/// The newest resumable session log under `root`, ordered by its latest
/// committed durable event rather than mutable filesystem timestamps.
///
/// # Errors
/// Unsafe/corrupt entries or an unreadable store fail loud.
pub fn find_latest_session(
    root: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, heycode_session::SessionQueryError> {
    find_latest_session_matching(root, heycode_session::SessionFilter::new())
}

/// Last active conversation in one canonical working directory.
/// Empty startup logs and sessions in other folders are excluded.
///
/// # Errors
/// An invalid workspace or an unreadable store fails explicitly.
pub fn find_latest_session_in(
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, heycode_session::SessionQueryError> {
    let cwd = std::fs::canonicalize(cwd)
        .map_err(|_| heycode_session::SessionQueryError::StoreUnavailable)?;
    let filter = heycode_session::SessionFilter::new()
        .with_used_only()
        .with_cwd(cwd)?;
    find_latest_session_matching(root, filter)
}

fn find_latest_session_matching(
    root: &std::path::Path,
    filter: heycode_session::SessionFilter,
) -> Result<Option<std::path::PathBuf>, heycode_session::SessionQueryError> {
    if !root
        .try_exists()
        .map_err(|_| heycode_session::SessionQueryError::StoreUnavailable)?
    {
        return Ok(None);
    }
    let service = heycode_session::SessionQueryService::local(root.to_path_buf());
    // `-c` continues the newest session that can actually be opened. An
    // unreadable row (a crash-truncated or corrupt log) is never a target.
    Ok(service
        .latest(&filter)?
        .filter(heycode_session::SessionSummary::is_readable)
        .map(|summary| root.join(summary.id().as_str()).join("session.jsonl")))
}

fn provider_error(provider: &str, env: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "no API key for `{provider}` — run `heycode setup`, export {env}, or use --fake"
    )
}

/// Build the DeepSeek provider via the env→credentials ladder.
///
/// # Errors
/// Missing key or client-construction failures.
pub fn deepseek_provider(model: Option<&str>) -> Result<Arc<dyn Provider>, anyhow::Error> {
    let key = lookup_credential(heycode_llm::DeepSeekProvider::API_KEY_ENV)?
        .ok_or_else(|| provider_error("deepseek", heycode_llm::DeepSeekProvider::API_KEY_ENV))?;
    Ok(Arc::new(heycode_llm::DeepSeekProvider::from_key(
        key,
        model.map(str::to_owned),
    )?))
}

/// Build the OpenRouter provider via the env→credentials ladder.
///
/// # Errors
/// Missing key or client-construction failures.
pub fn openrouter_provider(model: Option<&str>) -> Result<Arc<dyn Provider>, anyhow::Error> {
    let key =
        lookup_credential(heycode_llm::OpenRouterProvider::API_KEY_ENV)?.ok_or_else(|| {
            provider_error("openrouter", heycode_llm::OpenRouterProvider::API_KEY_ENV)
        })?;
    Ok(Arc::new(heycode_llm::OpenRouterProvider::from_key(
        key,
        model.map(str::to_owned),
        vec![openrouter_transform_option()?],
    )?))
}

fn openrouter_transform_option() -> anyhow::Result<heycode_core::ProviderRequestOption> {
    Ok(
        heycode_provider_openrouter::OpenRouterTransformPolicy::all_disabled().provider_option(
            heycode_provider_openrouter::OpenRouterTransformRequestContext::new(true, false),
        )?,
    )
}

fn api_key_query(reference: &str) -> anyhow::Result<CredentialQuery> {
    Ok(CredentialQuery::new(
        CredentialReference::new(reference)?,
        CredentialKind::new("api-key")?,
    ))
}

fn provider_credential_query(provider: &str, reference: &str) -> anyhow::Result<CredentialQuery> {
    if matches!(provider, "minimax" | "minimax-token-plan")
        && reference != default_provider_reference(provider)?
    {
        anyhow::bail!(
            "MiniMax requires its plan-owned credential reference; configure {}",
            default_provider_reference(provider)?
        );
    }
    if provider == "zai" && reference == heycode_provider_zai::ZAI_CODING_API_KEY_REFERENCE {
        anyhow::bail!("Z.ai general inference cannot use the Coding Plan credential reference");
    }
    let kind = if provider == "minimax-token-plan" {
        "subscription-key"
    } else if matches!(provider, "vertex-google" | "vertex-claude") {
        "oauth-token"
    } else {
        "api-key"
    };
    Ok(CredentialQuery::new(
        CredentialReference::new(reference)?,
        CredentialKind::new(kind)?,
    ))
}

fn bootstrap_credentials(root: &std::path::Path) -> anyhow::Result<(Context, CredentialsService)> {
    let context = Context::new();
    let service = CredentialsService::new();
    service.register(
        &context,
        Arc::new(EnvironmentCredentialProvider::process()?),
    )?;
    service.register(
        &context,
        Arc::new(FileCredentialProvider::open(FileCredentialConfig::new(
            root,
        ))?),
    )?;
    Ok((context, service))
}

/// Providers with a live inference route, and providers heycode knows about but
/// cannot yet run a turn against.
///
/// The distinction matters to the message a user sees. Several providers ship a
/// profile, an authorization flow and a model catalog without a `Provider`
/// implementation — they are discoverable and configurable, and selecting one
/// as `[llm] provider` still cannot work. Telling that user their provider is
/// "unknown" is false and sends them looking for a typo.
pub(crate) const INFERENCE_PROVIDERS: &[&str] = &[
    "anthropic",
    "bedrock",
    "bedrock-mantle",
    "deepseek",
    "fireworks",
    "groq",
    "mistral",
    "together",
    "xai",
    "google",
    "openai",
    "openrouter",
    "ollama",
    "lmstudio",
    "vertex-claude",
    "vertex-google",
    "azure-openai",
    "custom-openai",
    "minimax",
    "minimax-token-plan",
    "zai",
];

/// Known to heycode (profile, catalog or authorization flow) but unavailable for
/// direct inference, so not selectable as `[llm] provider`.
pub(crate) const CONFIGURED_ONLY_PROVIDERS: &[&str] = &["zai-coding"];

/// Explain a provider heycode cannot run inference against.
fn unselectable_provider(provider: &str) -> String {
    let available = INFERENCE_PROVIDERS.join(", ");
    if provider == "zai-coding" {
        return format!(
            "provider `zai-coding` is restricted by Z.ai to officially supported tools; heycode is not listed. Use `zai` with a separate general API key. See https://docs.z.ai/devpack/usage-policy — available for inference: {available}"
        );
    }
    if CONFIGURED_ONLY_PROVIDERS.contains(&provider) {
        format!(
            "provider `{provider}` has a profile and catalog but no inference route yet, \
             so it cannot be selected as `[llm] provider` — available for inference: {available}"
        )
    } else {
        format!("unknown provider `{provider}` — available for inference: {available}")
    }
}

/// The credential reference (environment variable name) a provider reads when
/// `llm.api_key_env` is not set.
///
/// # Errors
/// A provider without a credential reference.
pub fn default_provider_reference(provider: &str) -> anyhow::Result<&'static str> {
    if let Some(spec) = heycode_provider_compatible::spec(provider) {
        return Ok(spec.credential_reference);
    }
    match provider {
        "minimax" => Ok("MINIMAX_API_KEY"),
        "minimax-token-plan" => Ok("MINIMAX_TOKEN_PLAN_KEY"),
        "zai" => Ok(heycode_provider_zai::ZAI_GENERAL_API_KEY_REFERENCE),
        "lmstudio" => Ok("LM_STUDIO_API_KEY"),
        "anthropic" => Ok(ANTHROPIC_API_KEY_REFERENCE),
        "deepseek" => Ok(heycode_llm::DeepSeekProvider::API_KEY_ENV),
        "openai" => Ok(OPENAI_API_KEY_REFERENCE),
        "openrouter" => Ok(heycode_llm::OpenRouterProvider::API_KEY_ENV),
        "google" => Ok(heycode_provider_google::GOOGLE_API_KEY_REFERENCE),
        "bedrock" | "bedrock-mantle" => {
            Ok(heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE)
        }
        "vertex-google" | "vertex-claude" => {
            Ok(heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE)
        }
        "azure-openai" => Ok(heycode_provider_azure::AZURE_OPENAI_API_KEY_REFERENCE),
        other => Err(anyhow::anyhow!(unselectable_provider(other))),
    }
}

fn default_deepseek_api_key_flows(
    cfg: &Config,
    prompt: Arc<dyn SecretPrompt>,
) -> anyhow::Result<Vec<ApiKeyAuthorizationFlow>> {
    let deepseek_reference = if cfg.llm.provider == "deepseek" {
        cfg.llm
            .api_key_env
            .clone()
            .unwrap_or_else(|| heycode_llm::DeepSeekProvider::API_KEY_ENV.to_owned())
    } else {
        heycode_llm::DeepSeekProvider::API_KEY_ENV.to_owned()
    };
    Ok(vec![ApiKeyAuthorizationFlow::new(
        ApiKeyFlowConfig {
            id: AuthorizationFlowId::new("deepseek-api-key")?,
            label: "DeepSeek API key".to_owned(),
            query: api_key_query(&deepseek_reference)?,
            prompt: "Paste your DeepSeek API key".to_owned(),
        },
        prompt,
        Arc::new(match cfg.llm.base_url.as_deref() {
            Some(base_url) if cfg.llm.provider == "deepseek" => {
                HttpApiKeyValidator::deepseek_at(base_url, Some(cfg.llm.model.clone()))?
            }
            _ => HttpApiKeyValidator::deepseek(
                (cfg.llm.provider == "deepseek").then(|| cfg.llm.model.clone()),
            )?,
        }),
    )])
}

struct DisconnectedProvider {
    name: String,
    model: String,
}

impl Provider for DisconnectedProvider {
    fn info(&self) -> heycode_llm::ProviderInfo {
        heycode_llm::ProviderInfo {
            name: self.name.clone(),
            default_model: self.model.clone(),
        }
    }

    fn stream(&self, _request: heycode_llm::ChatRequest) -> heycode_llm::ChunkStream {
        Box::pin(futures::stream::iter([Err(
            heycode_llm::LlmError::Transport(
                "provider is not connected; finish onboarding first".to_owned(),
            ),
        )]))
    }
}

fn credential_llm_plugin(
    config: heycode_config::LlmSection,
    reference: String,
    allow_unconfigured: bool,
    validated_at_ms: Option<u64>,
) -> Box<dyn Plugin> {
    let provider_name = config.provider;
    let model = config.model;
    let reference_explicit = config.api_key_env.is_some();
    let protocol = config.protocol;
    let base_url = config.base_url;
    struct CredentialLlmPlugin {
        provider_name: String,
        model: String,
        reference: String,
        reference_explicit: bool,
        protocol: LlmProtocolCfg,
        allow_unconfigured: bool,
        validated_at_ms: Option<u64>,
        /// `llm.base_url`: every request for this provider — inference,
        /// validation, catalog — goes here instead of the official host.
        base_url: Option<String>,
    }
    impl Plugin for CredentialLlmPlugin {
        fn name(&self) -> &'static str {
            "llm"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "llm",
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut rows = Vec::new();
            if !externally_owned_inference(&self.provider_name) {
                rows.push(heycode_core::PluginContributionSpec::new(
                    heycode_core::ContributionKind::InferenceProvider,
                    self.provider_name.clone(),
                ));
            }
            rows.extend(heycode_llm::provider_interception_inventory());
            rows
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_LLM,
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            if externally_owned_inference(&self.provider_name) {
                &[]
            } else if matches!(self.provider_name.as_str(), "anthropic" | "openai") {
                &[
                    SERVICE_CREDENTIALS,
                    SERVICE_HTTP,
                    heycode_settings::SERVICE_SETTINGS,
                ]
            } else {
                &[SERVICE_CREDENTIALS, SERVICE_HTTP]
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), heycode_core::CoreError> {
            if externally_owned_inference(&self.provider_name) {
                context.provide(
                    heycode_llm::SERVICE_PROVIDERS,
                    "llm",
                    ProviderRegistry::new(),
                )?;
                context.provide(
                    heycode_llm::SERVICE_LLM,
                    "llm",
                    LlmSelection {
                        provider_name: self.provider_name.clone(),
                        model: self.model.clone(),
                    },
                )?;
                return context.provide(
                    heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                    "llm",
                    heycode_llm::ProviderInterception::default(),
                );
            }
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| heycode_core::CoreError::other("credentials missing"))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| heycode_core::CoreError::other("http transport missing"))?;
            let query = provider_credential_query(&self.provider_name, &self.reference)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let secret = credentials
                .resolve(&query)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            if let (Some(secret), Some(checked_at_ms)) = (secret.as_ref(), self.validated_at_ms) {
                let descriptor = credentials
                    .describe(&query)
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                let provider = descriptor.provider.ok_or_else(|| {
                    heycode_core::CoreError::other("validated credential has no active provider")
                })?;
                credentials
                    .record_validation(
                        &query,
                        &provider,
                        heycode_credentials::CredentialValidation::Valid { checked_at_ms },
                        secret,
                        15 * 60 * 1_000,
                    )
                    .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            }
            let configured =
                secret.is_some() || (self.provider_name == "lmstudio" && !self.reference_explicit);
            drop(secret);
            let provider: Arc<dyn Provider> = if configured {
                provider_activation::build_credential_provider(
                    &provider_activation::CredentialRoute {
                        provider_name: self.provider_name.clone(),
                        model: self.model.clone(),
                        reference: self.reference.clone(),
                        reference_explicit: self.reference_explicit,
                        protocol: self.protocol,
                        base_url: self.base_url.clone(),
                    },
                    &http,
                    &credentials,
                    context
                        .get::<heycode_settings::SettingsService>(
                            heycode_settings::SERVICE_SETTINGS,
                        )
                        .as_deref(),
                )?
            } else if self.allow_unconfigured {
                Arc::new(DisconnectedProvider {
                    name: default_provider_name(&self.provider_name)
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?
                        .to_owned(),
                    model: self.model.clone(),
                })
            } else {
                return Err(heycode_core::CoreError::other(
                    provider_error(&self.provider_name, &self.reference).to_string(),
                ));
            };
            let registry = ProviderRegistry::new();
            registry
                .register(provider)
                .map_err(heycode_core::CoreError::DuplicatePlugin)?;
            context.provide(heycode_llm::SERVICE_PROVIDERS, "llm", registry)?;
            context.provide(
                heycode_llm::SERVICE_LLM,
                "llm",
                LlmSelection {
                    provider_name: self.provider_name.clone(),
                    model: self.model.clone(),
                },
            )?;
            context.provide(
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                "llm",
                heycode_llm::ProviderInterception::default(),
            )
        }
    }
    Box::new(CredentialLlmPlugin {
        provider_name,
        model,
        reference,
        reference_explicit,
        protocol,
        allow_unconfigured,
        validated_at_ms,
        base_url,
    })
}

fn lmstudio_config_for_route(
    cfg: &Config,
) -> anyhow::Result<heycode_provider_lmstudio::LmStudioConfig> {
    let mut config = heycode_provider_lmstudio::LmStudioConfig::local();
    if cfg.llm.provider == "lmstudio" {
        if let Some(endpoint) = cfg.llm.base_url.as_ref() {
            config =
                config.with_endpoint(heycode_provider_lmstudio::LmStudioEndpoint::new(endpoint)?);
        }
        if let Some(reference) = cfg.llm.api_key_env.as_deref() {
            config = config.with_bearer_token(api_key_query(reference)?);
        }
    }
    Ok(config)
}

fn ollama_llm_plugin(model: String) -> Box<dyn Plugin> {
    struct OllamaLlmPlugin(String);

    impl Plugin for OllamaLlmPlugin {
        fn name(&self) -> &'static str {
            "llm"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Provider,
                    heycode_core::PluginContributionKind::Waterfall,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            let mut rows = vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::InferenceProvider,
                heycode_provider_lmstudio::OLLAMA_PROVIDER,
            )];
            rows.extend(heycode_llm::provider_interception_inventory());
            rows
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_LLM,
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
            ]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_provider_lmstudio::SERVICE_OLLAMA_INFERENCE]
        }

        fn apply(&self, context: &mut Context) -> Result<(), heycode_core::CoreError> {
            let inference = context
                .get::<heycode_provider_lmstudio::OllamaInference>(
                    heycode_provider_lmstudio::SERVICE_OLLAMA_INFERENCE,
                )
                .ok_or_else(|| {
                    heycode_core::CoreError::other("Ollama inference service missing")
                })?;
            if inference.info().default_model != self.0 {
                return Err(heycode_core::CoreError::other(
                    "Ollama inference profile does not match the selected model",
                ));
            }
            let provider: Arc<dyn Provider> = inference;
            let registry = ProviderRegistry::new();
            registry
                .register(provider)
                .map_err(heycode_core::CoreError::DuplicatePlugin)?;
            context.provide(heycode_llm::SERVICE_PROVIDERS, self.name(), registry)?;
            context.provide(
                heycode_llm::SERVICE_LLM,
                self.name(),
                LlmSelection {
                    provider_name: heycode_provider_lmstudio::OLLAMA_PROVIDER.to_owned(),
                    model: self.0.clone(),
                },
            )?;
            context.provide(
                heycode_llm::SERVICE_PROVIDER_INTERCEPTION,
                self.name(),
                heycode_llm::ProviderInterception::default(),
            )
        }
    }

    Box::new(OllamaLlmPlugin(model))
}

fn default_provider_name(provider: &str) -> anyhow::Result<&'static str> {
    if let Some(spec) = heycode_provider_compatible::spec(provider) {
        return Ok(spec.id);
    }
    match provider {
        "anthropic" => Ok("anthropic"),
        "deepseek" => Ok("deepseek"),
        "openai" => Ok("openai"),
        "openrouter" => Ok("openrouter"),
        "ollama" => Ok("ollama"),
        "lmstudio" => Ok("lmstudio"),
        "minimax" => Ok("minimax"),
        "minimax-token-plan" => Ok("minimax-token-plan"),
        "zai" => Ok("zai"),
        "google" => Ok("google"),
        "bedrock" => Ok("bedrock"),
        "bedrock-mantle" => Ok("bedrock-mantle"),
        "vertex-google" => Ok("vertex-google"),
        "vertex-claude" => Ok("vertex-claude"),
        "azure-openai" => Ok("azure-openai"),
        "custom-openai" => Ok("custom-openai"),
        other => Err(anyhow::anyhow!(unselectable_provider(other))),
    }
}

fn externally_owned_inference(provider: &str) -> bool {
    matches!(
        provider,
        "bedrock"
            | "bedrock-mantle"
            | "google"
            | "vertex-google"
            | "vertex-claude"
            | "azure-openai"
            | "custom-openai"
    )
}

fn validate_llm_protocol(provider: &str, protocol: LlmProtocolCfg) -> anyhow::Result<()> {
    let valid = match provider {
        provider if heycode_provider_compatible::spec(provider).is_some() => {
            matches!(protocol, LlmProtocolCfg::Auto | LlmProtocolCfg::OpenAiChat)
        }
        "custom-openai" | "minimax" | "minimax-token-plan" | "zai" => {
            matches!(protocol, LlmProtocolCfg::Auto | LlmProtocolCfg::OpenAiChat)
        }
        "deepseek" => matches!(
            protocol,
            LlmProtocolCfg::Auto | LlmProtocolCfg::OpenAiChat | LlmProtocolCfg::AnthropicMessages
        ),
        "bedrock-mantle" => matches!(
            protocol,
            LlmProtocolCfg::OpenAiResponses | LlmProtocolCfg::AnthropicMessages
        ),
        _ => protocol == LlmProtocolCfg::Auto,
    };
    if valid {
        Ok(())
    } else if provider == "bedrock-mantle" && protocol == LlmProtocolCfg::Auto {
        Err(anyhow::anyhow!(
            "Bedrock Mantle requires explicit [llm].protocol = openai_responses or anthropic_messages"
        ))
    } else {
        Err(anyhow::anyhow!(
            "[llm].protocol `{protocol}` is incompatible with provider `{provider}`"
        ))
    }
}

fn aws_inference_plugin_for_config(
    cfg: &Config,
    region: Option<heycode_authorization_aws::AwsRegion>,
) -> anyhow::Result<Option<Box<dyn Plugin>>> {
    if !matches!(cfg.llm.provider.as_str(), "bedrock" | "bedrock-mantle") {
        return Ok(None);
    }
    let region = region.ok_or_else(|| anyhow::anyhow!("AWS inference region is unresolved"))?;
    let reference = cfg
        .llm
        .api_key_env
        .as_deref()
        .unwrap_or(heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE);
    let credential = provider_credential_query(&cfg.llm.provider, reference)?;
    let plugin = match (cfg.llm.provider.as_str(), cfg.llm.protocol) {
        ("bedrock", LlmProtocolCfg::Auto) => {
            heycode_provider_aws::aws_converse_live_settings_plugin(
                region,
                credential,
                cfg.llm.model.clone(),
                None,
            )?
        }
        ("bedrock-mantle", LlmProtocolCfg::OpenAiResponses) => {
            heycode_provider_aws::aws_inference_plugin(
                heycode_provider_aws::AwsInferencePluginConfig::mantle_responses_live(
                    region,
                    credential,
                    cfg.llm.model.clone(),
                )?,
            )
        }
        ("bedrock-mantle", LlmProtocolCfg::AnthropicMessages) => {
            let max_output_tokens = cfg.llm.max_output_tokens.ok_or_else(|| {
                anyhow::anyhow!("Bedrock Mantle Messages requires explicit [llm].max_output_tokens")
            })?;
            heycode_provider_aws::aws_inference_plugin(
                heycode_provider_aws::AwsInferencePluginConfig::mantle_messages_live(
                    region,
                    credential,
                    cfg.llm.model.clone(),
                    Some(max_output_tokens),
                )?,
            )
        }
        _ => {
            return Err(anyhow::anyhow!(
                "configured AWS provider/protocol combination is unavailable"
            ));
        }
    };
    Ok(Some(plugin))
}

fn connection_parameters(
    cfg: &Config,
    selection: Option<&heycode_routing::RoutingSelection>,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let Some(selection) = selection.filter(|selection| selection.provider() == cfg.llm.provider)
    else {
        return Ok(std::collections::BTreeMap::new());
    };
    let allowed: &[&str] = match selection.provider() {
        "bedrock" | "bedrock-mantle" => &["region"],
        "vertex-google" | "vertex-claude" => &["project", "location"],
        "azure-openai" => &["resource", "deployment"],
        _ => &[],
    };
    if selection
        .parameters()
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(anyhow::anyhow!(
            "saved connection contains coordinates unsupported by its provider"
        ));
    }
    Ok(selection.parameters().clone())
}

#[cfg(test)]
fn explicit_gcp_profile_request(
    environment: &dyn heycode_authorization_gcp::GcpEnvironment,
) -> anyhow::Result<heycode_authorization_gcp::GcpProfileRequest> {
    gcp_profile_request(environment, &std::collections::BTreeMap::new())
}

fn gcp_profile_request(
    environment: &dyn heycode_authorization_gcp::GcpEnvironment,
    parameters: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<heycode_authorization_gcp::GcpProfileRequest> {
    let project = parameters
        .get("project")
        .cloned()
        .or_else(|| environment.var(heycode_authorization_gcp::ENV_GOOGLE_CLOUD_PROJECT))
        .ok_or_else(|| anyhow::anyhow!("Vertex inference requires GOOGLE_CLOUD_PROJECT"))?;
    let location = parameters
        .get("location")
        .cloned()
        .or_else(|| environment.var(heycode_authorization_gcp::ENV_GOOGLE_CLOUD_LOCATION))
        .ok_or_else(|| anyhow::anyhow!("Vertex inference requires GOOGLE_CLOUD_LOCATION"))?;
    Ok(heycode_authorization_gcp::GcpProfileRequest {
        project: Some(project),
        location: Some(location),
        ..heycode_authorization_gcp::GcpProfileRequest::default()
    })
}

fn azure_connection(
    parameters: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<(
    heycode_provider_azure::AzureResourceName,
    heycode_provider_azure::AzureDeploymentName,
)> {
    if parameters.len() != 2 {
        return Err(anyhow::anyhow!(
            "Azure OpenAI inference requires saved resource and deployment coordinates"
        ));
    }
    let resource = parameters
        .get("resource")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Azure OpenAI inference requires a saved resource"))?;
    let deployment = parameters
        .get("deployment")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Azure OpenAI inference requires a saved deployment"))?;
    Ok((
        heycode_provider_azure::AzureResourceName::new(resource)?,
        heycode_provider_azure::AzureDeploymentName::new(deployment)?,
    ))
}

fn session_id_from_resume_path(path: &std::path::Path) -> anyhow::Result<heycode_core::SessionId> {
    let directory = if path.is_file() {
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("resume session path has no parent"))?
    } else {
        path
    };
    let id = directory
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("resume session identity is unavailable"))?;
    Ok(heycode_core::SessionId::from_raw(id))
}

/// Who answers approval prompts on a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPrompter {
    /// The TUI renders a dialog.
    Interactive,
    /// An ACP or app-server client receives the request and answers it.
    Proxied,
    /// Nobody: a headless `heycode run`. `ask` degrades to an explained deny.
    None,
}

/// Everything needed to compose one world.
pub struct WorldOptions<'a> {
    /// Parsed configuration.
    pub config: &'a Config,
    /// Pre-opened workspace trust service. Construction must precede project
    /// config/profile/MCP discovery.
    pub trust: WorkspaceTrustService,
    /// Redacted startup migration notice, when one applied or remains pending.
    pub config_migration: Option<&'a ConfigMigrationNotice>,
    /// Optional already-discovered profile overlays (named picker/CLI share
    /// this exact input).
    pub profile_layers: &'a [ProfileLayer],
    /// Sessions root (temp dirs in tests).
    pub sessions_dir: std::path::PathBuf,
    /// Owner-only content-addressed attachment root.
    pub attachments_dir: std::path::PathBuf,
    /// Explicit per-attachment admission ceiling.
    pub attachment_max_bytes: usize,
    /// Authoritative origin for a newly created local session. Ignored while
    /// resuming an existing durable stream.
    pub session_source: heycode_session::SessionSource,
    /// Who can answer an `ask`-mode approval on this surface.
    pub approval_prompter: ApprovalPrompter,
    /// Writable user settings document (isolated temp path in tests).
    pub settings_user_path: std::path::PathBuf,
    /// Owner-only credential root and legacy migration source.
    pub credentials_root: std::path::PathBuf,
    /// Standalone versioned model-catalog cache path.
    pub catalog_cache_path: std::path::PathBuf,
    /// Enable external file watching for this world.
    pub settings_watch: bool,
    /// Block the composer behind integrated first-run/connect onboarding.
    pub onboarding_required: bool,
    /// Safe successful preflight instant for the active credential.
    pub credential_validated_at_ms: Option<u64>,
    /// Working directory handed to tools.
    pub cwd: std::path::PathBuf,
    /// Offline smoke provider (skips real providers entirely).
    pub fake: Option<Arc<dyn Provider>>,
    /// Resume this session log instead of creating a fresh one.
    pub resume: Option<std::path::PathBuf>,
}

struct ApprovalComposition {
    policy: Arc<dyn ApprovalPolicy>,
    plugin: Box<dyn Plugin>,
}

/// Compose the approval policy for one surface.
///
/// `ask` needs someone to answer: the TUI shows a dialog, and ACP/app-server
/// clients proxy the request to their front end. A headless `heycode run` has
/// neither, so `ask` there becomes [`UnpromptedDeny`] — every call is refused
/// with a reason that names the fix — instead of a prompt nobody will answer
/// hanging the process forever with no output.
fn approval_composition(
    mode: ApprovalMode,
    prompter: ApprovalPrompter,
) -> anyhow::Result<ApprovalComposition> {
    if mode == ApprovalMode::AcceptedEdits && prompter == ApprovalPrompter::None {
        anyhow::bail!(
            "Accepted edits needs a conversation that can ask for permission. Use full_access for an unattended run."
        );
    }
    if mode == ApprovalMode::Auto {
        anyhow::bail!(
            "Auto is unavailable for this connection. Choose full_access, accepted_edits or default."
        );
    }
    // The interactive shell can prompt, so every mode there is switchable at
    // runtime through `/permissions <mode>`; the same interactive instance is
    // the `ask` target.
    if prompter == ApprovalPrompter::Interactive
        || (prompter == ApprovalPrompter::Proxied
            && matches!(mode, ApprovalMode::Ask | ApprovalMode::AcceptedEdits))
    {
        let interactive = InteractiveApproval::new(heycode_core::EventBus::default());
        let ask: Arc<dyn ApprovalPolicy> = Arc::new(interactive.clone());
        let initial: Arc<dyn ApprovalPolicy> = match mode {
            ApprovalMode::FullAccess => Arc::new(AutoApprove),
            ApprovalMode::AcceptedEdits => Arc::new(heycode_agent::AcceptedEdits::new(ask.clone())),
            ApprovalMode::Auto => unreachable!("Auto rejected before composition"),
            ApprovalMode::Deny => Arc::new(DenyAll),
            ApprovalMode::Ask => ask.clone(),
        };
        let switch = Arc::new(heycode_agent::SwitchableApproval::new(initial, Some(ask)));
        return Ok(ApprovalComposition {
            plugin: heycode_agent::switchable_approval_plugin(switch.clone(), interactive),
            policy: switch,
        });
    }
    Ok(match mode {
        ApprovalMode::Ask | ApprovalMode::AcceptedEdits if prompter == ApprovalPrompter::None => {
            let policy: Arc<dyn ApprovalPolicy> = Arc::new(heycode_agent::UnpromptedDeny);
            ApprovalComposition {
                plugin: approval_plugin(policy.clone()),
                policy,
            }
        }
        ApprovalMode::FullAccess => {
            let policy: Arc<dyn ApprovalPolicy> = Arc::new(AutoApprove);
            ApprovalComposition {
                plugin: approval_plugin(policy.clone()),
                policy,
            }
        }
        ApprovalMode::Deny => {
            let policy: Arc<dyn ApprovalPolicy> = Arc::new(DenyAll);
            ApprovalComposition {
                plugin: approval_plugin(policy.clone()),
                policy,
            }
        }
        ApprovalMode::Ask | ApprovalMode::AcceptedEdits | ApprovalMode::Auto => {
            unreachable!("prompting modes handled before headless composition")
        }
    })
}

fn zai_native_tools_plugin() -> Box<dyn Plugin> {
    struct ZaiNativeToolsPlugin;

    impl Plugin for ZaiNativeToolsPlugin {
        fn name(&self) -> &'static str {
            "native-zai"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Tool],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::NativeTool,
                heycode_provider_zai::ZAI_WEB_SEARCH_IMPLEMENTATION,
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_native_tools::SERVICE_NATIVE_TOOLS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), heycode_core::CoreError> {
            let registry = context
                .get::<heycode_native_tools::NativeToolRegistry>(
                    heycode_native_tools::SERVICE_NATIVE_TOOLS,
                )
                .ok_or_else(|| heycode_core::CoreError::other("native-tools service mismatch"))?;
            let contribution = heycode_provider_zai::zai_web_search_contribution()
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let route = contribution.route();
            let implementation = heycode_native_tools::NativeToolImplementation::new(
                route.logical(),
                route.implementation(),
                route.kind(),
                route.provider().map(str::to_owned),
                contribution.priority(),
            )
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            registry
                .register(context, implementation)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }

    Box::new(ZaiNativeToolsPlugin)
}

/// Resolve the full production factory/profile graph without invoking plugin
/// apply. Used by both composition and side-effect-free doctor inspection.
///
/// # Errors
/// Config/factory/profile/backend construction failures.
pub fn resolve_world_plugins(options: &WorldOptions<'_>) -> anyhow::Result<Vec<ScopedPlugin>> {
    let requested_identity = heycode_trust::WorkspaceIdentity::discover(&options.cwd)?;
    let trust_snapshot = options.trust.snapshot()?;
    if requested_identity.id() != trust_snapshot.identity().id() {
        return Err(anyhow::anyhow!(
            "workspace trust identity does not match composition cwd"
        ));
    }
    let workspace_root = trust_snapshot.identity().canonical_root().to_path_buf();
    let project_settings = options
        .trust
        .access(heycode_trust::ProjectInputKind::Settings)?
        .is_allowed()
        .then(|| heycode_config::project_state_dir(&workspace_root).join("settings.toml"));
    let mut effective_config = options.config.clone();
    let (mut startup_connection, mut saved_connection, setup_required) = if options.fake.is_none() {
        let (selection, saved, setup_required) = resolve_startup_connection_from_files(
            &mut effective_config,
            options.settings_user_path.clone(),
            project_settings.clone(),
        )?;
        (Some(selection), saved, setup_required)
    } else {
        (
            None,
            false,
            startup_requires_setup_from_files(
                options.settings_user_path.clone(),
                project_settings.clone(),
            )?,
        )
    };
    if setup_required {
        effective_config.llm = Config::defaults().llm;
        startup_connection = None;
        saved_connection = false;
    }
    let cfg = &effective_config;
    let connection_parameters = connection_parameters(cfg, startup_connection.as_ref())?;

    for layer in options.profile_layers {
        if matches!(
            layer.scope,
            PluginScope::Project | PluginScope::LocalProject
        ) && !options.trust.allows_plugin_scope(layer.scope)?
        {
            return Err(anyhow::anyhow!(
                "project profile layer is deferred until this workspace is trusted"
            ));
        }
    }
    if options.fake.is_none() && !INFERENCE_PROVIDERS.contains(&cfg.llm.provider.as_str()) {
        return Err(anyhow::anyhow!(unselectable_provider(&cfg.llm.provider)));
    }

    let sandbox = sandbox_service(cfg, &workspace_root)?;

    // Catalog admission, key validation and inference must use the same region.
    // A saved coordinate takes precedence over host defaults without changing them.
    let explicit_aws_region = connection_parameters
        .get("region")
        .map(|region| heycode_authorization_aws::AwsRegion::new(region.clone()))
        .transpose()?;
    let aws_region = explicit_aws_region
        .clone()
        .or_else(|| resolve_aws_region(&heycode_authorization_aws::ProcessAwsHost));
    let aws_region_configured = aws_region.is_some();
    let azure_connection = (cfg.llm.provider == heycode_provider_azure::AZURE_OPENAI_PROVIDER)
        .then(|| azure_connection(&connection_parameters))
        .transpose()?;
    let custom_openai_connection = (cfg.llm.provider
        == heycode_provider_openai_compatible::CUSTOM_OPENAI_PROVIDER)
        .then(|| {
            let endpoint = cfg.llm.base_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!("custom OpenAI-compatible inference requires a saved server URL")
            })?;
            Ok::<_, anyhow::Error>((
                heycode_provider_openai_compatible::CustomOpenAiEndpoint::new(endpoint)?,
                heycode_provider_openai_compatible::CustomOpenAiModel::new(cfg.llm.model.clone())?,
            ))
        })
        .transpose()?;
    if matches!(cfg.llm.provider.as_str(), "bedrock" | "bedrock-mantle") && aws_region.is_none() {
        return Err(anyhow::anyhow!(
            "AWS inference requires a valid AWS_REGION or AWS_DEFAULT_REGION"
        ));
    }

    #[cfg(unix)]
    let import_generation =
        Arc::new(heycode_config::imports::ImportStore::new(&options.credentials_root)?.snapshot()?);
    #[cfg(unix)]
    let import_mount = Arc::new(
        heycode_extension_host::config_import::PinnedImportMount::new(
            import_generation,
            options.trust.clone(),
        )?,
    );
    #[cfg(unix)]
    let import_settings_paths = std::iter::once(options.settings_user_path.clone())
        .chain(project_settings.clone())
        .collect::<Vec<_>>();

    let mut factories = PluginFactories::new();

    factories.register("doctor", doctor_plugin);
    let config_migration = options.config_migration.cloned();
    factories.register("doctor-config", move || {
        config_migration_doctor_plugin(config_migration)
    });
    let trust = options.trust.clone();
    factories.register("trust", move || trust_service_plugin(trust));
    factories.register("ui", ui_registry_plugin);

    let settings_user_path = options.settings_user_path.clone();
    let settings_watch = options.settings_watch;
    let requested_runtime =
        requested_runtime_from_files(settings_user_path.clone(), project_settings.clone())?;
    #[cfg(unix)]
    let settings_import_mount = import_mount.clone();
    factories.register("settings", move || {
        let mut config = FileSettingsConfig::user(settings_user_path);
        if let Some(project_settings) = project_settings {
            config = config.with_project(project_settings);
        }
        let config = if settings_watch {
            config
        } else {
            config.without_watch()
        };
        #[cfg(unix)]
        {
            heycode_settings_file::file_settings_plugin_with_overlay(config, settings_import_mount)
        }
        #[cfg(not(unix))]
        {
            heycode_settings_file::file_settings_plugin(config)
        }
    });
    factories.register("doctor-settings", settings_doctor_plugin);
    factories.register(
        "settings-aws-bedrock",
        heycode_provider_aws::aws_bedrock_settings_plugin,
    );
    factories.register(
        "settings-google-inference",
        heycode_provider_google::google_inference_settings_plugin,
    );
    factories.register("http", http_plugin);
    factories.register("provider-activation", provider_activation::plugin);
    factories.register("sandbox", move || heycode_sandbox::sandbox_plugin(sandbox));
    factories.register("subprocess-local", local_subprocess_plugin);
    factories.register("terminal-registry", heycode_exec::terminal_registry_plugin);
    let shell_config = LocalShellConfig::platform(
        workspace_root.clone(),
        std::time::Duration::from_millis(cfg.tools.bash_timeout_ms),
    )?;
    let scope_cwd = workspace_root.clone();
    factories.register("workspace-scope", move || {
        workspace_integration::scope_plugin(scope_cwd, shell_config)
    });
    factories.register("shell-local", workspace_integration::shell_plugin);
    factories.register("filesystem-local", workspace_integration::filesystem_plugin);
    #[cfg(unix)]
    {
        let retained_root = options
            .settings_user_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("settings path has no retained-output owner root"))?
            .join("retained-output");
        let retained_config = heycode_exec::RetainedOutputConfig::new(retained_root)?;
        factories.register("retained-output-local", move || {
            heycode_exec::retained_output_plugin(retained_config.clone())
        });
    }
    factories.register("lsp-registry", heycode_exec::lsp_registry_plugin);
    factories.register("credentials", credentials_plugin);
    let environment_credentials = EnvironmentCredentialProvider::process()?;
    factories.register("credentials-env", move || {
        environment_credentials_plugin(environment_credentials)
    });
    let command_credentials = CommandCredentialProvider::new(std::iter::empty())?;
    factories.register("credentials-command", move || {
        command_credentials_plugin(command_credentials)
    });
    let credentials_root = options.credentials_root.clone();
    factories.register("credentials-file", move || {
        file_credentials_plugin(FileCredentialConfig::new(credentials_root))
    });
    factories.register("doctor-credentials", credentials_doctor_plugin);
    factories.register("authorization", authorization_plugin);
    let secret_prompt = InteractiveSecretPrompt::new(heycode_core::EventBus::default());
    let prompt_for_flows: Arc<dyn SecretPrompt> = Arc::new(secret_prompt.clone());
    let prompt_for_service = secret_prompt.clone();
    factories.register("secret-prompt", move || {
        secret_prompt_plugin(prompt_for_service)
    });
    let mut api_key_flows = default_deepseek_api_key_flows(cfg, prompt_for_flows.clone())?;
    let compatible_bindings = heycode_provider_compatible::builtin_specs()
        .iter()
        .map(|spec| {
            let active = cfg.llm.provider == spec.id;
            let base = active.then_some(cfg.llm.base_url.as_deref()).flatten();
            let reference = active
                .then_some(cfg.llm.api_key_env.as_deref())
                .flatten()
                .unwrap_or(spec.credential_reference)
                .to_owned();
            (*spec, spec.models_endpoint(base), reference)
        })
        .collect::<Vec<_>>();
    for (spec, url, reference) in &compatible_bindings {
        api_key_flows.extend(heycode_provider_compatible::compatible_authorization_flows(
            *spec,
            url,
            reference,
            prompt_for_flows.clone(),
        )?);
    }
    factories.register("catalog-compatible", move || {
        heycode_provider_compatible::compatible_catalog_plugin(compatible_bindings)
    });
    factories.register("authorization-api-key", move || {
        api_key_authorization_plugin(api_key_flows)
    });
    let openrouter_reference = if cfg.llm.provider == "openrouter" {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_llm::OpenRouterProvider::API_KEY_ENV)
    } else {
        heycode_llm::OpenRouterProvider::API_KEY_ENV
    };
    let openrouter_config = match cfg.llm.base_url.as_deref() {
        Some(base_url) if cfg.llm.provider == "openrouter" => OpenRouterPluginConfig::at_base_url(
            api_key_query(openrouter_reference)?,
            prompt_for_flows.clone(),
            base_url,
            Some(cfg.llm.model.clone()),
        )?,
        _ => OpenRouterPluginConfig::official(
            api_key_query(openrouter_reference)?,
            prompt_for_flows.clone(),
            (cfg.llm.provider == "openrouter").then(|| cfg.llm.model.clone()),
        )?,
    };
    factories.register("provider-openrouter", move || {
        openrouter_plugin(openrouter_config)
    });
    let anthropic_reference = if cfg.llm.provider == "anthropic" {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(ANTHROPIC_API_KEY_REFERENCE)
    } else {
        ANTHROPIC_API_KEY_REFERENCE
    };
    let anthropic_config = match cfg.llm.base_url.as_deref() {
        Some(base_url) if cfg.llm.provider == "anthropic" => AnthropicPluginConfig::at_base_url(
            api_key_query(anthropic_reference)?,
            prompt_for_flows.clone(),
            base_url,
            Some(cfg.llm.model.clone()),
        )?,
        _ => AnthropicPluginConfig::official(
            api_key_query(anthropic_reference)?,
            prompt_for_flows.clone(),
            (cfg.llm.provider == "anthropic").then(|| cfg.llm.model.clone()),
        )?,
    };
    factories.register("provider-anthropic", move || {
        anthropic_plugin(anthropic_config)
    });
    let openai_reference = if cfg.llm.provider == "openai" {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(OPENAI_API_KEY_REFERENCE)
    } else {
        OPENAI_API_KEY_REFERENCE
    };
    let openai_config = match cfg.llm.base_url.as_deref() {
        Some(base_url) if cfg.llm.provider == "openai" => OpenAiPluginConfig::at_base_url(
            api_key_query(openai_reference)?,
            prompt_for_flows.clone(),
            base_url,
            Some(cfg.llm.model.clone()),
        )?,
        _ => OpenAiPluginConfig::official(
            api_key_query(openai_reference)?,
            prompt_for_flows.clone(),
            (cfg.llm.provider == "openai").then(|| cfg.llm.model.clone()),
        )?,
    };
    factories.register("provider-openai", move || openai_plugin(openai_config));
    let aws_reference = if matches!(cfg.llm.provider.as_str(), "bedrock" | "bedrock-mantle") {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE)
    } else {
        heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE
    };
    let aws_config = heycode_authorization_aws::AwsAuthPluginConfig::new(
        api_key_query(aws_reference)?,
        prompt_for_flows.clone(),
        std::sync::Arc::new(heycode_authorization_aws::ProcessAwsHost),
    )
    .with_region(explicit_aws_region);
    factories.register("authorization-aws", move || {
        heycode_authorization_aws::aws_authorization_plugin(aws_config.clone())
    });
    factories.register(
        "authorization-gcp",
        heycode_authorization_gcp::gcp_auth_plugin,
    );
    let lmstudio_config = lmstudio_config_for_route(cfg)?;
    let lmstudio_provider_config = lmstudio_config.clone();
    factories.register("provider-lmstudio", move || {
        heycode_provider_lmstudio::lmstudio_plugin(lmstudio_provider_config)
    });
    if cfg.llm.provider == heycode_provider_lmstudio::OLLAMA_PROVIDER {
        let endpoint = match cfg.llm.base_url.as_deref() {
            Some(base_url) => heycode_provider_lmstudio::OllamaEndpoint::new(base_url)?,
            None => heycode_provider_lmstudio::OllamaEndpoint::local(),
        };
        let profile = heycode_provider_lmstudio::OllamaProfile::new(cfg.llm.model.clone())?;
        let credential = cfg
            .llm
            .api_key_env
            .as_deref()
            .map(api_key_query)
            .transpose()?;
        factories.register("provider-ollama", move || {
            heycode_provider_lmstudio::ollama_plugin_with_credential(
                endpoint.clone(),
                profile.clone(),
                credential.clone(),
            )
        });
    }
    if cfg.llm.provider != heycode_provider_lmstudio::OLLAMA_PROVIDER {
        factories.register("catalog-ollama", || {
            heycode_provider_lmstudio::ollama_catalog_plugin(
                heycode_provider_lmstudio::OllamaEndpoint::local(),
            )
        });
    }
    factories.register(
        "lmstudio-control",
        heycode_provider_lmstudio::lmstudio_control_plugin,
    );
    factories.register("telemetry-local-off", heycode_telemetry::telemetry_plugin);
    factories.register("telemetry-metrics", telemetry_metrics_plugin);
    factories.register(
        heycode_telemetry_otlp::PLUGIN_TELEMETRY_OTLP_HTTP,
        heycode_telemetry_otlp::telemetry_otlp_http_plugin,
    );
    // The hook service is constructed with the decision that exists at
    // composition, and `Unknown` is not trusted — a project hook stays inert
    // until someone affirmatively vouches for the workspace (K12).
    let hook_trust = options
        .trust
        .snapshot()
        .map(|snapshot| snapshot.decision())
        .unwrap_or(heycode_trust::WorkspaceTrustDecision::Unknown);
    factories.register("hooks", move || heycode_hooks::hooks_plugin(hook_trust));
    let onboarding_required = options.onboarding_required || setup_required;
    let reconnect = saved_connection.then(|| {
        let profile = setup::connection_profiles()
            .into_iter()
            .find(|profile| profile.registry_name == cfg.llm.provider);
        let name = profile.map_or_else(
            || cfg.llm.provider.clone(),
            |profile| profile.descriptor.display_name,
        );
        heycode_onboarding::OnboardingOption {
            id: cfg.llm.provider.clone(),
            label: format!("Reconnect {name}"),
            description: "Repair the saved credential and keep this provider".into(),
        }
    });
    factories.register("onboarding", move || {
        heycode_onboarding::onboarding_plugin_with_reconnect(onboarding_required, reconnect)
    });
    let profile_home = options
        .settings_user_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings path has no owner root"))?
        .to_path_buf();
    factories.register("profiles", move || named_profiles_plugin(profile_home));

    let interactive = options.session_source == heycode_session::SessionSource::Interactive;
    let approval = approval_composition(
        if setup_required {
            ApprovalMode::Ask
        } else {
            cfg.approval.effective(interactive)
        },
        options.approval_prompter,
    )?;
    let approval_policy = approval.policy.clone();
    let approval_row = approval.plugin;
    factories.register("approval", move || approval_row);

    let resume = (!setup_required).then(|| options.resume.clone()).flatten();
    let sessions_dir = options.sessions_dir.clone();
    let session_metadata = heycode_session::SessionCreationMetadata::new(
        Some(workspace_root.clone()),
        Some(requested_runtime.clone()),
        options.session_source,
    )?;
    let (session_id, mcp_product) = match resume.as_deref() {
        Some(path) => {
            let id = session_id_from_resume_path(path)?;
            let product = heycode_tui::McpProductSession::new(&id, approval_policy)?;
            (id, product)
        }
        None => heycode_tui::McpProductSession::fresh(approval_policy)?,
    };
    factories.register("session", move || match resume {
        Some(path) => heycode_session::session_resume_plugin(path),
        None => heycode_session::session_with_id_and_metadata_plugin(
            sessions_dir,
            session_id,
            session_metadata,
        ),
    });
    let attachment_config = AttachmentStoreConfig::new(
        options.attachments_dir.clone(),
        options.attachment_max_bytes,
    )?;
    factories.register("attachments-local", move || {
        local_attachment_plugin(attachment_config.clone())
    });
    let session_query_root = options.sessions_dir.clone();
    factories.register("session-query-jsonl", move || {
        heycode_session::session_query_jsonl_plugin(session_query_root)
    });
    // Project instructions follow the same trust gate as skills: the trusted
    // workspace root is read, an untrusted one is not, and the user's own
    // `$HEYCODE_HOME/AGENTS.md` applies either way.
    let instruction_sources = heycode_prompt::instructions::InstructionSources {
        user_home: Some(options.credentials_root.clone()),
        workspace: options
            .trust
            .access(heycode_trust::ProjectInputKind::Instructions)?
            .is_allowed()
            .then(|| workspace_root.clone()),
    };
    factories.register("prompt", move || {
        heycode_prompt::prompt_plugin_with_instructions(instruction_sources)
    });
    factories.register("native-tools", native_tools_plugin);
    let openai_native_model = if cfg.llm.provider == "openai" {
        cfg.llm.model.clone()
    } else {
        OPENAI_GPT_5_6_SOL.to_owned()
    };
    factories.register("native-openai", move || {
        openai_configured_native_tools_plugin(openai_native_model)
    });
    let anthropic_native_model = if cfg.llm.provider == "anthropic" {
        cfg.llm.model.clone()
    } else {
        ANTHROPIC_CLAUDE_OPUS_5.to_owned()
    };
    factories.register("native-anthropic", move || {
        anthropic_configured_native_tools_plugin(anthropic_native_model)
    });
    factories.register("native-openrouter", openrouter_native_tools_plugin);
    factories.register("native-zai", zai_native_tools_plugin);
    factories.register("web", web_registry_plugin);
    factories.register("web-portable", || {
        portable_web_plugin(PortableWebConfig::official())
    });
    factories.register("web-extract", web_extract_plugin);
    factories.register("web-policy", web_policy_plugin);
    let tools_cfg = heycode_tools::ToolsConfig {
        read_max_bytes: cfg.tools.read_max_bytes,
        read_max_lines: cfg.tools.read_max_lines,
        web_enabled: cfg.web.enabled,
        terminals_enabled: cfg.tools.terminals_enabled,
        cwd: workspace_root.clone(),
    };
    factories.register("tools", move || tools_plugin(tools_cfg));
    let browser_config =
        heycode_tools::interactive::BrowserConfig::from_environment().filter(|_| cfg.web.enabled);
    let speech_config = heycode_tools::interactive::SpeechCommandConfig::from_environment()?;
    factories.register("interactive-tools", move || {
        heycode_tools::interactive::interactive_tools_plugin_with_speech(
            browser_config,
            speech_config,
        )
    });
    #[cfg(unix)]
    factories.register("lsp-tools", heycode_tools::lsp_tools_plugin);
    factories.register("native-tool-policy", native_tool_policy_plugin);
    factories.register("models", || {
        model_catalog_plugin(std::time::Duration::from_secs(5 * 60))
    });
    factories.register("runtimes", runtime_registry_plugin);
    let claude_runtime = ClaudeRuntimeConfig::new(workspace_root.clone())?;
    factories.register("runtime-claude", move || {
        claude_runtime_plugin(claude_runtime)
    });
    let codex_client = CodexClientInfo::new("heycode", "heycode", env!("CARGO_PKG_VERSION"))?;
    let codex_runtime = CodexAppServerConfig::new(
        "codex",
        &workspace_root,
        codex_environment_snapshot(),
        codex_client,
    )?
    .with_outer_sandbox_mode(match cfg.sandbox.mode {
        heycode_config::SandboxModeCfg::Off => heycode_exec::SandboxMode::Off,
        heycode_config::SandboxModeCfg::ReadOnly => heycode_exec::SandboxMode::ReadOnly,
        heycode_config::SandboxModeCfg::Workspace => heycode_exec::SandboxMode::WorkspaceWrite,
    });
    factories.register("runtime-codex", move || codex_runtime_plugin(codex_runtime));
    let opencode_runtime = OpenCodeRuntimeConfig::new(workspace_root.clone())?;
    factories.register("runtime-opencode", move || {
        opencode_runtime_plugin(opencode_runtime)
    });
    let grok_runtime = heycode_runtime_grok::GrokRuntimeConfig::new(workspace_root.clone())?;
    factories.register("runtime-grok", move || {
        heycode_runtime_grok::grok_runtime_plugin(grok_runtime)
    });
    let deepseek_harness_runtime = DeepSeekHarnessRuntimeConfig::new();
    factories.register("runtime-deepseek-harness", move || {
        deepseek_harness_runtime_plugin(deepseek_harness_runtime)
    });
    factories.register("mcp-registry", mcp_registry_plugin);
    factories.register("mcp-management", || {
        // The shell can ask ("Test connection", "Refresh and probe"), so
        // it gets a probe that really connects. Listing still never does.
        heycode_mcp::management::mcp_management_plugin_with_probe(Some(Arc::new(
            heycode_mcp::management::LiveMcpProbe::default(),
        )))
    });
    let lifecycle_cache_root = options.credentials_root.join("plugins");
    #[cfg(unix)]
    let product_extension_cache_root = lifecycle_cache_root.clone();
    #[cfg(unix)]
    let product_extension_validator = heycode_extensions::ManifestValidator::new(
        heycode_extensions::ApiVersion::new(1)?,
        crate::plugin_cli::host_platform(),
    );
    #[cfg(unix)]
    let installed_code_authority = Arc::new(
        |_cache: &heycode_extensions::PluginInstallCache,
         _enabled: &[heycode_extensions::lifecycle::PluginState]| {
            Ok::<_, heycode_extension_host::InstalledCodePluginError>(Vec::new())
        },
    );
    factories.register("plugin-lifecycle", move || {
        crate::plugin_cli::managed_lifecycle_plugin(lifecycle_cache_root.clone())
    });
    let catalog_cache_path = options.catalog_cache_path.clone();
    factories.register("catalog-cache-file", move || {
        file_catalog_persistence_plugin(FileCatalogConfig::new(catalog_cache_path))
    });
    let override_home = options
        .settings_user_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings path has no catalog-override owner root"))?
        .to_path_buf();
    let mut override_layers = vec![heycode_catalog_file::CatalogOverrideLayer::new(
        "user",
        override_home.join("catalog-overrides.toml"),
    )];
    if options
        .trust
        .access(heycode_trust::ProjectInputKind::Settings)?
        .is_allowed()
    {
        override_layers.push(heycode_catalog_file::CatalogOverrideLayer::new(
            "project",
            heycode_config::project_state_dir(&workspace_root).join("catalog-overrides.toml"),
        ));
    }
    let override_config = heycode_catalog_file::CatalogOverridesConfig::new(override_layers);
    factories.register("catalog-overrides", move || {
        heycode_catalog_file::catalog_overrides_plugin(override_config.clone())
    });
    let deepseek_reference = if cfg.llm.provider == "deepseek" {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_llm::DeepSeekProvider::API_KEY_ENV)
    } else {
        heycode_llm::DeepSeekProvider::API_KEY_ENV
    };
    let mut deepseek_catalog = DeepSeekCatalogConfig::official(api_key_query(deepseek_reference)?);
    if cfg.llm.provider == "deepseek"
        && let Some(base_url) = cfg.llm.base_url.as_ref()
    {
        deepseek_catalog = deepseek_catalog.with_base_url(base_url);
    }
    factories.register("catalog-deepseek", move || {
        deepseek_catalog_plugin(deepseek_catalog)
    });
    let mut openrouter_catalog = OpenRouterCatalogConfig::official();
    if cfg.llm.provider == "openrouter"
        && let Some(base_url) = cfg.llm.base_url.as_ref()
    {
        openrouter_catalog = openrouter_catalog.with_base_url(base_url);
    }
    factories.register("catalog-openrouter", move || {
        openrouter_catalog_plugin(openrouter_catalog)
    });
    let anthropic_catalog_credential = api_key_query(anthropic_reference)?;
    factories.register("token-counters", heycode_llm::token_counters_plugin);
    let anthropic_counter_credential = api_key_query(ANTHROPIC_API_KEY_REFERENCE)?;
    factories.register("token-count-anthropic", move || {
        heycode_provider_anthropic::anthropic_token_counter_plugin(
            heycode_provider_anthropic::AnthropicTokenCounterConfig::official(
                anthropic_counter_credential.clone(),
            ),
        )
    });
    let mut anthropic_catalog = AnthropicCatalogConfig::official(anthropic_catalog_credential);
    if cfg.llm.provider == "anthropic"
        && let Some(base_url) = cfg.llm.base_url.as_ref()
    {
        anthropic_catalog = anthropic_catalog.with_base_url(base_url);
    }
    factories.register("catalog-anthropic", move || {
        anthropic_catalog_plugin(anthropic_catalog)
    });
    let openai_catalog_credential = api_key_query(openai_reference)?;
    let mut openai_catalog = OpenAiCatalogConfig::official(openai_catalog_credential);
    if cfg.llm.provider == "openai"
        && let Some(base_url) = cfg.llm.base_url.as_ref()
    {
        openai_catalog = openai_catalog.with_base_url(base_url);
    }
    factories.register("catalog-openai", move || {
        openai_catalog_plugin(openai_catalog)
    });
    let google_catalog_credential =
        api_key_query(heycode_provider_google::GOOGLE_API_KEY_REFERENCE)?;
    factories.register("catalog-google", move || {
        heycode_provider_google::google_catalog_plugin(
            heycode_provider_google::GoogleCatalogConfig::api_key(
                google_catalog_credential.clone(),
            ),
        )
    });
    let azure_reference = if cfg.llm.provider == heycode_provider_azure::AZURE_OPENAI_PROVIDER {
        cfg.llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_provider_azure::AZURE_OPENAI_API_KEY_REFERENCE)
    } else {
        heycode_provider_azure::AZURE_OPENAI_API_KEY_REFERENCE
    };
    let mut azure_catalog =
        heycode_provider_azure::AzureOpenAiCatalogConfig::api_key(api_key_query(azure_reference)?);
    if let Some((resource, deployment)) = azure_connection.clone() {
        azure_catalog = azure_catalog.with_connection(resource, deployment);
    }
    factories.register("catalog-azure-openai", move || {
        heycode_provider_azure::azure_openai_catalog_plugin(azure_catalog)
    });
    let custom_openai_credential =
        if cfg.llm.provider == heycode_provider_openai_compatible::CUSTOM_OPENAI_PROVIDER {
            cfg.llm
                .api_key_env
                .as_deref()
                .map(api_key_query)
                .transpose()?
        } else {
            None
        };
    let mut custom_openai_catalog =
        heycode_provider_openai_compatible::CustomOpenAiCatalogConfig::discovery();
    if let Some((endpoint, _)) = custom_openai_connection.clone() {
        custom_openai_catalog =
            custom_openai_catalog.with_connection(endpoint, custom_openai_credential.clone());
    }
    factories.register("catalog-custom-openai", move || {
        heycode_provider_openai_compatible::custom_openai_catalog_plugin(custom_openai_catalog)
    });
    if cfg.llm.provider != "vertex-google" {
        let vertex_catalog_credential = provider_credential_query(
            "vertex-google",
            heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE,
        )?;
        factories.register("catalog-google-vertex", move || {
            heycode_provider_google::vertex_gemini_catalog_plugin(
                heycode_provider_google::VertexGeminiCatalogConfig::oauth_token(
                    vertex_catalog_credential.clone(),
                ),
            )
        });
    }
    if cfg.llm.provider != "vertex-claude" {
        factories.register(
            "catalog-google-claude-vertex",
            heycode_provider_google::maintained_claude_vertex_catalog_plugin,
        );
    }
    factories.register("catalog-minimax", || heycode_provider_minimax::minimax_catalog_plugin(
        heycode_provider_minimax::MiniMaxCatalogConfig::new(heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::PayAsYouGo>::international(), heycode_provider_minimax::MiniMaxApiFamily::OpenAiCompatible)));
    factories.register("catalog-minimax-token-plan", || heycode_provider_minimax::minimax_catalog_plugin(
        heycode_provider_minimax::MiniMaxCatalogConfig::new(heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::TokenPlan>::international(), heycode_provider_minimax::MiniMaxApiFamily::OpenAiCompatible)));
    factories.register(
        "catalog-zai",
        heycode_provider_zai::zai_catalog_plugin::<heycode_provider_zai::General>,
    );
    factories.register(
        "catalog-zai-coding",
        heycode_provider_zai::zai_catalog_plugin::<heycode_provider_zai::Coding>,
    );
    factories.register("catalog-lmstudio", move || {
        heycode_provider_lmstudio::lmstudio_catalog_plugin(lmstudio_config)
    });
    if cfg.llm.provider != "bedrock" {
        let bedrock_credential = api_key_query(aws_reference)?;
        factories.register("catalog-bedrock", move || {
            heycode_provider_aws::bedrock_catalog_plugin(
                heycode_provider_aws::BedrockCatalogConfig::api_key(bedrock_credential.clone()),
            )
        });
    }
    if aws_region_configured && cfg.llm.provider != "bedrock-mantle" {
        let mantle_credential =
            api_key_query(heycode_authorization_aws::AWS_BEDROCK_API_KEY_REFERENCE)?;
        // Mantle serves fewer regions than the control plane, and the region
        // list would go stale the moment AWS adds one. Both mount together and
        // an unsupported region reports an honest failed refresh rather than
        // being rejected by a snapshot compiled in last month.
        factories.register("catalog-bedrock-mantle", move || {
            heycode_provider_aws::mantle_catalog_plugin(
                heycode_provider_aws::MantleCatalogConfig::api_key(mantle_credential.clone()),
            )
        });
    }
    if setup_required {
        let selection = LlmSelection {
            provider_name: cfg.llm.provider.clone(),
            model: cfg.llm.model.clone(),
        };
        let provider = Arc::new(DisconnectedProvider {
            name: cfg.llm.provider.clone(),
            model: cfg.llm.model.clone(),
        });
        factories.register("llm", move || llm_plugin(selection, vec![provider]));
    } else if let Some(fake) = options.fake.clone() {
        let selection = LlmSelection {
            provider_name: fake.info().name.to_owned(),
            model: cfg.llm.model.clone(),
        };
        factories.register("llm", move || llm_plugin(selection, vec![fake]));
    } else if cfg.llm.provider == heycode_provider_lmstudio::OLLAMA_PROVIDER {
        let model = cfg.llm.model.clone();
        factories.register("llm", move || ollama_llm_plugin(model));
    } else if cfg.llm.provider == heycode_provider_openai_compatible::CUSTOM_OPENAI_PROVIDER {
        validate_llm_protocol(&cfg.llm.provider, cfg.llm.protocol)?;
        let selection = LlmSelection {
            provider_name: cfg.llm.provider.clone(),
            model: cfg.llm.model.clone(),
        };
        factories.register("llm", move || llm_plugin(selection, Vec::new()));
    } else {
        let provider_name = cfg.llm.provider.clone();
        let protocol = cfg.llm.protocol;
        validate_llm_protocol(&provider_name, protocol)?;
        let reference = match cfg.llm.api_key_env.clone() {
            Some(reference) => reference,
            None => default_provider_reference(&provider_name)?.to_owned(),
        };
        let allow_unconfigured = options.onboarding_required || requested_runtime != "native";
        let credential_validated_at_ms = options.credential_validated_at_ms;
        let config = cfg.llm.clone();
        factories.register("llm", move || {
            credential_llm_plugin(
                config,
                reference,
                allow_unconfigured,
                credential_validated_at_ms,
            )
        });
    }
    if !setup_required
        && options.fake.is_none()
        && let Some(plugin) = aws_inference_plugin_for_config(cfg, aws_region.clone())?
    {
        factories.register(plugin.name(), move || plugin);
    }
    if !setup_required && options.fake.is_none() && cfg.llm.provider == "google" {
        if cfg.llm.model != heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH {
            return Err(anyhow::anyhow!(
                "Google inference model has no maintained composition evidence"
            ));
        }
        let reference = cfg
            .llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_provider_google::GOOGLE_API_KEY_REFERENCE);
        let evidence = heycode_provider_google::GoogleGeminiModelEvidence::new(
            heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH,
            vec![heycode_llm::ModelDescriptor::unknown(
                heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH,
            )],
        )?;
        let plugin = heycode_provider_google::google_developer_settings_plugin(
            api_key_query(reference)?,
            evidence,
        )?;
        factories.register("inference-google-gemini", move || plugin);
    }
    if !setup_required
        && options.fake.is_none()
        && matches!(cfg.llm.provider.as_str(), "vertex-google" | "vertex-claude")
    {
        let profile = gcp_profile_request(
            &heycode_authorization_gcp::ProcessGcpEnvironment,
            &connection_parameters,
        )?;
        let reference = cfg
            .llm
            .api_key_env
            .as_deref()
            .unwrap_or(heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE);
        let credential = provider_credential_query(&cfg.llm.provider, reference)?;
        let plugin = match cfg.llm.provider.as_str() {
            "vertex-google" => {
                if cfg.llm.model != heycode_provider_google::GOOGLE_GEMINI_3_7_FLASH {
                    return Err(anyhow::anyhow!(
                        "Vertex Gemini model has no maintained composition evidence"
                    ));
                }
                heycode_provider_google::google_lazy_vertex_settings_plugin(profile, credential)?
            }
            "vertex-claude" => {
                if cfg.llm.model != heycode_provider_google::CLAUDE_VERTEX_DEFAULT_MODEL {
                    return Err(anyhow::anyhow!(
                        "Claude-on-Vertex model has no maintained composition evidence"
                    ));
                }
                heycode_provider_google::google_inference_plugin(
                    heycode_provider_google::GoogleInferencePluginConfig::lazy_claude_vertex(
                        profile,
                        credential,
                        heycode_provider_google::ClaudeVertexControls::sonnet_five_default(),
                    )?,
                )
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "configured Vertex inference plugin identity is unavailable"
                ));
            }
        };
        factories.register(plugin.name(), move || plugin);
    }
    if !setup_required
        && options.fake.is_none()
        && cfg.llm.provider == heycode_provider_azure::AZURE_OPENAI_PROVIDER
    {
        let (resource, deployment) = azure_connection.ok_or_else(|| {
            anyhow::anyhow!(
                "Azure OpenAI inference requires saved resource and deployment coordinates"
            )
        })?;
        if cfg.llm.model != deployment.as_str() {
            return Err(anyhow::anyhow!(
                "Azure OpenAI selected model must equal the saved deployment"
            ));
        }
        let credential = provider_credential_query(
            heycode_provider_azure::AZURE_OPENAI_PROVIDER,
            azure_reference,
        )?;
        let plugin = heycode_provider_azure::azure_openai_inference_plugin(
            heycode_provider_azure::AzureInferencePluginConfig::api_key(
                resource, deployment, credential,
            )?,
        );
        factories.register(plugin.name(), move || plugin);
    }
    if !setup_required
        && options.fake.is_none()
        && cfg.llm.provider == heycode_provider_openai_compatible::CUSTOM_OPENAI_PROVIDER
    {
        let (endpoint, model) = custom_openai_connection.ok_or_else(|| {
            anyhow::anyhow!("custom OpenAI-compatible inference route is incomplete")
        })?;
        let plugin = heycode_provider_openai_compatible::custom_openai_inference_plugin(
            heycode_provider_openai_compatible::CustomOpenAiInferencePluginConfig::new(
                endpoint,
                model,
                custom_openai_credential,
            )?,
        );
        factories.register(plugin.name(), move || plugin);
    }
    factories.register("request-transforms", request_transforms_plugin);
    factories.register("request-transforms-openrouter", || {
        openrouter_request_transforms_plugin(
            heycode_provider_openrouter::OpenRouterTransformPolicy::all_disabled(),
        )
    });
    factories.register("provider-telemetry", provider_telemetry_plugin);
    let agent_options = AgentOptions {
        compaction: CompactionPolicy {
            auto: cfg.compaction.auto,
            threshold_ratio: cfg.compaction.threshold_ratio,
            context_window: cfg.compaction.context_window,
        },
        max_task_depth: cfg.subagent.max_depth,
        cwd: Some(workspace_root.clone()),
        auto_title: cfg.ui.auto_title,
    };
    factories.register("agent-options", move || agent_options_plugin(agent_options));
    factories.register("commands", commands_plugin);
    let config_report = cfg.report();
    factories.register("status", move || {
        status_plugin_with_config(config_report.clone())
    });
    factories.register("status-context", context_status_plugin);
    factories.register("status-web", web_status_plugin);
    let health_root = options
        .settings_user_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings path has no owner root"))?
        .to_path_buf();
    factories.register("health-history", move || {
        health_history_plugin(health_root.join("state/health-history.jsonl"))
    });
    let init_root = workspace_root.clone();
    factories.register("init", move || init_plugin(init_root));
    let skill_home = options.credentials_root.clone();
    let project_skills_allowed = options
        .trust
        .access(heycode_trust::ProjectInputKind::Instructions)?
        .is_allowed();
    let skill_roots = if project_skills_allowed {
        heycode_skills::default_roots(&workspace_root, &skill_home)?
    } else {
        vec![heycode_skills::user_root(&skill_home)?]
    };
    let skill_plugin = if project_skills_allowed {
        heycode_skills::skills_plugin_with_workspace_reload_guard(skill_roots, &workspace_root)?
    } else {
        skills_plugin(skill_roots)
    };
    factories.register("skills", move || skill_plugin);
    // Resolve each server's transport at the composition boundary, so the
    // plugin receives a fully resolved specification and an ambiguous or
    // empty selection fails loud here rather than defaulting deeper in.
    let mut mcp_servers: std::collections::HashMap<String, McpServerSpec> =
        std::collections::HashMap::new();
    for (name, scfg) in &cfg.mcp.servers {
        let spec = match scfg.transport(name)? {
            heycode_config::McpServerTransport::Stdio { command, args, env } => {
                McpServerSpec::Stdio(McpServerConfig {
                    command,
                    args,
                    env,
                    required: scfg.required,
                })
            }
            heycode_config::McpServerTransport::StreamableHttp { url } => {
                McpServerSpec::StreamableHttp {
                    url,
                    required: scfg.required,
                }
            }
        };
        mcp_servers.insert(name.clone(), spec);
    }
    let mut mcp_routers = std::collections::HashMap::new();
    for name in mcp_servers.keys() {
        let server = heycode_mcp::McpServerId::new(name.clone())?;
        let router = mcp_product.router(
            server,
            heycode_mcp::McpElicitationCapabilities::form_and_url(),
        )?;
        mcp_routers.insert(name.clone(), router);
    }
    let mcp_row = heycode_mcp::mcp_product_plugin(
        mcp_servers,
        workspace_root.clone(),
        mcp_product.approval_handler(),
        mcp_routers,
        mcp_product.mcp_lifecycle_hooks(),
    )?;
    factories.register("mcp", move || mcp_row);
    let product_hook_adapter = mcp_product.hook_adapter();
    let product_tui_bridge = mcp_product.tui_bridge();
    factories.register("compactions", compactions_plugin);
    let (subagent_dir, subagent_depth) = (options.sessions_dir.clone(), cfg.subagent.max_depth);
    let codex_subagent_dir = subagent_dir.clone();
    let claude_subagent_dir = subagent_dir.clone();
    let codex_subagent_workspace = workspace_root.clone();
    let claude_subagent_workspace = workspace_root.clone();
    let subagent_budget = heycode_agent::SubagentBudgetLimits {
        max_in_flight: cfg.subagent.max_concurrent,
        max_requests: cfg.subagent.max_provider_requests,
        max_output_tokens: cfg.subagent.max_output_tokens,
    };
    if !(0..=131072).contains(&subagent_budget.max_output_tokens) {
        anyhow::bail!(
            "invalid [subagent] admission budget: output tokens 0..131072 required; zero concurrency/request/output limits mean unlimited"
        );
    }
    factories.register("subagent", move || {
        heycode_agent::subagent_plugin_with_budget(subagent_dir, subagent_depth, subagent_budget)
    });
    factories.register("subagent-codex", move || {
        runtime_subagent_plugin(RuntimeSubagentConfig::new(
            "subagent-codex",
            "codex",
            "codex",
            "Codex delegated agent",
            codex_subagent_dir,
            codex_subagent_workspace,
            subagent_depth,
        ))
    });
    factories.register("subagent-claude", move || {
        runtime_subagent_plugin(RuntimeSubagentConfig::new(
            "subagent-claude",
            "claude",
            "claude",
            "Claude delegated agent",
            claude_subagent_dir,
            claude_subagent_workspace,
            subagent_depth,
        ))
    });
    if let Some(base) = cfg.subagent.worktree_base.as_deref() {
        let base = heycode_agent::GitCommitId::new(base)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        for (plugin_id, runtime_id, provider_id, display_name) in [
            (
                "subagent-worktree-codex",
                "codex",
                "worktree-codex",
                "Codex isolated worktree agent",
            ),
            (
                "subagent-worktree-claude",
                "claude",
                "worktree-claude",
                "Claude isolated worktree agent",
            ),
            (
                "subagent-worktree-opencode",
                "opencode",
                "worktree-opencode",
                "OpenCode isolated worktree agent",
            ),
        ] {
            let config = heycode_agent::WorktreeSubagentConfig::new(
                plugin_id,
                runtime_id,
                provider_id,
                display_name,
                options.sessions_dir.clone(),
                workspace_root.clone(),
                options.credentials_root.join(provider_id),
                base.clone(),
                heycode_agent::WorktreeRetention::RemoveAlways,
                subagent_depth,
            );
            factories.register(plugin_id, move || {
                heycode_agent::worktree_subagent_plugin(config)
            });
        }
    }
    // File-authored hooks and agents: the user's own always load; a project's
    // `.heycode/hooks` and `.heycode/agents` follow the instructions gate, and a
    // project hook that runs a command additionally needs process authority.
    let user_declarations = heycode_extension_host::user_declarations::UserDeclarationRoots {
        user_home: Some(options.credentials_root.clone()),
        workspace: options
            .trust
            .access(heycode_trust::ProjectInputKind::Instructions)?
            .is_allowed()
            .then(
                || heycode_extension_host::user_declarations::WorkspaceDeclarationAccess {
                    root: workspace_root.clone(),
                    executables_allowed: options
                        .trust
                        .access(heycode_trust::ProjectInputKind::Process)
                        .is_ok_and(|access| access.is_allowed()),
                },
            ),
    };
    #[cfg(unix)]
    let imported_agents = import_mount.agent_presets()?;
    #[cfg(unix)]
    factories.register("product-extensions", move || {
        heycode_extension_host::installed_product_extensions_plugin_with_imported_agents(
            product_extension_cache_root,
            product_extension_validator,
            installed_code_authority,
            user_declarations,
            imported_agents,
        )
    });
    #[cfg(unix)]
    let resources_mount = import_mount.clone();
    #[cfg(unix)]
    factories.register("config-import-resources", move || {
        heycode_extension_host::config_import::imported_resources_plugin(resources_mount)
    });
    #[cfg(unix)]
    let import_home = options.credentials_root.clone();
    #[cfg(unix)]
    let command_import_mount = import_mount.clone();
    #[cfg(unix)]
    factories.register("config-import", move || {
        config_import_integration::integration_plugin(
            import_home,
            import_settings_paths,
            command_import_mount,
        )
    });
    factories.register("code-mode", heycode_agent::code_mode_plugin);
    factories.register("plan", heycode_agent::plan_plugin);
    let job_limits = heycode_agent::JobLimits {
        max_running: cfg.jobs.max_concurrent,
        max_queued: cfg.jobs.max_queued,
        max_history: cfg.jobs.max_history,
        max_admissions: cfg.jobs.max_admissions,
    };
    if !(1..=256).contains(&job_limits.max_running)
        || job_limits.max_queued > 4096
        || !(1..=4096).contains(&job_limits.max_history)
    {
        anyhow::bail!(
            "invalid [jobs] admission budget: concurrency 1..256, queue 0..4096 and history 1..4096 required; zero admissions means unlimited"
        );
    }
    factories.register("agent", move || {
        heycode_agent::agent_plugin_with_job_limits(job_limits)
    });
    factories.register("product-hook-attachments", move || {
        heycode_tui::product_hook_attachments_plugin(product_hook_adapter)
    });
    factories.register("advisor", heycode_agent::advisor_plugin);
    factories.register("subagent-jobs", subagent_jobs_plugin);
    let execution_config = heycode_agent::ExecutionJobConfig {
        retained_bytes: cfg.tools.execution_retained_bytes,
        inline_bytes: cfg.tools.execution_inline_bytes,
        history_limit: cfg.tools.execution_history_limit,
        foreground_timeout_secs: cfg.tools.foreground_timeout_secs,
    };
    factories.register("execution-jobs", move || {
        heycode_agent::execution_jobs_plugin_with_config(execution_config)
    });
    factories.register(
        "goals",
        || goal_plugin(heycode_agent::GoalPolicy::default()),
    );
    factories.register("workflows", native_workflow_plugin);
    factories.register("schedules", durable_schedules_plugin);
    factories.register("teams", team_plugin);
    factories.register("work", heycode_agent::work_plugin_with_teams);
    let credentials_name = options.credentials_root.file_name().ok_or_else(|| {
        anyhow::anyhow!("credential root has no final component for reviewer storage")
    })?;
    let credentials_parent = options
        .credentials_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("credential root has no parent for reviewer storage"))?;
    let configured_review_root = std::fs::canonicalize(credentials_parent)
        .map_err(|_| anyhow::anyhow!("credential-root parent is unavailable"))?
        .join(credentials_name)
        .join("review-worktrees");
    let review_worktrees_root = if configured_review_root.starts_with(&workspace_root) {
        let parent = workspace_root.parent().ok_or_else(|| {
            anyhow::anyhow!("workspace has no parent for isolated reviewer storage")
        })?;
        parent.join(format!(
            ".heycode-review-worktrees-{}",
            trust_snapshot.identity().id().as_str()
        ))
    } else {
        configured_review_root
    };
    let review_config = heycode_agent::ReviewPluginConfig::new(
        workspace_root.clone(),
        review_worktrees_root,
        options.sessions_dir.join("reviews"),
    );
    factories.register("reviewer", move || review_plugin(review_config.clone()));
    let deferred_provider = Arc::new(heycode_agent::LexicalDeferredToolProvider::new(64)?);
    factories.register("deferred-tools", move || {
        heycode_agent::deferred_tools_plugin(deferred_provider.clone())
    });
    factories.register(
        "loop-budget-settings",
        heycode_agent::settings_loop_budget_plugin,
    );
    factories.register("agent-attachments", agent_attachments_plugin);
    factories.register("agent-documents", agent_documents_plugin);
    factories.register("runtime-native", native_runtime_plugin);
    factories.register("app-server", app_server_plugin);
    // `--model`/`--provider`/`--set llm.*` are this process's pins: they
    // outrank the persisted `[settings.routing]` route for this run only.
    let routing_overrides = heycode_routing::RoutingOverrides::new(
        cfg.is_patched("llm.provider")
            .then(|| cfg.llm.provider.clone()),
        cfg.is_patched("llm.model").then(|| cfg.llm.model.clone()),
    );
    // The connection UI must address the same credential as the registered
    // provider flow, including an explicit api_key_env override.
    let mut routing_profiles = setup::connection_profiles();
    if let Some(reference) = cfg.llm.api_key_env.as_ref()
        && let Some(profile) = routing_profiles
            .iter_mut()
            .find(|profile| profile.registry_name == cfg.llm.provider)
    {
        profile.credential_reference = Some(reference.clone());
    }
    factories.register("routing", move || {
        heycode_routing::routing_plugin_with_connections(
            routing_overrides.clone(),
            routing_profiles.clone(),
        )
    });
    factories.register("routing-auth", routing_auth_plugin);
    factories.register("app-server-controls", app_server_controls_plugin);
    factories.register("tui", move || {
        heycode_tui::tui_plugin_with_mcp_bridge(product_tui_bridge)
    });
    factories.register("panel-commands", heycode_tui::panel_commands_plugin);
    factories.register("autocompact", heycode_agent::autocompact_plugin);
    let workspace_user_home = options.credentials_root.clone();
    let workspace_trusted = options
        .trust
        .access(heycode_trust::ProjectInputKind::Instructions)?
        .is_allowed();
    let memory_home = options.credentials_root.clone();
    let auto_memory_root = options.sessions_dir.clone();
    factories.register("memory-commands", move || {
        heycode_tui::memory_commands::memory_commands_plugin(
            Some(memory_home),
            auto_memory_root,
            workspace_trusted,
        )
    });
    let workspace_runtime = requested_runtime.clone();
    let fixed_worktree_providers = cfg.subagent.worktree_base.is_some();
    #[cfg(unix)]
    let has_project_imports = import_mount.has_project_resources();
    #[cfg(not(unix))]
    let has_project_imports = false;
    factories.register("workspace-transitions", move || {
        workspace_integration::integration_plugin_with_imports(
            workspace_user_home,
            workspace_trusted,
            workspace_runtime,
            fixed_worktree_providers,
            has_project_imports,
        )
    });

    if options.profile_layers.is_empty() {
        let (order, scope) = if cfg.profile.plugins.is_empty() {
            (default_profile(&factories), PluginScope::BuiltIn)
        } else {
            (
                cfg.resolve_plugins(
                    &factories
                        .names()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )?,
                PluginScope::User,
            )
        };
        let selected: Vec<_> = order
            .into_iter()
            .map(|id| EffectivePluginSelection { id, scope })
            .collect();
        return Ok(factories.build_scoped(&selected)?);
    }
    if !cfg.profile.plugins.is_empty() {
        return Err(anyhow::anyhow!(
            "--profile cannot be combined with legacy `[profile] plugins`; move choices into the standalone profile"
        ));
    }
    let base = default_profile(&factories);
    let base_refs: Vec<_> = base.iter().map(String::as_str).collect();
    let tree = resolve_profile_tree(&base_refs, options.profile_layers)?;
    if tree.code_authority.is_some() {
        return Err(anyhow::anyhow!(
            "managed code authority requires a matching managed PL08 admission generation source; none is composed"
        ));
    }
    let plugins = factories.build_profile(&tree)?;
    Ok(plugins)
}

/// Inspect the production world graph without invoking plugin apply, network,
/// credential resolution, sessions, watchers, or child processes.
#[must_use]
pub fn inspect_world(options: &WorldOptions<'_>) -> heycode_core::CompositionReport {
    match resolve_world_plugins(options) {
        Ok(plugins) => heycode_core::inspect_composition(&plugins),
        Err(error) => heycode_core::CompositionReport {
            healthy: false,
            plugins: Vec::new(),
            diagnostics: vec![heycode_core::CompositionDiagnostic {
                code: "world_resolution".to_owned(),
                plugin: None,
                message: error.to_string(),
                related: Vec::new(),
            }],
        },
    }
}

/// Activate the production plugin selection inside an isolated disposable
/// world and return only body-free activation health.
///
/// The probe preserves the parsed config and resolved profile layers while
/// replacing workspace/product state, provider inference, credential reads,
/// settings watchers, session resume and configured MCP transports with inert
/// isolated inputs. The returned report names every deliberate suppression.
/// A successfully composed context is shut down before the temporary root is
/// removed.
#[must_use]
pub fn probe_world_activation(
    options: &WorldOptions<'_>,
) -> heycode_core::ActivationDiagnosticReport {
    let Ok(root) = tempfile::tempdir() else {
        return heycode_core::ActivationDiagnosticReport::unavailable(
            "activation_isolation_unavailable",
        );
    };
    let workspace = root.path().join("workspace");
    if std::fs::create_dir(&workspace).is_err() {
        return heycode_core::ActivationDiagnosticReport::unavailable(
            "activation_isolation_unavailable",
        );
    }

    let Ok(snapshot) = options.trust.snapshot() else {
        return heycode_core::ActivationDiagnosticReport::unavailable(
            "activation_trust_unavailable",
        );
    };
    let Ok(trust) = WorkspaceTrustService::memory(&workspace, project_content_policy()) else {
        return heycode_core::ActivationDiagnosticReport::unavailable(
            "activation_trust_unavailable",
        );
    };
    match snapshot.decision() {
        heycode_trust::WorkspaceTrustDecision::Unknown => {}
        decision @ (heycode_trust::WorkspaceTrustDecision::Restricted
        | heycode_trust::WorkspaceTrustDecision::Trusted) => {
            if trust.set_session(decision, 0).is_err() {
                return heycode_core::ActivationDiagnosticReport::unavailable(
                    "activation_trust_unavailable",
                );
            }
        }
    }

    let mut config = options.config.clone();
    let configured_mcp = !config.mcp.servers.is_empty();
    config.mcp.servers.clear();
    let annotate_suppressions = |report: heycode_core::ActivationDiagnosticReport| {
        let mut report = report
            .with_suppressed("provider_credentials_and_inference")
            .with_suppressed("persistent_product_state");
        if configured_mcp {
            report = report.with_suppressed("configured_mcp_servers");
        }
        if options.settings_watch {
            report = report.with_suppressed("settings_watchers");
        }
        if options.resume.is_some() {
            report = report.with_suppressed("session_resume");
        }
        report
    };
    let isolated = WorldOptions {
        config: &config,
        trust,
        config_migration: options.config_migration,
        profile_layers: options.profile_layers,
        sessions_dir: root.path().join("sessions"),
        attachments_dir: root.path().join("attachments"),
        attachment_max_bytes: options.attachment_max_bytes,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: ApprovalPrompter::Proxied,
        settings_user_path: root.path().join("settings.toml"),
        credentials_root: root.path().join("credentials"),
        catalog_cache_path: root.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: workspace,
        fake: Some(Arc::new(
            heycode_llm::testing::FakeProvider::new(Vec::new()),
        )),
        resume: None,
    };
    let Ok(plugins) = resolve_world_plugins(&isolated) else {
        return annotate_suppressions(heycode_core::ActivationDiagnosticReport::unavailable(
            "activation_world_resolution",
        ));
    };
    let composed = heycode_core::compose_scoped_activation(&plugins);
    let diagnostic = annotate_suppressions(composed.report.diagnostic());
    if let Ok(mut context) = composed.context {
        context.shutdown();
    }
    diagnostic
}

/// Run S11 checks in a restricted diagnostic world. This activates only the
/// doctor registry, a non-watching settings reader, non-resolving environment
/// credential registration, and K08's dry composition check. It
/// creates no session, credential file, catalog, network client or child process.
///
/// # Errors
/// Diagnostic plugin composition, registry state or check execution failures.
pub async fn diagnose_world(
    options: &WorldOptions<'_>,
    cancellation: tokio_util::sync::CancellationToken,
) -> anyhow::Result<DoctorReport> {
    let composition = inspect_world(options);
    let environment = EnvironmentCredentialProvider::process()?;
    let settings = FileSettingsConfig::user(options.settings_user_path.clone()).without_watch();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        doctor_plugin(),
        config_migration_doctor_plugin(options.config_migration.cloned()),
        file_settings_plugin(settings),
        settings_doctor_plugin(),
        credentials_plugin(),
        environment_credentials_plugin(environment),
        credentials_doctor_plugin(),
        composition_doctor_plugin(composition),
    ];
    let mut context = heycode_core::compose(&plugins)?;
    let registry = context
        .get::<DoctorRegistry>(SERVICE_DOCTOR)
        .ok_or_else(|| anyhow::anyhow!("doctor registry missing after diagnostic composition"))?;
    let report = registry.run(cancellation).await;
    context.shutdown();
    Ok(report?)
}

/// Compose the full plugin stack into a live context.
///
/// # Errors
/// World resolution, missing provider keys, resume corruption, dependency,
/// collision, or plugin apply failure.
pub fn compose_world(options: &WorldOptions<'_>) -> anyhow::Result<Context> {
    let plugins = resolve_world_plugins(options)?;
    let composed = heycode_core::compose_scoped_activation(&plugins);
    match composed.context {
        Ok(context) => Ok(context),
        // A successful composition's report says only "every plugin activated",
        // which `ctx.plugins()` already says. Its information is all in the
        // failure: which plugin, which stage, and how much of the world never
        // got a turn. Without this the operator sees a bare message and cannot
        // tell a bad plugin from a bad ordering.
        Err(error) => Err(match describe_activation_failure(&composed.report) {
            Some(detail) => anyhow::Error::from(error).context(detail),
            None => error.into(),
        }),
    }
}

/// One line naming which plugin failed, at which stage, and how much of the
/// world never got a turn.
///
/// Returns `None` for a report with no failed row, so a caller can never
/// dress a non-activation error up as one.
fn describe_activation_failure(report: &heycode_core::ActivationReport) -> Option<String> {
    let failure = report.failure()?;
    let stage = failure
        .outcome
        .failure()
        .map_or("unknown", |detail| detail.stage.as_str());
    let never_ran = report
        .activations()
        .iter()
        .filter(|row| matches!(row.outcome, PluginActivationOutcome::NotAttempted))
        .count();
    Some(format!(
        "plugin `{}` failed to activate at the {stage} stage; \
         {never_ran} later plugin(s) never ran",
        failure.plugin
    ))
}

/// The built-in composition order, used when `[profile] plugins` is absent.
///
/// Ordering constraints encoded here: `tools`/`prompt`/`commands` must precede
/// every plugin that registers into them; delegated runtimes follow `runtimes`
/// and `subprocess-local`; `init` follows `commands`; `plan` follows `commands`
/// and `approval`; `agent` follows `agent-options`; `runtime-native` follows
/// both `agent` and `runtimes`; `tui` is last.
/// Whether an AWS region resolves from this host for integrations that cannot
/// collect a draft coordinate before plugin activation.
///
/// Only `Resolved` counts: `Unresolved`, `Malformed` and `Undetermined` all
/// answer `None`, so the tri-state that governs the status report governs the
/// activation decision too. The ordinary Bedrock catalog mounts without this
/// gate so its provider-owned region form can perform draft discovery; Mantle
/// still requires an already-resolved region.
///
/// Separated from the wiring because the decision is the part worth testing and
/// the process environment is not injectable without `unsafe` (GOTCHAS #4).
fn resolve_aws_region(
    host: &dyn heycode_authorization_aws::AwsHost,
) -> Option<heycode_authorization_aws::AwsRegion> {
    let profile = heycode_authorization_aws::AwsProfileResolution::resolve(host);
    heycode_authorization_aws::AwsRegionResolution::resolve(host, &profile)
        .region()
        .cloned()
}

#[cfg(test)]
fn aws_region_resolves(host: &dyn heycode_authorization_aws::AwsHost) -> bool {
    resolve_aws_region(host).is_some()
}

fn default_profile(available: &PluginFactories) -> Vec<String> {
    let names = available.names();
    BUILTIN_PLUGIN_ORDER
        .iter()
        .filter(|n| names.iter().any(|have| have == *n))
        .map(|n| (*n).to_owned())
        .collect()
}

/// Resolve the mandatory effective sandbox policy from config.
///
/// # Errors
/// Requesting a sandbox the platform cannot provide fails composition loudly
/// (never silently unconfined).
fn sandbox_service(
    cfg: &Config,
    workspace_root: &std::path::Path,
) -> anyhow::Result<heycode_exec::SandboxService> {
    use heycode_config::SandboxModeCfg;
    let (mode, backend) = match cfg.sandbox.mode {
        SandboxModeCfg::Off => (
            heycode_exec::SandboxMode::Off,
            heycode_sandbox::platform_default().ok(),
        ),
        SandboxModeCfg::ReadOnly => (
            heycode_exec::SandboxMode::ReadOnly,
            Some(heycode_sandbox::platform_default()?),
        ),
        SandboxModeCfg::Workspace => (
            heycode_exec::SandboxMode::WorkspaceWrite,
            Some(heycode_sandbox::platform_default()?),
        ),
    };
    Ok(heycode_exec::SandboxService::new(
        mode,
        workspace_root.to_path_buf(),
        backend,
    )?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_core::{CoreResult, PluginDescriptor, ScopedPlugin};

    struct Fails(&'static str);
    impl Plugin for Fails {
        fn name(&self) -> &'static str {
            self.0
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.0, "0.0.0", &[])
        }
        fn apply(&self, _ctx: &mut Context) -> CoreResult<()> {
            Err(heycode_core::CoreError::other("deliberate"))
        }
    }

    struct Works(&'static str);
    impl Plugin for Works {
        fn name(&self) -> &'static str {
            self.0
        }
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(self.0, "0.0.0", &[])
        }
        fn apply(&self, _ctx: &mut Context) -> CoreResult<()> {
            Ok(())
        }
    }

    fn scoped(plugin: Box<dyn Plugin>) -> ScopedPlugin {
        ScopedPlugin::new(PluginScope::BuiltIn, plugin)
    }

    /// An operator whose heycode will not start needs to know which plugin to
    /// remove. The bare error names a message; this names the culprit, the
    /// stage, and how much of the world never got a turn.
    #[test]
    fn a_failed_activation_is_described_by_plugin_stage_and_lost_plugins() {
        let plugins = vec![
            scoped(Box::new(Works("first"))),
            scoped(Box::new(Fails("culprit"))),
            scoped(Box::new(Works("never-ran-a"))),
            scoped(Box::new(Works("never-ran-b"))),
        ];
        let composed = heycode_core::compose_scoped_activation(&plugins);
        assert!(composed.context.is_err());

        let detail =
            describe_activation_failure(&composed.report).expect("a failed report describes");
        assert!(detail.contains("culprit"), "{detail}");
        assert!(detail.contains("apply"), "{detail}");
        assert!(detail.contains("2 later plugin(s) never ran"), "{detail}");
        assert!(
            !detail.contains("first"),
            "an activated plugin must not be blamed: {detail}"
        );
    }

    /// Host-only gates never promote an absent or malformed region. Draft-aware
    /// Bedrock setup mounts independently; integrations without that boundary
    /// may use this exact predicate.
    #[test]
    fn aws_host_region_gate_never_promotes_missing_or_malformed_values() {
        use heycode_authorization_aws::MapAwsHost;

        assert!(
            aws_region_resolves(&MapAwsHost::new().with_var("AWS_REGION", "us-east-1")),
            "a resolved region mounts them"
        );
        assert!(
            aws_region_resolves(&MapAwsHost::new().with_var("AWS_DEFAULT_REGION", "eu-central-1")),
            "the documented fallback variable resolves too"
        );
        assert!(
            !aws_region_resolves(&MapAwsHost::new()),
            "nothing configured is not a host-resolved region"
        );
        assert!(
            !aws_region_resolves(&MapAwsHost::new().with_var("AWS_REGION", "not a region")),
            "a malformed region is not a resolved one"
        );
    }

    #[test]
    fn aws_inference_factory_requires_exact_region_protocol_and_message_cap() {
        use heycode_authorization_aws::MapAwsHost;

        let region = resolve_aws_region(&MapAwsHost::new().with_var("AWS_REGION", "us-east-1"));
        let mut config = Config::defaults();
        config.llm.provider = "bedrock".to_owned();
        config.llm.model = "anthropic.claude-sonnet-4-6-v1:0".to_owned();
        let configured = aws_inference_plugin_for_config(&config, region.clone())
            .unwrap()
            .unwrap();
        assert_eq!(configured.name(), "inference-bedrock-converse");

        config.llm.provider = "bedrock-mantle".to_owned();
        assert!(validate_llm_protocol(&config.llm.provider, config.llm.protocol).is_err());
        config.llm.protocol = LlmProtocolCfg::AnthropicMessages;
        assert!(aws_inference_plugin_for_config(&config, region.clone()).is_err());
        config.llm.max_output_tokens = Some(4096);
        let configured = aws_inference_plugin_for_config(&config, region)
            .unwrap()
            .unwrap();
        assert_eq!(configured.name(), "inference-bedrock-mantle-messages");
    }

    #[test]
    fn saved_cloud_coordinates_override_host_defaults_only_for_the_matching_provider() {
        let selection = heycode_routing::RoutingSelection::new("native", "bedrock", "model", None)
            .unwrap()
            .with_parameters(std::collections::BTreeMap::from([(
                "region".into(),
                "eu-central-1".into(),
            )]))
            .unwrap();
        let mut cfg = Config::defaults();
        cfg.llm.provider = "bedrock".into();
        assert_eq!(
            connection_parameters(&cfg, Some(&selection))
                .unwrap()
                .get("region")
                .map(String::as_str),
            Some("eu-central-1")
        );
        cfg.llm.provider = "google".into();
        assert!(
            connection_parameters(&cfg, Some(&selection))
                .unwrap()
                .is_empty()
        );
        cfg.llm.provider = "bedrock".into();
        let invalid = selection
            .with_parameters(std::collections::BTreeMap::from([(
                "project".into(),
                "unrelated".into(),
            )]))
            .unwrap();
        assert!(connection_parameters(&cfg, Some(&invalid)).is_err());
    }

    #[test]
    fn saved_vertex_coordinates_work_without_environment_configuration() {
        let parameters = std::collections::BTreeMap::from([
            ("project".into(), "my-project".into()),
            ("location".into(), "global".into()),
        ]);
        let profile = gcp_profile_request(
            &heycode_authorization_gcp::testing::MapGcpEnvironment::new(),
            &parameters,
        )
        .unwrap();
        assert_eq!(profile.project.as_deref(), Some("my-project"));
        assert_eq!(profile.location.as_deref(), Some("global"));
    }

    #[test]
    fn vertex_factory_input_requires_explicit_project_location_and_oauth_kind() {
        use heycode_authorization_gcp::testing::MapGcpEnvironment;

        assert!(explicit_gcp_profile_request(&MapGcpEnvironment::new()).is_err());
        let environment = MapGcpEnvironment::new()
            .with_var(
                heycode_authorization_gcp::ENV_GOOGLE_CLOUD_PROJECT,
                "heycode-project",
            )
            .with_var(
                heycode_authorization_gcp::ENV_GOOGLE_CLOUD_LOCATION,
                "global",
            );
        let profile = explicit_gcp_profile_request(&environment).unwrap();
        assert_eq!(profile.project.as_deref(), Some("heycode-project"));
        assert_eq!(profile.location.as_deref(), Some("global"));
        let credential = provider_credential_query(
            "vertex-claude",
            heycode_provider_google::GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE,
        )
        .unwrap();
        assert_eq!(credential.kind.as_str(), "oauth-token");
        let config = heycode_provider_google::GoogleInferencePluginConfig::lazy_claude_vertex(
            profile,
            credential,
            heycode_provider_google::ClaudeVertexControls::sonnet_five_default(),
        )
        .unwrap();
        assert_eq!(
            heycode_provider_google::google_inference_plugin(config).name(),
            "inference-google-claude-vertex"
        );
    }

    /// A provider heycode ships a profile and catalog for is not "unknown" — telling
    /// that user to check for a typo sends them looking for the wrong thing.
    #[test]
    fn a_configured_only_provider_is_distinguished_from_an_unknown_one() {
        let configured = unselectable_provider("zai-coding");
        assert!(configured.contains("restricted by Z.ai"), "{configured}");
        assert!(!configured.contains("unknown provider"), "{configured}");

        let unknown = unselectable_provider("frobnicator");
        assert!(unknown.contains("unknown provider"), "{unknown}");
        assert!(!unknown.contains("no inference route"), "{unknown}");

        // Both name what a user can actually select.
        for message in [configured, unknown] {
            assert!(message.contains("deepseek"), "{message}");
            assert!(message.contains("openrouter"), "{message}");
        }
    }

    /// The two lists must stay disjoint: a provider in both would render as
    /// configured-only while actually being selectable.
    #[test]
    fn the_inference_and_configured_only_provider_lists_are_disjoint() {
        for provider in INFERENCE_PROVIDERS {
            assert!(
                !CONFIGURED_ONLY_PROVIDERS.contains(provider),
                "`{provider}` is in both lists"
            );
        }
    }

    #[test]
    fn optional_local_credentials_are_checked_only_when_selected() {
        assert!(provider_requires_credential("deepseek"));
        assert!(provider_requires_credential("openrouter"));
        assert!(!provider_requires_credential("ollama"));
        assert!(!provider_requires_credential("custom-openai"));
        assert!(INFERENCE_PROVIDERS.contains(&"ollama"));
        assert!(INFERENCE_PROVIDERS.contains(&"custom-openai"));
        assert!(INFERENCE_PROVIDERS.contains(&"google"));

        let root = tempfile::tempdir().unwrap();
        let mut config = Config::defaults();
        config.llm.provider = "custom-openai".to_owned();
        assert!(!provider_uses_credential(&config));
        assert!(provider_key_present_at(&config, root.path()).unwrap());

        config.llm.api_key_env = Some("HEYCODE_CUSTOM_SERVER_TEST_KEY".to_owned());
        assert!(provider_uses_credential(&config));
        assert!(!provider_key_present_at(&config, root.path()).unwrap());
    }

    /// Guards the other direction: a clean composition must never be dressed up
    /// as an activation failure by a caller that already holds an error.
    #[test]
    fn a_successful_report_has_nothing_to_describe() {
        let plugins = vec![scoped(Box::new(Works("a"))), scoped(Box::new(Works("b")))];
        let composed = heycode_core::compose_scoped_activation(&plugins);
        assert!(composed.context.is_ok());
        assert!(describe_activation_failure(&composed.report).is_none());
    }
}

/// Standalone release checking and updates.
pub mod update;
