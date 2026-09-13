//! Registry-backed app-server control plane.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use heycode_authorization::{AuthorizationMethod, AuthorizationOperationId, AuthorizationService};
use heycode_authorization_api_key::{InteractiveSecretPrompt, SecretPromptNotification};
use heycode_credentials::{CredentialDescriptor, CredentialValidation, CredentialsService};
use heycode_llm::{
    CapabilitySupport, CatalogError, CatalogFailureKind, CatalogFreshness, CatalogRefreshMode,
    CatalogRegistry, ModelDescriptor, ModelLifecycleStatus, ProviderProfile, ProviderRegistry,
};
use heycode_mcp::McpRegistry;
use heycode_routing::{RoutingError, RoutingSelection, RoutingService};
use heycode_runtime::{AgentRuntimeKind, AgentRuntimeRegistry};
use heycode_settings::{SettingsApplies, SettingsNamespace, SettingsService, SettingsSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{AppEventSink, AppServer, AppServerError, AppServerErrorCode, AppServerEvent};

use heycode_sdk::{
    APP_CONTROL_METHODS, AppAuthorizationFlow, AppAuthorizationReceipt, AppCapabilityEvidence,
    AppCatalogFreshness, AppCatalogRefresh, AppControlWarning, AppCredentialStatus,
    AppCredentialValidation, AppInitializeResult, AppLogoutResult, AppModelCapabilities,
    AppModelCatalog, AppModelLifecycle, AppModelRow, AppPluginContribution, AppPluginInventory,
    AppPluginRow, AppProviderCatalog, AppProviderRow, AppRouteSelection, AppRuntimeCapabilities,
    AppRuntimeCatalog, AppRuntimeConfigurationCapabilities, AppRuntimeRow, AppRuntimeWorkspace,
    AppServerCapabilities, AppServerIdentity, AppSettingsSnapshot,
};

pub(crate) fn initialize_result(controls: bool) -> AppInitializeResult {
    AppInitializeResult {
        protocol_version: heycode_sdk::APP_SERVER_PROTOCOL_VERSION,
        server: AppServerIdentity {
            name: "heycode".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        capabilities: AppServerCapabilities {
            turns: true,
            attachments: true,
            cancel: true,
            authorization: controls,
            models: controls,
            runtimes: controls,
            workspace: controls,
            mcp: controls,
            plugins: controls,
            settings: controls,
        },
    }
}

struct AuthorizationControls {
    authorization: Arc<AuthorizationService>,
    credentials: Arc<CredentialsService>,
    providers: Arc<ProviderRegistry>,
    prompt: Arc<InteractiveSecretPrompt>,
    notifications: Mutex<tokio::sync::mpsc::UnboundedReceiver<SecretPromptNotification>>,
    operation_gate: Mutex<()>,
    next_operation: AtomicU64,
}

pub(crate) struct AppControlPlane {
    server: Weak<AppServer>,
    authorization: AuthorizationControls,
    providers: Arc<ProviderRegistry>,
    models: Arc<CatalogRegistry>,
    runtimes: Arc<AgentRuntimeRegistry>,
    routing: Arc<RoutingService>,
    mcp: Arc<McpRegistry>,
    settings: Arc<SettingsService>,
    inventory: heycode_core::PluginInventory,
}

impl AppControlPlane {
    #[allow(clippy::too_many_arguments)]
    fn new(
        server: Weak<AppServer>,
        authorization: Arc<AuthorizationService>,
        credentials: Arc<CredentialsService>,
        providers: Arc<ProviderRegistry>,
        prompt: Arc<InteractiveSecretPrompt>,
        notifications: tokio::sync::mpsc::UnboundedReceiver<SecretPromptNotification>,
        models: Arc<CatalogRegistry>,
        runtimes: Arc<AgentRuntimeRegistry>,
        routing: Arc<RoutingService>,
        mcp: Arc<McpRegistry>,
        settings: Arc<SettingsService>,
        inventory: heycode_core::PluginInventory,
    ) -> Self {
        Self {
            server,
            authorization: AuthorizationControls {
                authorization,
                credentials,
                providers: providers.clone(),
                prompt,
                notifications: Mutex::new(notifications),
                operation_gate: Mutex::new(()),
                next_operation: AtomicU64::new(1),
            },
            providers,
            models,
            runtimes,
            routing,
            mcp,
            settings,
            inventory,
        }
    }

    pub(crate) async fn dispatch(
        &self,
        method: &str,
        params: Value,
        events: mpsc::Sender<crate::AppServerNotification>,
        sequence: Arc<AtomicU64>,
        cancellation: CancellationToken,
    ) -> Result<Value, AppServerError> {
        match method {
            "authorization/list" => {
                parse::<EmptyParams>(params)?;
                to_value(self.authorization.list()?)
            }
            "authorization/start" => {
                let params = parse::<AuthorizationStartParams>(params)?;
                let current = self
                    .routing
                    .selection()
                    .map_err(|_| AppServerError::unavailable())?;
                let profile = self
                    .providers
                    .profiles()
                    .into_iter()
                    .find(|profile| profile.registry_name == current.provider())
                    .ok_or_else(AppServerError::invalid)?;
                let reference = profile
                    .credential_reference
                    .ok_or_else(AppServerError::invalid)?;
                let sink = AppEventSink::control(sequence, events);
                to_value(
                    self.authorization
                        .start(
                            &params.flow_id,
                            current.provider(),
                            &reference,
                            sink,
                            cancellation,
                        )
                        .await?,
                )
            }
            "authorization/answer" => {
                let params = parse::<AuthorizationAnswerParams>(params)?;
                self.authorization.answer(params)?;
                Ok(Value::Null)
            }
            "authorization/cancel" => {
                let params = parse::<AuthorizationPromptParams>(params)?;
                self.authorization.cancel(params.prompt_id)?;
                Ok(Value::Null)
            }
            "authorization/logout" => {
                let params = parse::<AuthorizationLogoutParams>(params)?;
                let provider = match params.provider {
                    Some(provider) => provider,
                    None => self
                        .routing
                        .selection()
                        .map_err(|_| AppServerError::unavailable())?
                        .provider()
                        .to_owned(),
                };
                to_value(self.authorization.logout(&provider)?)
            }
            "providers/list" => {
                parse::<EmptyParams>(params)?;
                to_value(self.provider_catalog()?)
            }
            "providers/select" => {
                let params = parse::<ProviderSelectParams>(params)?;
                let selection = self.server()?.commit_route(|| {
                    self.routing
                        .select_provider(&params.provider)
                        .map_err(map_routing_error)
                })?;
                to_value(route(selection))
            }
            "models/list" => {
                let params = parse::<ModelListParams>(params)?;
                to_value(self.model_catalog(params, cancellation).await?)
            }
            "models/select" => {
                let params = parse::<ModelSelectParams>(params)?;
                let selection = self.server()?.commit_route(|| {
                    self.routing
                        .select_model(&params.model)
                        .map_err(map_routing_error)
                })?;
                to_value(route(selection))
            }
            "runtimes/list" => {
                parse::<EmptyParams>(params)?;
                to_value(self.runtime_catalog()?)
            }
            "runtime/select" => {
                let params = parse::<RuntimeSelectParams>(params)?;
                to_value(self.select_runtime(&params.runtime)?)
            }
            "workspace/select" => {
                let params = parse::<WorkspaceSelectParams>(params)?;
                to_value(self.server()?.select_workspace(&params.cwd)?)
            }
            "mcp/list" => {
                parse::<EmptyParams>(params)?;
                let snapshot = self
                    .mcp
                    .snapshot()
                    .map_err(|_| AppServerError::unavailable())?;
                to_value(&*snapshot)
            }
            "plugins/list" => {
                parse::<EmptyParams>(params)?;
                to_value(self.plugin_inventory()?)
            }
            "settings/list" => {
                parse::<EmptyParams>(params)?;
                to_value(self.settings_list()?)
            }
            "settings/get" => {
                let params = parse::<SettingsGetParams>(params)?;
                to_value(self.setting(&params.namespace)?)
            }
            "settings/replace" => {
                let params = parse::<SettingsReplaceParams>(params)?;
                to_value(self.replace_setting(params)?)
            }
            _ => Err(AppServerError::classified(
                AppServerErrorCode::MethodNotFound,
            )),
        }
    }

    fn server(&self) -> Result<Arc<AppServer>, AppServerError> {
        self.server
            .upgrade()
            .ok_or_else(AppServerError::unavailable)
    }

    fn runtime_catalog(&self) -> Result<AppRuntimeCatalog, AppServerError> {
        let current = self.routing.selection().map_err(map_routing_error)?;
        let runtimes = self
            .runtimes
            .descriptors()
            .map_err(|_| AppServerError::unavailable())?
            .into_iter()
            .map(runtime_row)
            .collect();
        Ok(AppRuntimeCatalog {
            current: route(current),
            runtimes,
        })
    }

    fn select_runtime(&self, id: &str) -> Result<AppRouteSelection, AppServerError> {
        let runtime = self
            .runtimes
            .get(id)
            .map_err(|_| AppServerError::unavailable())?
            .ok_or_else(AppServerError::invalid)?;
        let current = self.routing.selection().map_err(map_routing_error)?;
        if current.runtime() == id {
            return Ok(route(current));
        }
        let selection = self.server()?.select_runtime_after(runtime, || {
            self.routing.select_runtime(id).map_err(map_routing_error)
        })?;
        Ok(route(selection))
    }

    fn provider_catalog(&self) -> Result<AppProviderCatalog, AppServerError> {
        let current = self
            .routing
            .selection()
            .map_err(|_| AppServerError::unavailable())?;
        Ok(AppProviderCatalog {
            current: route(current),
            providers: self
                .providers
                .profiles()
                .into_iter()
                .map(provider_row)
                .collect(),
        })
    }

    async fn model_catalog(
        &self,
        params: ModelListParams,
        cancellation: CancellationToken,
    ) -> Result<AppModelCatalog, AppServerError> {
        let current = self
            .routing
            .selection()
            .map_err(|_| AppServerError::unavailable())?;
        let provider = params
            .provider
            .unwrap_or_else(|| current.provider().to_owned());
        let profile = self
            .providers
            .profiles()
            .into_iter()
            .find(|row| row.registry_name == provider)
            .ok_or_else(AppServerError::invalid)?;
        let mode = match params.refresh {
            AppCatalogRefresh::PreferCache => CatalogRefreshMode::PreferCache,
            AppCatalogRefresh::Force => CatalogRefreshMode::Force,
        };
        match self.models.refresh(&provider, mode, cancellation).await {
            Ok(view) => Ok(AppModelCatalog {
                provider,
                current_model: current.model().to_owned(),
                default_model: profile.default_model,
                revision: Some(view.snapshot.revision),
                fetched_at_ms: Some(view.snapshot.fetched_at_ms),
                freshness: match view.freshness {
                    CatalogFreshness::Live => AppCatalogFreshness::Live,
                    CatalogFreshness::FreshCache => AppCatalogFreshness::FreshCache,
                    CatalogFreshness::StaleFallback => AppCatalogFreshness::StaleFallback,
                },
                warning: view.warning.as_ref().map(catalog_warning),
                models: view
                    .snapshot
                    .models
                    .iter()
                    .map(|model| model_row(model, unix_time_ms()))
                    .collect(),
            }),
            Err(CatalogError::Cancelled { .. }) => Err(AppServerError::cancelled()),
            Err(error) => {
                let default_model = profile.default_model;
                let mut fallback_ids = BTreeSet::from([default_model.clone()]);
                fallback_ids.insert(current.model().to_owned());
                Ok(AppModelCatalog {
                    provider,
                    current_model: current.model().to_owned(),
                    default_model: default_model.clone(),
                    revision: None,
                    fetched_at_ms: None,
                    freshness: AppCatalogFreshness::DefaultFallback,
                    warning: Some(catalog_warning(&error)),
                    models: fallback_ids
                        .into_iter()
                        .map(|id| {
                            let selectable = id == default_model;
                            let mut row = model_row(&ModelDescriptor::unknown(id), unix_time_ms());
                            row.selectable = selectable;
                            row
                        })
                        .collect(),
                })
            }
        }
    }

    fn plugin_inventory(&self) -> Result<AppPluginInventory, AppServerError> {
        let snapshot = self
            .inventory
            .snapshot()
            .map_err(|_| AppServerError::unavailable())?;
        Ok(AppPluginInventory {
            plugins: snapshot
                .plugins
                .into_iter()
                .map(|plugin| AppPluginRow {
                    id: plugin.descriptor.id.to_owned(),
                    version: plugin.descriptor.version.to_owned(),
                    source: plugin.descriptor.source.as_str().to_owned(),
                    scope: plugin.scope.as_str().to_owned(),
                })
                .collect(),
            contributions: snapshot
                .contributions
                .into_iter()
                .map(|row| AppPluginContribution {
                    plugin: row.plugin.to_owned(),
                    kind: row.kind.as_str().to_owned(),
                    name: row.name,
                })
                .collect(),
        })
    }

    fn settings_list(&self) -> Result<Vec<AppSettingsSnapshot>, AppServerError> {
        Ok(self
            .settings
            .describe()
            .map_err(|_| AppServerError::unavailable())?
            .iter()
            .map(|snapshot| setting_row(snapshot, self.settings.writable()))
            .collect())
    }

    fn setting(&self, namespace: &str) -> Result<AppSettingsSnapshot, AppServerError> {
        let namespace = SettingsNamespace::new(namespace).map_err(|_| AppServerError::invalid())?;
        let snapshot = self
            .settings
            .get(&namespace)
            .map_err(|_| AppServerError::unavailable())?
            .ok_or_else(AppServerError::invalid)?;
        Ok(setting_row(&snapshot, self.settings.writable()))
    }

    fn replace_setting(
        &self,
        params: SettingsReplaceParams,
    ) -> Result<AppSettingsSnapshot, AppServerError> {
        let namespace =
            SettingsNamespace::new(params.namespace).map_err(|_| AppServerError::invalid())?;
        let current = self
            .settings
            .get(&namespace)
            .map_err(|_| AppServerError::unavailable())?
            .ok_or_else(AppServerError::invalid)?;
        // Exposure must be PROVED, not attested: without a verified projection
        // this namespace never renders values, so it must not accept writes to
        // them either.
        if current.wire_projection().is_none() {
            return Err(AppServerError::unavailable());
        }
        let next = self
            .settings
            .replace_user(&namespace, params.user, Some(params.expected_revision))
            .map_err(|error| match error {
                // A managed lock is a conflict with administrator policy, not a
                // broken service: the write is refused because current state
                // forbids it, which is exactly what Conflict means here.
                heycode_settings::SettingsError::Conflict { .. }
                | heycode_settings::SettingsError::ManagedLock { .. } => {
                    AppServerError::classified(AppServerErrorCode::Conflict)
                }
                _ => AppServerError::unavailable(),
            })?;
        Ok(setting_row(&next, self.settings.writable()))
    }
}

impl AuthorizationControls {
    fn list(&self) -> Result<Vec<AppAuthorizationFlow>, AppServerError> {
        Ok(self
            .authorization
            .descriptors()
            .map_err(|_| AppServerError::unavailable())?
            .into_iter()
            .map(|descriptor| {
                let provider = provider_owner(&self.providers, &descriptor);
                AppAuthorizationFlow {
                    id: descriptor.id.as_str().to_owned(),
                    label: descriptor.label,
                    method: authorization_method(descriptor.method).to_owned(),
                    interactive: descriptor.interactive,
                    provider,
                    credential: uninspected_credential_status(&descriptor.query),
                }
            })
            .collect())
    }

    async fn start(
        &self,
        flow_id: &str,
        provider: &str,
        credential_reference: &str,
        sink: AppEventSink,
        cancellation: CancellationToken,
    ) -> Result<AppAuthorizationReceipt, AppServerError> {
        let _operation_guard = self.operation_gate.lock().await;
        let descriptor = self
            .authorization
            .descriptors()
            .map_err(|_| AppServerError::unavailable())?
            .into_iter()
            .find(|descriptor| descriptor.id.as_str() == flow_id)
            .ok_or_else(AppServerError::invalid)?;
        if descriptor.query.reference.as_str() != credential_reference {
            return Err(AppServerError::invalid());
        }
        let operation = self.next_operation()?;
        let mut notifications = self.notifications.lock().await;
        while notifications.try_recv().is_ok() {}
        let authorize = self.authorization.authorize_correlated(
            &descriptor.id,
            descriptor.query.clone(),
            Some(operation),
            cancellation,
        );
        tokio::pin!(authorize);
        let mut prompt_ids = BTreeSet::new();
        loop {
            tokio::select! {
                result = &mut authorize => {
                    while let Ok(notification) = notifications.try_recv() {
                        forward_prompt(notification, operation, &mut prompt_ids, &sink).await?;
                    }
                    let receipt = result.map_err(map_authorization_error)?;
                    return Ok(AppAuthorizationReceipt {
                        provider: provider.to_owned(),
                        flow: receipt.flow.as_str().to_owned(),
                        committed_by: receipt.committed_by.as_str().to_owned(),
                        credential: credential_status(receipt.credential),
                    });
                }
                notification = notifications.recv() => {
                    let notification = notification.ok_or_else(AppServerError::unavailable)?;
                    forward_prompt(notification, operation, &mut prompt_ids, &sink).await?;
                }
            }
        }
    }

    fn answer(&self, params: AuthorizationAnswerParams) -> Result<(), AppServerError> {
        if params.secret.is_empty()
            || params.secret.len() > 64 * 1024
            || params.secret.trim() != params.secret
            || params.secret.chars().any(char::is_control)
        {
            return Err(AppServerError::invalid());
        }
        if self.prompt.answer(
            params.prompt_id,
            heycode_credentials::CredentialSecret::new(params.secret),
        ) {
            Ok(())
        } else {
            Err(AppServerError::classified(AppServerErrorCode::Conflict))
        }
    }

    fn cancel(&self, prompt_id: u64) -> Result<(), AppServerError> {
        if self.prompt.cancel(prompt_id) {
            Ok(())
        } else {
            Err(AppServerError::classified(AppServerErrorCode::Conflict))
        }
    }

    fn logout(&self, provider: &str) -> Result<AppLogoutResult, AppServerError> {
        let profile = self
            .providers
            .profiles()
            .into_iter()
            .find(|profile| profile.registry_name == provider)
            .ok_or_else(AppServerError::invalid)?;
        let reference = profile
            .credential_reference
            .ok_or_else(AppServerError::invalid)?;
        let matches = self
            .authorization
            .descriptors()
            .map_err(|_| AppServerError::unavailable())?
            .into_iter()
            .filter(|descriptor| descriptor.query.reference.as_str() == reference)
            .collect::<Vec<_>>();
        let [descriptor] = matches.as_slice() else {
            return Err(AppServerError::invalid());
        };
        let deleted = self
            .credentials
            .delete(&descriptor.query)
            .map_err(|_| AppServerError::unavailable())?;
        Ok(AppLogoutResult {
            deleted_from: deleted.map(|provider| provider.as_str().to_owned()),
        })
    }

    fn next_operation(&self) -> Result<AuthorizationOperationId, AppServerError> {
        let value = self
            .next_operation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Internal))?;
        AuthorizationOperationId::new(value)
            .ok_or_else(|| AppServerError::classified(AppServerErrorCode::Internal))
    }
}

