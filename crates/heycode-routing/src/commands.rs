//! Plugin-owned connect/logout/provider/model/effort commands.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandSource, CommandTiming,
    UiEvent,
};
use heycode_authorization::{AuthorizationDescriptor, AuthorizationService};
use heycode_credentials::CredentialsService;
use heycode_onboarding::OnboardingService;
use tokio_util::sync::CancellationToken;

use crate::{ProviderSwitchOutcome, RoutingError, RoutingService};

pub(crate) fn routing_commands(
    service: RoutingService,
) -> Result<Vec<Arc<dyn Command>>, anyhow::Error> {
    let service = Arc::new(service);
    let source = CommandSource::from_plugin("routing")?;
    let effort_unavailable = CommandAvailability::unavailable(
        "The active inference adapter does not expose reasoning effort levels",
    )?;
    Ok(vec![
        crate::fallback::command(service.clone())?,
        Arc::new(ProviderCommand {
            descriptor: CommandDescriptor::new(
                "provider",
                "Show or persist the active inference provider",
                vec![
                    CommandArgument::optional("id", "Registered provider id")?,
                    CommandArgument::optional(
                        "opaque",
                        "Opaque-state choice: portable, fork, or cancel",
                    )?,
                ],
                CommandTiming::Queued,
                source.clone(),
            )?,
            service: service.clone(),
        }),
        Arc::new(ModelCommand {
            descriptor: CommandDescriptor::new(
                "model",
                "Show or persist a catalog-proven model",
                vec![
                    CommandArgument::optional("id", "Provider-native model id")?,
                    CommandArgument::optional(
                        "opaque",
                        "Opaque-state choice: portable, fork, or cancel",
                    )?,
                ],
                CommandTiming::Queued,
                source.clone(),
            )?,
            service: service.clone(),
        }),
        Arc::new(EffortCommand {
            descriptor: CommandDescriptor::new(
                "effort",
                "Show or persist reasoning effort when the adapter exposes levels",
                vec![CommandArgument::optional(
                    "level",
                    "Adapter-owned effort id",
                )?],
                CommandTiming::Queued,
                source,
            )?,
            service,
            unavailable: effort_unavailable,
        }),
    ])
}

pub(crate) fn auth_commands(
    routing: RoutingService,
    onboarding: Arc<OnboardingService>,
    authorization: Arc<AuthorizationService>,
    credentials: Arc<CredentialsService>,
) -> Result<Vec<Arc<dyn Command>>, anyhow::Error> {
    let source = CommandSource::from_plugin("routing-auth")?;
    let auth = Arc::new(RoutingAuth {
        routing,
        onboarding,
        authorization,
        credentials,
    });
    Ok(vec![
        Arc::new(ConnectCommand {
            descriptor: CommandDescriptor::new(
                "connect",
                "Open connection setup or run one authorization flow",
                vec![CommandArgument::optional(
                    "target",
                    "Provider id or authorization flow id",
                )?],
                CommandTiming::Queued,
                source.clone(),
            )?,
            auth: auth.clone(),
        }),
        Arc::new(LogoutCommand {
            descriptor: CommandDescriptor::new(
                "logout",
                "Disconnect the active route or remove one provider credential",
                vec![CommandArgument::optional(
                    "provider",
                    "Provider id; defaults to current",
                )?],
                CommandTiming::Immediate,
                source,
            )?,
            auth,
        }),
    ])
}

struct RoutingAuth {
    routing: RoutingService,
    onboarding: Arc<OnboardingService>,
    authorization: Arc<AuthorizationService>,
    credentials: Arc<CredentialsService>,
}

struct LogoutOutcome {
    target: String,
    reset: bool,
    deleted: Vec<heycode_credentials::CredentialProviderId>,
    cleanup_error: Option<String>,
}

impl RoutingAuth {
    fn begin_connect(&self) -> Result<(), RoutingError> {
        self.onboarding
            .begin_connect()
            .map_err(|_| RoutingError::RegistryUnavailable)
    }

