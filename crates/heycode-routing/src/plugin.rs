//! Routing settings/service/command composition.

use std::collections::BTreeSet;
use std::sync::Arc;

use heycode_core::{Context, CoreError, CoreResult, Plugin};

use crate::{
    RoutingError, RoutingOverrides, RoutingSelection, RoutingService,
    routing_definition_with_overrides, settings_namespace,
};

/// Mount persisted routing state and provider/model/effort commands.
#[must_use]
pub fn routing_plugin() -> Box<dyn Plugin> {
    routing_plugin_with_overrides(RoutingOverrides::default())
}

/// [`routing_plugin`] with this process's command-line pins (`--model`,
/// `--provider`, `--set llm.*`) layered above the persisted route.
#[must_use]
pub fn routing_plugin_with_overrides(overrides: RoutingOverrides) -> Box<dyn Plugin> {
    routing_plugin_with_connections(overrides, Vec::new())
}

/// Mount routing with provider-owned connection profiles for restart activation.
#[must_use]
pub fn routing_plugin_with_connections(
    overrides: RoutingOverrides,
    connections: Vec<heycode_llm::ConnectionProfile>,
) -> Box<dyn Plugin> {
    struct RoutingPlugin {
        overrides: RoutingOverrides,
        connections: Vec<heycode_llm::ConnectionProfile>,
    }

    impl Plugin for RoutingPlugin {
        fn name(&self) -> &'static str {
            "routing"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[
                    heycode_core::PluginContributionKind::Service,
                    heycode_core::PluginContributionKind::Command,
                ],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            std::iter::once(heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "routing",
            ))
            .chain(
                ["fallback", "provider", "model", "effort"]
                    .into_iter()
                    .map(|id| {
                        heycode_core::PluginContributionSpec::new(
                            heycode_core::ContributionKind::Command,
                            id,
                        )
                    }),
            )
            .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_MODELS,
                heycode_runtime::SERVICE_RUNTIMES,
                heycode_agent::SERVICE_COMMANDS,
                heycode_agent::SERVICE_AGENT,
            ]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_ROUTING]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = get::<heycode_settings::SettingsService>(
                context,
                heycode_settings::SERVICE_SETTINGS,
            )?;
            let providers =
                get::<heycode_llm::ProviderRegistry>(context, heycode_llm::SERVICE_PROVIDERS)?;
            let models = get::<heycode_llm::CatalogRegistry>(context, heycode_llm::SERVICE_MODELS)?;
            let runtimes = get::<heycode_runtime::AgentRuntimeRegistry>(
                context,
                heycode_runtime::SERVICE_RUNTIMES,
            )?;
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let agent = get::<heycode_agent::Agent>(context, heycode_agent::SERVICE_AGENT)?;
            let native_runtime = runtimes
                .get(agent.runtime_id())
                .map_err(|error| core_error(error.to_string()))?
                .ok_or_else(|| core_error("native runtime is not registered"))?;
            if native_runtime.descriptor().kind() != heycode_runtime::AgentRuntimeKind::Native {
                return Err(core_error("active runtime is not native"));
            }

            let route = agent.selection();
            let base =
                RoutingSelection::new(agent.runtime_id(), route.provider_name, route.model, None)
                    .map_err(core_error)?;
            let provider_ids = providers
                .names()
                .into_iter()
                .chain(
                    self.connections
                        .iter()
                        .map(|profile| profile.registry_name.clone()),
                )
                .collect::<BTreeSet<_>>();
            let runtime_ids = runtimes
                .descriptors()
                .map_err(|error| core_error(error.to_string()))?
                .into_iter()
                .filter(|descriptor| {
                    descriptor.id().as_str() == agent.runtime_id()
                        || (descriptor.kind() == heycode_runtime::AgentRuntimeKind::Delegated
                            && descriptor.capabilities().supports_primary_sessions())
                })
                .map(|descriptor| descriptor.id().as_str().to_owned())
                .collect::<BTreeSet<_>>();
            let namespace = settings_namespace().map_err(|error| core_error(error.to_string()))?;
            let snapshot = settings
                .register(
                    context,
                    routing_definition_with_overrides(
                        &base,
                        &self.overrides,
                        provider_ids,
                        runtime_ids.clone(),
                    )
                    .map_err(|error| core_error(error.to_string()))?,
                )
                .map_err(|error| core_error(error.to_string()))?;
            let service = RoutingService::new(
                settings.clone(),
                namespace.clone(),
                agent.clone(),
                providers,
                models,
                runtime_ids,
                self.overrides.notice(&snapshot),
            )
            .with_connection_profiles(self.connections.clone());
            let initial = RoutingSelection::from_value(snapshot.resolved()).map_err(core_error)?;
            if crate::model::snapshot_requires_setup(&snapshot).map_err(core_error)? {
                agent.disconnect_inference();
            } else if !self.overrides.is_empty()
                || !service.activate_pending(&snapshot).map_err(core_error)?
            {
                service
                    .apply_persisted_selection(&initial)
                    .map_err(core_error)?;
            }

            let watcher = service.clone();
            settings
                .watch(context, &namespace, move |change| {
                    match crate::model::snapshot_requires_setup(change.next()).and_then(
                        |required| {
                            if required {
                                watcher.disconnect_inference();
                                Ok(())
                            } else if crate::model::pending_connection(
                                change.next().resolved().get("pending_connection"),
                            )?
                            .is_some()
                            {
                                // A validated setup choice belongs to the next
                                // composition. Keep the retired world blocked
                                // until that composition activates it.
                                watcher.disconnect_inference();
                                Ok(())
                            } else {
                                let selection =
                                    RoutingSelection::from_value(change.next().resolved())?;
                                watcher.apply_persisted_selection(&selection)
                            }
                        },
                    ) {
                        Ok(_) => {}
                        Err(error) => watcher.agent_event(error),
                    }
                })
                .map_err(|error| core_error(error.to_string()))?;

            for command in crate::commands::routing_commands(service.clone()).map_err(core_error)? {
                commands
                    .register_effect(context, command)
                    .map_err(|error| core_error(error.to_string()))?;
            }
            context.provide(crate::SERVICE_ROUTING, self.name(), service)?;
            let published = get::<RoutingService>(context, crate::SERVICE_ROUTING)?;
            crate::fallback::install(context, &agent, &published).map_err(core_error)?;
            Ok(())
        }
    }

    Box::new(RoutingPlugin {
        overrides,
        connections,
    })
}