async fn forward_prompt(
    notification: SecretPromptNotification,
    operation: AuthorizationOperationId,
    prompt_ids: &mut BTreeSet<u64>,
    sink: &AppEventSink,
) -> Result<(), AppServerError> {
    match notification {
        SecretPromptNotification::Requested {
            id,
            prompt,
            error,
            query,
            operation: Some(owner),
            masked,
        } if owner == operation => {
            prompt_ids.insert(id);
            sink.emit(AppServerEvent::AuthorizationPromptRequested {
                prompt_id: id,
                prompt: error.map_or_else(|| prompt.clone(), |error| format!("{error} {prompt}")),
                reference: query.reference.as_str().to_owned(),
                kind: query.kind.as_str().to_owned(),
                masked,
            })
            .await
        }
        SecretPromptNotification::Resolved { id, answered } if prompt_ids.remove(&id) => {
            sink.emit(AppServerEvent::AuthorizationPromptResolved {
                prompt_id: id,
                answered,
            })
            .await
        }
        _ => Ok(()),
    }
}

fn map_authorization_error(error: heycode_authorization::AuthorizationError) -> AppServerError {
    match error {
        heycode_authorization::AuthorizationError::Cancelled => AppServerError::cancelled(),
        heycode_authorization::AuthorizationError::UnknownFlow { .. }
        | heycode_authorization::AuthorizationError::InvalidFlowId { .. } => {
            AppServerError::invalid()
        }
        _ => AppServerError::unavailable(),
    }
}