    async fn connect(
        &self,
        target: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_authorization::AuthorizationReceipt, RoutingError> {
        let matches = self
            .authorization
            .descriptors()
            .map_err(|error| RoutingError::Authorization(error.to_string()))?
            .into_iter()
            .filter(|descriptor| connect_matches(descriptor, target))
            .collect::<Vec<_>>();
        let [descriptor] = matches.as_slice() else {
            return Err(RoutingError::UnknownConnectTarget(target.to_owned()));
        };
        self.authorization
            .authorize(&descriptor.id, descriptor.query.clone(), cancellation)
            .await
            .map_err(|error| RoutingError::Authorization(error.to_string()))
    }

    async fn logout(&self, target: Option<&str>) -> Result<LogoutOutcome, RoutingError> {
        let active = self.routing.selection()?;
        let (target, reset, credential_provider, credential_reference) = match target {
            None if active.runtime() == "native" => (
                active.provider().to_owned(),
                true,
                Some(active.provider().to_owned()),
                active
                    .credential_reference()
                    .map(|reference| reference.as_str().to_owned()),
            ),
            None => (active.runtime().to_owned(), true, None, None),
            Some(target) if target == active.runtime() => (
                target.to_owned(),
                true,
                (active.runtime() == "native").then(|| active.provider().to_owned()),
                (active.runtime() == "native")
                    .then(|| {
                        active
                            .credential_reference()
                            .map(|reference| reference.as_str().to_owned())
                    })
                    .flatten(),
            ),
            Some(target) if active.runtime() == "native" && target == active.provider() => (
                target.to_owned(),
                true,
                Some(target.to_owned()),
                active
                    .credential_reference()
                    .map(|reference| reference.as_str().to_owned()),
            ),
            Some(target)
                if self
                    .routing
                    .connection_profiles()
                    .iter()
                    .any(|profile| profile.registry_name == target) =>
            {
                (target.to_owned(), false, Some(target.to_owned()), None)
            }
            Some(target) if self.routing.has_runtime(target) => {
                (target.to_owned(), false, None, None)
            }
            Some(target) => return Err(RoutingError::UnknownProvider(target.to_owned())),
        };

        let exact_active_reference =
            reset && active.runtime() == "native" && credential_reference.is_some();
        let reference = credential_reference.or_else(|| {
            credential_provider.as_deref().and_then(|provider| {
                self.routing
                    .connection_profiles()
                    .iter()
                    .find(|profile| profile.registry_name == provider)
                    .and_then(|profile| profile.credential_reference.clone())
            })
        });
        let mut queries = reference
            .as_deref()
            .map(|reference| {
                self.authorization
                    .descriptors()
                    .map_err(|error| RoutingError::Credential(error.to_string()))
                    .map(|descriptors| {
                        descriptors
                            .into_iter()
                            .filter(|descriptor| descriptor.query.reference.as_str() == reference)
                            .map(|descriptor| descriptor.query)
                            .collect::<Vec<_>>()
                    })
            })
            .transpose()?
            .unwrap_or_default();
        if queries.is_empty()
            && exact_active_reference
            && let Some(reference) = reference
        {
            queries.push(heycode_credentials::CredentialQuery::new(
                heycode_credentials::CredentialReference::new(reference)
                    .map_err(|error| RoutingError::Credential(error.to_string()))?,
                heycode_credentials::CredentialKind::new("api-key")
                    .map_err(|error| RoutingError::Credential(error.to_string()))?,
            ));
        }
        if reset {
            self.routing.require_setup(&active).await?;
        }
        let mut deleted = Vec::new();
        let mut cleanup_error = None;
        for query in queries {
            match self.credentials.delete_writable(&query) {
                Ok(providers) => deleted.extend(providers),
                Err(error) if reset => {
                    cleanup_error = Some(error.to_string());
                    break;
                }
                Err(error) => return Err(RoutingError::Credential(error.to_string())),
            }
        }
        Ok(LogoutOutcome {
            target,
            reset,
            deleted,
            cleanup_error,
        })
    }
}

fn connect_matches(descriptor: &AuthorizationDescriptor, target: &str) -> bool {
    descriptor.id.as_str() == target
        || descriptor
            .id
            .as_str()
            .strip_suffix("-api-key")
            .is_some_and(|provider| provider == target)
}

struct ConnectCommand {
    descriptor: CommandDescriptor,
    auth: Arc<RoutingAuth>,
}

#[async_trait]
impl Command for ConnectCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let target = args.trim();
        if target.is_empty() {
            self.auth.begin_connect()?;
            agent.ui().emit(UiEvent::ConnectRequested);
            return Ok(());
        }
        let receipt = self
            .auth
            .connect(target, tokio_util::sync::CancellationToken::new())
            .await?;
        agent.ui().emit(UiEvent::Info {
            text: format!(
                "connected {} via {}",
                receipt.credential.reference.as_str(),
                receipt.committed_by.as_str()
            ),
        });
        Ok(())
    }
}