/// Mount connect/logout only when the authorization/credential stack exists.
#[must_use]
pub fn routing_auth_plugin() -> Box<dyn Plugin> {
    struct RoutingAuthPlugin;

    impl Plugin for RoutingAuthPlugin {
        fn name(&self) -> &'static str {
            "routing-auth"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            ["connect", "logout"]
                .into_iter()
                .map(|id| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::Command,
                        id,
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                crate::SERVICE_ROUTING,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_authorization::SERVICE_AUTHORIZATION,
                heycode_onboarding::SERVICE_ONBOARDING,
                heycode_agent::SERVICE_COMMANDS,
                heycode_agent::SERVICE_AGENT,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let routing = get::<RoutingService>(context, crate::SERVICE_ROUTING)?;
            let onboarding = get::<heycode_onboarding::OnboardingService>(
                context,
                heycode_onboarding::SERVICE_ONBOARDING,
            )?;
            let authorization = get::<heycode_authorization::AuthorizationService>(
                context,
                heycode_authorization::SERVICE_AUTHORIZATION,
            )?;
            let credentials = get::<heycode_credentials::CredentialsService>(
                context,
                heycode_credentials::SERVICE_CREDENTIALS,
            )?;
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            for command in crate::commands::auth_commands(
                (*routing).clone(),
                onboarding,
                authorization,
                credentials,
            )
            .map_err(core_error)?
            {
                commands
                    .register_effect(context, command)
                    .map_err(|error| core_error(error.to_string()))?;
            }
            Ok(())
        }
    }

    Box::new(RoutingAuthPlugin)
}

fn get<T: Send + Sync + 'static>(
    context: &Context,
    key: heycode_core::ServiceKey,
) -> CoreResult<Arc<T>> {
    context
        .get::<T>(key)
        .ok_or_else(|| CoreError::MissingService(key.to_string()))
}

fn core_error(error: impl ToString) -> CoreError {
    CoreError::other(error.to_string())
}

impl RoutingService {
    fn agent_event(&self, error: RoutingError) {
        self.emit_error(error.to_string());
    }
}