fn credential_status(descriptor: CredentialDescriptor) -> AppCredentialStatus {
    AppCredentialStatus {
        reference: descriptor.reference.as_str().to_owned(),
        kind: descriptor.kind.as_str().to_owned(),
        inspected: true,
        configured: Some(descriptor.configured),
        source: descriptor.source.map(|source| {
            match source {
                heycode_credentials::CredentialSource::Environment => "environment",
                heycode_credentials::CredentialSource::Keychain => "keychain",
                heycode_credentials::CredentialSource::File => "file",
                heycode_credentials::CredentialSource::Command => "command",
                heycode_credentials::CredentialSource::AmbientRuntime => "ambient_runtime",
                heycode_credentials::CredentialSource::SubscriptionRuntime => {
                    "subscription_runtime"
                }
                _ => "unknown",
            }
            .to_owned()
        }),
        provider: descriptor
            .provider
            .map(|provider| provider.as_str().to_owned()),
        writable: Some(descriptor.writable),
        validation: match descriptor.validation {
            CredentialValidation::Unknown => AppCredentialValidation::Unknown,
            CredentialValidation::Valid { checked_at_ms } => {
                AppCredentialValidation::Valid { checked_at_ms }
            }
            CredentialValidation::Invalid {
                checked_at_ms,
                reason,
            } => AppCredentialValidation::Invalid {
                checked_at_ms,
                reason,
            },
            CredentialValidation::Stale { checked_at_ms } => {
                AppCredentialValidation::Stale { checked_at_ms }
            }
            _ => AppCredentialValidation::Unknown,
        },
    }
}