struct LogoutCommand {
    descriptor: CommandDescriptor,
    auth: Arc<RoutingAuth>,
}

#[async_trait]
impl Command for LogoutCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let target = (!args.trim().is_empty()).then(|| args.trim());
        let outcome = self.auth.logout(target).await?;
        if outcome.reset {
            agent.ui().emit(UiEvent::LoggedOut {
                target: outcome.target,
                cleanup_warning: outcome.cleanup_error.map(|error| {
                    format!("route disconnected, but heycode credential cleanup failed: {error}")
                }),
            });
        } else {
            agent.ui().emit(UiEvent::Info {
                text: if outcome.deleted.is_empty() {
                    format!(
                        "No connected account for {}: no heycode-owned credential was stored",
                        outcome.target
                    )
                } else {
                    format!(
                        "Disconnected {}: heycode-owned credential deleted",
                        outcome.target
                    )
                },
            });
        }
        Ok(())
    }
}

struct ProviderCommand {
    descriptor: CommandDescriptor,
    service: Arc<RoutingService>,
}

#[async_trait]
impl Command for ProviderCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let mut parts = args.split_whitespace();
        let Some(id) = parts.next() else {
            let selection = self.service.selection()?;
            agent.ui().emit(UiEvent::RoutePickerRequested {
                current_provider: selection.provider().to_owned(),
                current_runtime: selection.runtime().to_owned(),
            });
            return Ok(());
        };
        let resolution = parts.next();
        if parts.next().is_some() {
            anyhow::bail!("usage: /provider [id] [portable|fork|cancel]");
        }
        let outcome = match resolution {
            None => ProviderSwitchOutcome::Applied(self.service.select_provider(id)?),
            Some(choice) => {
                let resolution = opaque_resolution(choice).ok_or_else(|| {
                    anyhow::anyhow!("usage: /provider [id] [portable|fork|cancel]")
                })?;
                self.service
                    .select_provider_resolving(id, resolution, CancellationToken::new())
                    .await?
            }
        };
        let text = match outcome {
            ProviderSwitchOutcome::Applied(selection) => format!(
                "provider persisted as {} with model {}",
                selection.provider(),
                selection.model()
            ),
            ProviderSwitchOutcome::Cancelled => {
                "provider switch cancelled; route unchanged".to_owned()
            }
            ProviderSwitchOutcome::Forked {
                session_id,
                fork_event_count,
            } => format!(
                "created pre-checkpoint fork {session_id} from {fork_event_count} events; current route unchanged"
            ),
        };
        agent.ui().emit(UiEvent::Info { text });
        Ok(())
    }
}

struct ModelCommand {
    descriptor: CommandDescriptor,
    service: Arc<RoutingService>,
}