fn uninspected_credential_status(
    query: &heycode_credentials::CredentialQuery,
) -> AppCredentialStatus {
    AppCredentialStatus {
        reference: query.reference.as_str().to_owned(),
        kind: query.kind.as_str().to_owned(),
        inspected: false,
        configured: None,
        source: None,
        provider: None,
        writable: None,
        validation: AppCredentialValidation::Unknown,
    }
}

fn authorization_method(method: AuthorizationMethod) -> &'static str {
    match method {
        AuthorizationMethod::ApiKey => "api_key",
        AuthorizationMethod::OAuth => "oauth",
        AuthorizationMethod::DeviceCode => "device_code",
        AuthorizationMethod::Command => "command",
        AuthorizationMethod::Ambient => "ambient",
        _ => "unknown",
    }
}

fn provider_owner(
    providers: &ProviderRegistry,
    descriptor: &heycode_authorization::AuthorizationDescriptor,
) -> Option<String> {
    let owners = providers
        .profiles()
        .into_iter()
        .filter(|profile| {
            profile.credential_reference.as_deref() == Some(descriptor.query.reference.as_str())
        })
        .map(|profile| profile.registry_name)
        .collect::<Vec<_>>();
    let [owner] = owners.as_slice() else {
        return None;
    };
    Some(owner.clone())
}

fn route(selection: RoutingSelection) -> AppRouteSelection {
    AppRouteSelection {
        runtime: selection.runtime().to_owned(),
        provider: selection.provider().to_owned(),
        model: selection.model().to_owned(),
        effort: selection.effort().map(str::to_owned),
    }
}

fn map_routing_error(error: RoutingError) -> AppServerError {
    match error {
        RoutingError::InvalidSelection(_) | RoutingError::UnknownProvider(_) => {
            AppServerError::invalid()
        }
        RoutingError::UnknownRuntime(_)
        | RoutingError::UnselectableModel { .. }
        | RoutingError::EffortUnavailable => {
            AppServerError::classified(AppServerErrorCode::Unsupported)
        }
        RoutingError::OpaqueStateResolutionRequired { .. } => {
            AppServerError::classified(AppServerErrorCode::Conflict)
        }
        RoutingError::SetupRequired => AppServerError::classified(AppServerErrorCode::Closed),
        RoutingError::Settings(_)
        | RoutingError::OpaqueStatePreparation
        | RoutingError::UnknownConnectTarget(_)
        | RoutingError::NoLogoutTarget(_)
        | RoutingError::Authorization(_)
        | RoutingError::Credential(_)
        | RoutingError::BackendControl(_)
        | RoutingError::BackendSessionRetired
        | RoutingError::RegistryUnavailable => AppServerError::unavailable(),
    }
}

fn provider_row(profile: ProviderProfile) -> AppProviderRow {
    AppProviderRow {
        id: profile.registry_name,
        display_name: profile.descriptor.display_name,
        default_model: profile.default_model,
        credential_reference: profile.credential_reference,
        protocols: profile.descriptor.protocols,
    }
}

fn runtime_row(descriptor: heycode_runtime::AgentRuntimeDescriptor) -> AppRuntimeRow {
    let kind = descriptor.kind();
    let capabilities = descriptor.capabilities();
    let configuration = descriptor.configuration_capabilities();
    AppRuntimeRow {
        id: descriptor.id().as_str().to_owned(),
        display_name: descriptor.display_name().to_owned(),
        kind: match kind {
            AgentRuntimeKind::Native => "native",
            AgentRuntimeKind::Delegated => "delegated",
        }
        .to_owned(),
        workspace: match kind {
            AgentRuntimeKind::Native => AppRuntimeWorkspace::Composed,
            AgentRuntimeKind::Delegated => AppRuntimeWorkspace::Selectable,
        },
        capabilities: AppRuntimeCapabilities {
            models: evidence(capabilities.models),
            resume: evidence(capabilities.resume),
            fork: evidence(capabilities.fork),
            steer: evidence(capabilities.steer),
            follow_up: evidence(capabilities.follow_up),
            permissions: evidence(capabilities.permissions),
            questions: evidence(capabilities.questions),
            compaction: evidence(capabilities.compaction),
        },
        configuration: AppRuntimeConfigurationCapabilities {
            system_prompt: evidence(configuration.system_prompt),
            tools: evidence(configuration.tools),
            model: evidence(configuration.model),
            reasoning_effort: evidence(configuration.reasoning_effort),
        },
    }
}

fn model_row(model: &ModelDescriptor, at_ms: u64) -> AppModelRow {
    let status = model.lifecycle.effective_status(at_ms);
    AppModelRow {
        id: model.id.clone(),
        display_name: model.display_name.clone(),
        aliases: model.aliases.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        lifecycle: match status {
            ModelLifecycleStatus::Unknown => AppModelLifecycle::Unknown,
            ModelLifecycleStatus::Stable => AppModelLifecycle::Stable,
            ModelLifecycleStatus::Preview => AppModelLifecycle::Preview,
            ModelLifecycleStatus::Deprecated => AppModelLifecycle::Deprecated,
            ModelLifecycleStatus::Retired => AppModelLifecycle::Retired,
        },
        selectable: model.lifecycle.is_selectable(at_ms),
        replacement_ids: model.lifecycle.replacement_ids.clone(),
        capabilities: AppModelCapabilities {
            tools: evidence(model.capabilities.tools),
            reasoning: evidence(model.capabilities.reasoning),
            image_input: evidence(model.capabilities.image_input),
            document_input: evidence(model.capabilities.document_input),
            structured_output: evidence(model.capabilities.structured_output),
            native_web: evidence(model.capabilities.native_web),
            native_compaction: evidence(model.capabilities.native_compaction),
            prompt_cache: evidence(model.capabilities.prompt_cache),
        },
    }
}