#[async_trait]
impl Command for ModelCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let mut parts = args.split_whitespace();
        let Some(id) = parts.next() else {
            let active = self.service.active_configuration()?;
            agent.ui().emit(UiEvent::ModelPickerRequested {
                owner: active.owner().clone(),
                routing_revision: active.revision(),
                current_model: active.model().unwrap_or_default().to_owned(),
            });
            return Ok(());
        };
        let resolution = parts.next();
        if parts.next().is_some() {
            anyhow::bail!("usage: /model [id] [portable|fork|cancel]");
        }
        let active = self.service.active_configuration()?;
        let outcome = match resolution {
            None => {
                let catalog = match active.owner() {
                    heycode_agent::BackendControlOwner::NativeInference { .. } => None,
                    heycode_agent::BackendControlOwner::DelegatedRuntime { .. } => Some(
                        self.service
                            .models_owned(
                                active.owner(),
                                heycode_llm::CatalogRefreshMode::PreferCache,
                                CancellationToken::new(),
                            )
                            .await?,
                    ),
                };
                ProviderSwitchOutcome::Applied(
                    self.service
                        .select_model_owned(
                            active.owner(),
                            active.revision(),
                            catalog.as_ref().map(|view| view.snapshot.as_ref()),
                            id,
                            CancellationToken::new(),
                        )
                        .await?,
                )
            }
            Some(choice) => {
                if matches!(
                    active.owner(),
                    heycode_agent::BackendControlOwner::DelegatedRuntime { .. }
                ) {
                    anyhow::bail!("usage: /model [id]");
                }
                let resolution = opaque_resolution(choice)
                    .ok_or_else(|| anyhow::anyhow!("usage: /model [id] [portable|fork|cancel]"))?;
                self.service
                    .select_model_resolving(id, resolution, CancellationToken::new())
                    .await?
            }
        };
        let text = match outcome {
            ProviderSwitchOutcome::Applied(selection) => {
                let model = if selection.runtime() == agent.runtime_id() {
                    selection.model()
                } else {
                    selection.runtime_model().unwrap_or(id)
                };
                format!("model persisted as {model}")
            }
            ProviderSwitchOutcome::Cancelled => {
                "model switch cancelled; route unchanged".to_owned()
            }
            ProviderSwitchOutcome::Forked {
                session_id,
                fork_event_count,
            } => format!(
                "created pre-checkpoint fork {session_id} from {fork_event_count} events; current route unchanged"
            ),
        };
        agent.ui().emit(UiEvent::Info { text });
        Ok(())
    }
}

fn opaque_resolution(choice: &str) -> Option<heycode_agent::OpaqueStateResolution> {
    match choice {
        "portable" => Some(heycode_agent::OpaqueStateResolution::PortableRecompact),
        "fork" => Some(heycode_agent::OpaqueStateResolution::ForkBeforeCheckpoint),
        "cancel" => Some(heycode_agent::OpaqueStateResolution::Cancel),
        _ => None,
    }
}

struct EffortCommand {
    descriptor: CommandDescriptor,
    service: Arc<RoutingService>,
    unavailable: CommandAvailability,
}

#[async_trait]
impl Command for EffortCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.service.effort_command_available() {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let mut parts = args.split_whitespace();
        let Some(effort) = parts.next() else {
            let active = self.service.active_configuration()?;
            let options = self
                .service
                .effort_catalog_owned(active.owner(), active.revision(), CancellationToken::new())
                .await?;
            agent.ui().emit(UiEvent::EffortPickerRequested {
                owner: active.owner().clone(),
                routing_revision: active.revision(),
                current_effort: options.current().map(str::to_owned),
                choices: options.choices().to_vec(),
                default_effort: options.default().map(str::to_owned),
            });
            return Ok(());
        };
        if parts.next().is_some() {
            anyhow::bail!("usage: /effort [level]");
        }
        let active = self.service.active_configuration()?;
        let selection = self
            .service
            .select_effort_owned(
                active.owner(),
                active.revision(),
                effort,
                CancellationToken::new(),
            )
            .await?;
        agent.ui().emit(UiEvent::Info {
            text: format!(
                "reasoning effort persisted as {}",
                if selection.runtime() == agent.runtime_id() {
                    selection.effort().unwrap_or(effort)
                } else {
                    selection.runtime_effort().unwrap_or(effort)
                }
            ),
        });
        Ok(())
    }
}