const fn evidence(value: CapabilitySupport) -> AppCapabilityEvidence {
    match value {
        CapabilitySupport::Supported => AppCapabilityEvidence::Supported,
        CapabilitySupport::Unsupported => AppCapabilityEvidence::Unsupported,
        CapabilitySupport::Unknown => AppCapabilityEvidence::Unknown,
    }
}

fn catalog_warning(error: &CatalogError) -> AppControlWarning {
    let (code, message) = match error {
        CatalogError::UnknownCatalog { .. } | CatalogError::NoCachedCatalog { .. } => (
            "catalog_unavailable",
            "No live model catalog is available; the provider default is shown",
        ),
        CatalogError::Cancelled { .. } => ("cancelled", "Model catalog refresh was cancelled"),
        CatalogError::Refresh { kind, .. } => match kind {
            CatalogFailureKind::Unauthorized => {
                ("unauthorized", "Model catalog authorization is required")
            }
            CatalogFailureKind::Network => ("network", "Model catalog network access failed"),
            CatalogFailureKind::Unavailable => {
                ("unavailable", "Model catalog service is unavailable")
            }
            CatalogFailureKind::InvalidResponse => {
                ("invalid_response", "Model catalog response was invalid")
            }
            CatalogFailureKind::Cancelled => ("cancelled", "Model catalog refresh was cancelled"),
        },
        CatalogError::InvalidCatalog { .. } => {
            ("invalid_catalog", "Model catalog data was invalid")
        }
        CatalogError::Persistence { .. } => {
            ("persistence", "Model catalog persistence is unavailable")
        }
        CatalogError::DuplicateCatalog { .. }
        | CatalogError::DuplicatePersistence
        | CatalogError::RegistryUnavailable => {
            ("registry", "Model catalog registry is unavailable")
        }
    };
    AppControlWarning {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn setting_row(snapshot: &SettingsSnapshot, service_writable: bool) -> AppSettingsSnapshot {
    // S15: values reach a client ONLY through the verified redacted
    // projection. Reading the raw layers here would ship exactly the material
    // the proof exists to withhold, so its absence — not `wire_exposed()`
    // alone — is what gates every value field.
    let projection = snapshot.wire_projection();
    let exposed = projection.is_some();
    AppSettingsSnapshot {
        namespace: snapshot.namespace().as_str().to_owned(),
        exposed,
        writable: exposed && service_writable,
        applies: match snapshot.applies() {
            SettingsApplies::Live => "live",
            SettingsApplies::Restart => "restart",
        }
        .to_owned(),
        revision: snapshot.revision(),
        schema: projection.map(|projection| projection.schema().clone()),
        defaults: projection.map(|projection| projection.defaults().clone()),
        base: projection.and_then(|projection| projection.base().cloned()),
        user: projection.and_then(|projection| projection.user().cloned()),
        project: projection.and_then(|projection| projection.project().cloned()),
        managed: projection.and_then(|projection| projection.managed().cloned()),
        resolved: projection.map(|projection| projection.resolved().clone()),
        managed_locks: snapshot.managed_locks().to_vec(),
        redacted_paths: projection
            .map(|projection| projection.redacted_paths().to_vec())
            .unwrap_or_default(),
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, AppServerError> {
    serde_json::from_value(params).map_err(|_| AppServerError::invalid())
}

fn to_value(value: impl Serialize) -> Result<Value, AppServerError> {
    serde_json::to_value(value).map_err(|_| AppServerError::invalid())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthorizationStartParams {
    flow_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthorizationAnswerParams {
    prompt_id: u64,
    secret: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthorizationPromptParams {
    prompt_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationLogoutParams {
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderSelectParams {
    provider: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelListParams {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    refresh: AppCatalogRefresh,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSelectParams {
    model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSelectParams {
    runtime: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSelectParams {
    cwd: std::path::PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsGetParams {
    namespace: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsReplaceParams {
    namespace: String,
    user: Value,
    expected_revision: u64,
}

fn get<T: Send + Sync + 'static>(
    context: &heycode_core::Context,
    key: heycode_core::ServiceKey,
) -> heycode_core::CoreResult<Arc<T>> {
    context.get::<T>(key).ok_or_else(|| {
        heycode_core::CoreError::other(format!("{} service type mismatch", key.as_str()))
    })
}

/// Contribute registry-backed app-server controls as one disposable plugin.
#[must_use]
pub fn app_server_controls_plugin() -> Box<dyn heycode_core::Plugin> {
    struct AppServerControlsPlugin;

    impl heycode_core::Plugin for AppServerControlsPlugin {
        fn name(&self) -> &'static str {
            "app-server-controls"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            APP_CONTROL_METHODS
                .iter()
                .map(|method| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::AppServerMethod,
                        *method,
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_APP_SERVER,
                heycode_settings::SERVICE_SETTINGS,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_authorization::SERVICE_AUTHORIZATION,
                heycode_authorization_api_key::SERVICE_SECRET_PROMPT,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_MODELS,
                heycode_mcp::SERVICE_MCP,
                heycode_routing::SERVICE_ROUTING,
                heycode_runtime::SERVICE_RUNTIMES,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let server = get::<AppServer>(context, crate::SERVICE_APP_SERVER)?;
            let settings = get::<SettingsService>(context, heycode_settings::SERVICE_SETTINGS)?;
            let credentials =
                get::<CredentialsService>(context, heycode_credentials::SERVICE_CREDENTIALS)?;
            let authorization =
                get::<AuthorizationService>(context, heycode_authorization::SERVICE_AUTHORIZATION)?;
            let prompt = get::<InteractiveSecretPrompt>(
                context,
                heycode_authorization_api_key::SERVICE_SECRET_PROMPT,
            )?;
            let providers = get::<ProviderRegistry>(context, heycode_llm::SERVICE_PROVIDERS)?;
            let models = get::<CatalogRegistry>(context, heycode_llm::SERVICE_MODELS)?;
            let mcp = get::<McpRegistry>(context, heycode_mcp::SERVICE_MCP)?;
            let routing = get::<RoutingService>(context, heycode_routing::SERVICE_ROUTING)?;
            let runtimes = get::<heycode_runtime::AgentRuntimeRegistry>(
                context,
                heycode_runtime::SERVICE_RUNTIMES,
            )?;
            let selection = routing
                .selection()
                .map_err(|_| heycode_core::CoreError::other("routing selection unavailable"))?;
            let runtime = runtimes
                .get(selection.runtime())
                .map_err(|_| heycode_core::CoreError::other("runtime registry unavailable"))?
                .ok_or_else(|| heycode_core::CoreError::other("selected runtime is missing"))?;
            server.bind_runtime(
                context,
                runtime,
                selection.runtime_model().map(str::to_owned),
                selection.runtime_effort().map(str::to_owned),
            )?;
            routing
                .register_delegated_controls(Arc::new(crate::RoutingControlHandle::new(
                    &server,
                    routing.clone(),
                )))
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let notifications = prompt.subscribe_effect(context);
            let controls = Arc::new(AppControlPlane::new(
                Arc::downgrade(&server),
                authorization,
                credentials,
                providers,
                prompt,
                notifications,
                models,
                runtimes,
                routing,
                mcp,
                settings,
                context.plugin_inventory(),
            ));
            server.register_controls(context, controls)
        }
    }

    Box::new(AppServerControlsPlugin)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use heycode_authorization::{
        AuthorizationDescriptor, AuthorizationFlow, AuthorizationFlowFailure, AuthorizationFlowId,
        AuthorizationGrant, AuthorizationRequest,
    };
    use heycode_authorization_api_key::{SecretPrompt, SecretPromptRequest};
    use heycode_credentials::{
        CredentialKind, CredentialProvider, CredentialProviderId, CredentialProviderState,
        CredentialQuery, CredentialReference, CredentialSecret, CredentialSource,
    };

    use super::*;

    #[test]
    fn runtime_and_workspace_capabilities_follow_the_installed_control_generation() {
        let base = initialize_result(false);
        assert!(!base.capabilities.runtimes);
        assert!(!base.capabilities.workspace);

        let installed = initialize_result(true);
        assert!(installed.capabilities.runtimes);
        assert!(installed.capabilities.workspace);
    }

    #[test]
    fn runtime_rows_preserve_kind_workspace_and_exact_capability_evidence() {
        let descriptor = heycode_runtime::AgentRuntimeDescriptor::new(
            "delegated-test",
            "Delegated test",
            AgentRuntimeKind::Delegated,
            heycode_runtime::RuntimeCapabilities {
                models: CapabilitySupport::Supported,
                resume: CapabilitySupport::Unsupported,
                fork: CapabilitySupport::Unknown,
                steer: CapabilitySupport::Supported,
                follow_up: CapabilitySupport::Unsupported,
                permissions: CapabilitySupport::Unknown,
                questions: CapabilitySupport::Supported,
                compaction: CapabilitySupport::Unsupported,
            },
        )
        .unwrap();
        let row = runtime_row(descriptor);
        assert_eq!(row.kind, "delegated");
        assert_eq!(row.workspace, AppRuntimeWorkspace::Selectable);
        assert_eq!(row.capabilities.models, AppCapabilityEvidence::Supported);
        assert_eq!(row.capabilities.resume, AppCapabilityEvidence::Unsupported);
        assert_eq!(row.capabilities.fork, AppCapabilityEvidence::Unknown);
        assert_eq!(row.capabilities.permissions, AppCapabilityEvidence::Unknown);
    }

    /// S15 regression: `setting_row` must render the verified redacted
    /// projection, never the raw layers. Built as a unit test because a
    /// namespace holding real credential material is exactly what the composed
    /// product world does not contain — so only a purpose-built namespace can
    /// tell the two code paths apart.
    #[test]
    fn a_setting_row_renders_the_redacted_projection_not_the_raw_layers() {
        use heycode_settings::{
            SettingsDefinition, SettingsFieldPath, SettingsNamespace, SettingsSchema,
            SettingsService,
        };

        const CANARY: &str = "sk-ant-private-settings-canary";
        let context = heycode_core::Context::new();
        let service = SettingsService::new(heycode_settings::SettingsDocuments::new());
        let schema = SettingsSchema::new(
            serde_json::json!({"type": "object"}),
            serde_json::json!({"api_key": CANARY, "endpoint": "https://example.test"}),
            |_| Ok(()),
        )
        .unwrap()
        .with_wire_exposure()
        .with_secret_path(SettingsFieldPath::new("api_key").unwrap());
        let snapshot = service
            .register(
                &context,
                SettingsDefinition::new(SettingsNamespace::new("canary").unwrap(), schema),
            )
            .unwrap();

        // The raw layer still holds the material — this is what a regression
        // would ship.
        assert!(
            snapshot.defaults().to_string().contains(CANARY),
            "the raw layer must still hold the value, or this test proves nothing"
        );

        let row = setting_row(&snapshot, true);
        assert!(row.exposed, "a proved namespace stays exposed");
        for layer in [
            row.schema.as_ref(),
            row.defaults.as_ref(),
            row.base.as_ref(),
            row.user.as_ref(),
            row.project.as_ref(),
            row.managed.as_ref(),
            row.resolved.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                !layer.to_string().contains(CANARY),
                "a rendered layer leaked declared secret material"
            );
        }
        assert!(
            row.redacted_paths.iter().any(|path| path == "api_key"),
            "the client must be told which paths were withheld"
        );
        // A non-secret sibling still renders, so redaction is targeted rather
        // than blanking the namespace.
        assert!(
            row.resolved
                .as_ref()
                .map(std::string::ToString::to_string)
                .is_some_and(|rendered| rendered.contains("example.test"))
        );
    }

    struct MemoryCredentialProvider {
        id: CredentialProviderId,
        value: StdMutex<Option<String>>,
    }

    impl CredentialProvider for MemoryCredentialProvider {
        fn id(&self) -> &CredentialProviderId {
            &self.id
        }

        fn precedence(&self) -> u16 {
            10
        }

        fn inspect(&self, _query: &CredentialQuery) -> Result<CredentialProviderState, String> {
            Ok(if self.value.lock().unwrap().is_some() {
                CredentialProviderState::configured(CredentialSource::Keychain, true)
            } else {
                CredentialProviderState::unconfigured(true)
            })
        }

        fn resolve(&self, _query: &CredentialQuery) -> Result<Option<CredentialSecret>, String> {
            Ok(self
                .value
                .lock()
                .unwrap()
                .clone()
                .map(CredentialSecret::new))
        }

        fn write(&self, _query: &CredentialQuery, secret: &CredentialSecret) -> Result<(), String> {
            *self.value.lock().unwrap() = Some(secret.expose().to_owned());
            Ok(())
        }

        fn delete(&self, _query: &CredentialQuery) -> Result<(), String> {
            self.value.lock().unwrap().take();
            Ok(())
        }
    }

    struct PromptFlow {
        prompt: Arc<InteractiveSecretPrompt>,
        descriptor: AuthorizationDescriptor,
    }

    #[async_trait]
    impl AuthorizationFlow for PromptFlow {
        fn descriptor(&self) -> AuthorizationDescriptor {
            self.descriptor.clone()
        }

        async fn authorize(
            &self,
            request: AuthorizationRequest,
        ) -> Result<AuthorizationGrant, AuthorizationFlowFailure> {
            let secret = self
                .prompt
                .prompt(
                    SecretPromptRequest {
                        error: None,
                        prompt: "Paste test credential".to_owned(),
                        query: request.query,
                        operation: request.operation,
                        masked: true,
                    },
                    request.cancellation,
                )
                .await
                .map_err(|message| AuthorizationFlowFailure::new("input", message))?;
            Ok(AuthorizationGrant::new(secret))
        }
    }

    fn query() -> CredentialQuery {
        CredentialQuery::new(
            CredentialReference::new("TEST_API_KEY").unwrap(),
            CredentialKind::new("api-key").unwrap(),
        )
    }

    #[tokio::test]
    async fn correlated_masked_prompt_commits_before_secret_free_receipt() {
        let mut context = heycode_core::Context::new();
        let credentials = Arc::new(CredentialsService::new());
        credentials
            .register(
                &context,
                Arc::new(MemoryCredentialProvider {
                    id: CredentialProviderId::new("memory-writer").unwrap(),
                    value: StdMutex::new(None),
                }),
            )
            .unwrap();
        let authorization = Arc::new(AuthorizationService::new(credentials.clone()));
        let prompt = Arc::new(InteractiveSecretPrompt::new(context.events.clone()));
        let descriptor = AuthorizationDescriptor {
            id: AuthorizationFlowId::new("test-api-key").unwrap(),
            label: "Test API key".to_owned(),
            method: AuthorizationMethod::ApiKey,
            interactive: true,
            query: query(),
        };
        authorization
            .register(
                &context,
                Arc::new(PromptFlow {
                    prompt: prompt.clone(),
                    descriptor,
                }),
            )
            .unwrap();
        let notifications = prompt.subscribe_effect(&context);
        let controls = Arc::new(AuthorizationControls {
            authorization,
            credentials,
            providers: Arc::new(ProviderRegistry::new()),
            prompt,
            notifications: Mutex::new(notifications),
            operation_gate: Mutex::new(()),
            next_operation: AtomicU64::new(1),
        });
        let (events_tx, mut events_rx) = mpsc::channel(8);
        let sink = AppEventSink::control(Arc::new(AtomicU64::new(0)), events_tx);
        let task = {
            let controls = controls.clone();
            tokio::spawn(async move {
                controls
                    .start(
                        "test-api-key",
                        "test-provider",
                        "TEST_API_KEY",
                        sink,
                        CancellationToken::new(),
                    )
                    .await
            })
        };

        let requested = events_rx.recv().await.unwrap();
        assert_eq!(requested.method, "control/event");
        assert!(requested.params.session_id.is_none());
        let prompt_id = match requested.params.event {
            AppServerEvent::AuthorizationPromptRequested {
                prompt_id,
                masked: true,
                ..
            } => prompt_id,
            event => panic!("unexpected event: {event:?}"),
        };
        controls
            .answer(AuthorizationAnswerParams {
                prompt_id,
                secret: "transient-test-value".to_owned(),
            })
            .unwrap();
        let receipt = task.await.unwrap().unwrap();
        assert_eq!(receipt.credential.configured, Some(true));
        assert_eq!(receipt.provider, "test-provider");
        assert_eq!(receipt.committed_by, "memory-writer");
        let wire = serde_json::to_string(&receipt).unwrap();
        assert!(!wire.contains("transient-test-value"));
        assert!(matches!(
            events_rx.recv().await.unwrap().params.event,
            AppServerEvent::AuthorizationPromptResolved {
                prompt_id: resolved,
                answered: true,
            } if resolved == prompt_id
        ));
        context.shutdown();
    }
}
