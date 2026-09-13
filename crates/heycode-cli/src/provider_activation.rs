//! Explicit target-provider construction, shared with ordinary composition.
use super::*;

pub(crate) struct CredentialRoute {
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) reference: String,
    pub(crate) reference_explicit: bool,
    pub(crate) protocol: LlmProtocolCfg,
    pub(crate) base_url: Option<String>,
}

pub(crate) fn build_credential_provider(
    route: &CredentialRoute,
    http: &Arc<HttpService>,
    credentials: &Arc<CredentialsService>,
    settings: Option<&heycode_settings::SettingsService>,
) -> Result<Arc<dyn Provider>, heycode_core::CoreError> {
    let query = provider_credential_query(&route.provider_name, &route.reference)
        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
    let credential = RouteCredential::registry(credentials.as_ref().clone(), query);
    Ok(match route.provider_name.as_str() {
                    "minimax" | "minimax-token-plan" | "zai" if route.base_url.is_some() => {
                        return Err(heycode_core::CoreError::other("MiniMax and Z.ai direct inference use their documented product endpoints; use custom-openai for a gateway"));
                    }
                    "minimax" => Arc::new(heycode_provider_minimax::MiniMaxInference::new(
                        http.as_ref().clone(), credentials.as_ref().clone(),
                        heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::PayAsYouGo>::international(), route.model.clone(),
                    ).map_err(|error| heycode_core::CoreError::other(error.to_string()))?),
                    "minimax-token-plan" => Arc::new(heycode_provider_minimax::MiniMaxInference::new(
                        http.as_ref().clone(), credentials.as_ref().clone(),
                        heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::TokenPlan>::international(), route.model.clone(),
                    ).map_err(|error| heycode_core::CoreError::other(error.to_string()))?),
                    "zai" => Arc::new(heycode_provider_zai::ZaiInference::<heycode_provider_zai::General>::with_credential(
                        http.as_ref().clone(), credential.clone(), Some(route.model.clone()),
                    ).map_err(|error| heycode_core::CoreError::other(error.to_string()))?),
                    "lmstudio" => {
                        let mut config = heycode_provider_lmstudio::LmStudioConfig::local();
                        if let Some(endpoint) = route.base_url.as_ref() {
                            config = config.with_endpoint(
                                heycode_provider_lmstudio::LmStudioEndpoint::new(endpoint).map_err(
                                    |error| heycode_core::CoreError::other(error.to_string()),
                                )?,
                            );
                        }
                        if route.reference_explicit {
                            config = config.with_bearer_token(
                                provider_credential_query("lmstudio", &route.reference).map_err(
                                    |error| heycode_core::CoreError::other(error.to_string()),
                                )?,
                            );
                        }
                        Arc::new(
                            heycode_provider_lmstudio::LmStudioInference::new(
                                http.as_ref().clone(),
                                Some(credentials.clone()),
                                config,
                                route.model.clone(),
                            )
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        )
                    }
                    "anthropic" => {
                        let settings = settings.ok_or_else(|| heycode_core::CoreError::other("Anthropic settings missing"))?;
                        let policies = AnthropicSettingsPolicies::resolve(settings)
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let server_tools =
                            AnthropicConfiguredServerToolPolicy::resolve(settings, &route.model)
                                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let provider = match route.base_url.as_deref() {
                            Some(base_url) => {
                                heycode_provider_anthropic::AnthropicProvider::with_base_url_and_credential(
                                    http.as_ref().clone(),
                                    base_url,
                                    credential.clone(),
                                    Some(route.model.clone()),
                                )
                            }
                            None => heycode_provider_anthropic::AnthropicProvider::with_credential(
                                http.as_ref().clone(),
                                credential.clone(),
                                Some(route.model.clone()),
                            ),
                        }
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let provider = server_tools
                            .configure(provider)
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        Arc::new(
                            policies
                                .apply_provider(provider)
                                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        )
                    }
                    "deepseek" => match route.protocol {
                        LlmProtocolCfg::Auto | LlmProtocolCfg::OpenAiChat => Arc::new(
                            match route.base_url.as_deref() {
                                Some(base_url) => {
                                    heycode_llm::DeepSeekProvider::from_credential_with_transport_at(
                                        credential.clone(),
                                        Some(route.model.clone()),
                                        http.as_ref().clone(),
                                        base_url,
                                    )
                                }
                                None => heycode_llm::DeepSeekProvider::from_credential_with_transport(
                                    credential.clone(),
                                    Some(route.model.clone()),
                                    http.as_ref().clone(),
                                ),
                            }
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        ),
                        LlmProtocolCfg::AnthropicMessages if route.base_url.is_some() => {
                            return Err(heycode_core::CoreError::other(
                                "llm.base_url is not supported with llm.protocol = \"anthropic_messages\" for DeepSeek; use the default protocol",
                            ));
                        }
                        LlmProtocolCfg::AnthropicMessages => Arc::new(
                            heycode_provider_deepseek::DeepSeekAnthropicAdapter::with_credential(
                                heycode_provider_deepseek::DeepSeekAnthropicProfile::api_key(),
                                credential.clone(),
                                http.as_ref().clone(),
                            )
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        ),
                        LlmProtocolCfg::OpenAiResponses => {
                            return Err(heycode_core::CoreError::other(
                                "DeepSeek does not expose the OpenAI Responses protocol",
                            ));
                        }
                    },
                    "openrouter" => {
                        let options = vec![
                            openrouter_transform_option()
                                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        ];
                        Arc::new(
                            match route.base_url.as_deref() {
                                Some(base_url) => {
                                    heycode_llm::OpenRouterProvider::from_credential_with_transport_at(
                                        credential.clone(),
                                        Some(route.model.clone()),
                                        http.as_ref().clone(),
                                        options,
                                        base_url,
                                    )
                                }
                                None => {
                                    heycode_llm::OpenRouterProvider::from_credential_with_transport(
                                        credential.clone(),
                                        Some(route.model.clone()),
                                        http.as_ref().clone(),
                                        options,
                                    )
                                }
                            }
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        )
                    }
                    "openai" => {
                        let settings = settings.ok_or_else(|| heycode_core::CoreError::other("OpenAI settings missing"))?;
                        let policy = resolve_openai_prompt_cache_policy(settings)
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let hosted_tools =
                            OpenAiConfiguredHostedToolPolicy::resolve(settings, &route.model)
                                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let provider = match route.base_url.as_deref() {
                            Some(base_url) => {
                                heycode_provider_openai::OpenAiProvider::with_base_url_and_credential(
                                    http.as_ref().clone(),
                                    base_url,
                                    credential,
                                    Some(route.model.clone()),
                                )
                            }
                            None => heycode_provider_openai::OpenAiProvider::with_credential(
                                http.as_ref().clone(),
                                credential,
                                Some(route.model.clone()),
                            ),
                        }
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        let provider = hosted_tools
                            .configure(provider)
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
                        Arc::new(
                            policy
                                .apply_to(provider)
                                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        )
                    }
                    other => {
                        let spec = heycode_provider_compatible::spec(other).ok_or_else(|| {
                            heycode_core::CoreError::other(unselectable_provider(other))
                        })?;
                        Arc::new(
                            heycode_provider_compatible::CompatibleProvider::new(
                                *spec,
                                http.as_ref().clone(),
                                route.base_url.as_deref().unwrap_or(spec.base_url),
                                route.model.clone(),
                                credential,
                            )
                            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                        )
                    }
                })
}

struct ExplicitProviderActivator {
    http: Arc<HttpService>,
    credentials: Arc<CredentialsService>,
    settings: Arc<heycode_settings::SettingsService>,
    catalogs: Arc<heycode_llm::CatalogRegistry>,
}

#[async_trait::async_trait]
impl heycode_llm::ProviderActivator for ExplicitProviderActivator {
    async fn build(
        &self,
        provider: &str,
        model: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Arc<dyn Provider>, heycode_llm::LlmError> {
        use heycode_llm::{CatalogRefreshMode, LlmError};
        let invalid = |error: &dyn std::fmt::Display| LlmError::Transport(error.to_string());
        // The command names a product, not a credentialed deployment. These
        // products need independent coordinates and cannot inherit the source.
        match provider {
            "azure-openai" | "bedrock" | "bedrock-mantle" | "vertex-google" | "vertex-claude"
            | "custom-openai" => {
                return Err(LlmError::Transport(format!(
                    "provider `{provider}` requires a separately configured connection with its own deployment, endpoint and credential; activation cannot infer these from the current route"
                )));
            }
            "zai-coding" => return Err(LlmError::Transport(unselectable_provider(provider))),
            "ollama" | "lmstudio" => {}
            name if INFERENCE_PROVIDERS.contains(&name) => {}
            _ => return Err(LlmError::Transport(unselectable_provider(provider))),
        }
        let reference = if provider == "ollama" {
            None
        } else {
            Some(default_provider_reference(provider).map_err(|e| invalid(&e))?)
        };
        if !matches!(provider, "ollama" | "lmstudio") {
            let query = provider_credential_query(provider, reference.unwrap_or_default())
                .map_err(|e| invalid(&e))?;
            // Resolve only the target reference. The provider will resolve it
            // again per operation, so rotation is retained after activation.
            if self
                .credentials
                .resolve(&query)
                .map_err(|e| invalid(&e))?
                .is_none()
            {
                return Err(LlmError::Transport(format!(
                    "target provider `{provider}` requires its own credential `{}`",
                    query.reference.as_str()
                )));
            }
        }
        if provider == "minimax" {
            heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::PayAsYouGo>::international()
                .resolve(&self.credentials).map_err(|e| invalid(&e))?;
        } else if provider == "minimax-token-plan" {
            heycode_provider_minimax::MiniMaxProfile::<heycode_provider_minimax::TokenPlan>::international()
                .resolve(&self.credentials).map_err(|e| invalid(&e))?;
        }
        let catalog = self
            .catalogs
            .refresh(
                provider,
                CatalogRefreshMode::PreferCache,
                cancellation.clone(),
            )
            .await
            .map_err(|e| invalid(&e))?;
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| invalid(&e))?
            .as_millis()
            .try_into()
            .map_err(|e| invalid(&e))?;
        catalog
            .snapshot
            .resolve_model(model, at_ms)
            .map_err(|e| invalid(&e))?;
        if cancellation.is_cancelled() {
            return Err(LlmError::Provider(heycode_llm::ProviderFailure::new(
                heycode_llm::ProviderErrorClass::Cancelled,
                heycode_llm::ProviderFailureOrigin::Local,
            )));
        }
        match provider {
            "ollama" => Ok(Arc::new(
                heycode_provider_lmstudio::OllamaInference::new(
                    self.http.as_ref().clone(),
                    heycode_provider_lmstudio::OllamaEndpoint::local(),
                    model,
                )
                .map_err(|e| invalid(&e))?,
            )),
            "google" => {
                let query =
                    api_key_query(reference.unwrap_or_default()).map_err(|e| invalid(&e))?;
                let evidence = heycode_provider_google::GoogleGeminiModelEvidence::new(
                    model,
                    catalog.snapshot.models.clone(),
                )
                .map_err(|e| invalid(&e))?;
                let config = heycode_provider_google::google_developer_config_from_settings(
                    &self.settings,
                    query,
                    evidence,
                )
                .map_err(|e| invalid(&e))?;
                Ok(Arc::new(
                    config
                        .build_developer_provider(self.http.as_ref().clone(), &self.credentials)
                        .map_err(|e| invalid(&e))?,
                ))
            }
            _ => build_credential_provider(
                &CredentialRoute {
                    provider_name: provider.into(),
                    model: model.into(),
                    reference: reference.unwrap_or_default().into(),
                    reference_explicit: false,
                    protocol: LlmProtocolCfg::Auto,
                    base_url: None,
                },
                &self.http,
                &self.credentials,
                Some(&self.settings),
            )
            .map_err(|e| invalid(&e)),
        }
    }
}

pub(crate) fn plugin() -> Box<dyn Plugin> {
    struct ActivationPlugin;
    impl Plugin for ActivationPlugin {
        fn name(&self) -> &'static str {
            "provider-activation"
        }
        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Provider],
            )
        }
        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_llm::SERVICE_PROVIDERS,
                heycode_llm::SERVICE_MODELS,
                SERVICE_CREDENTIALS,
                SERVICE_HTTP,
                heycode_settings::SERVICE_SETTINGS,
            ]
        }
        fn apply(&self, context: &mut Context) -> Result<(), heycode_core::CoreError> {
            fn service<T: Send + Sync + 'static>(
                context: &Context,
                key: heycode_core::ServiceKey,
            ) -> Result<Arc<T>, heycode_core::CoreError> {
                context.get(key).ok_or_else(|| {
                    heycode_core::CoreError::other("provider activation dependency missing")
                })
            }
            let registry: Arc<ProviderRegistry> = service(context, heycode_llm::SERVICE_PROVIDERS)?;
            let factory = Arc::new(ExplicitProviderActivator {
                http: service(context, SERVICE_HTTP)?,
                credentials: service(context, SERVICE_CREDENTIALS)?,
                settings: service(context, heycode_settings::SERVICE_SETTINGS)?,
                catalogs: service(context, heycode_llm::SERVICE_MODELS)?,
            });
            let registration = registry
                .install_activator(factory)
                .map_err(|e| heycode_core::CoreError::other(e.to_string()))?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }
    Box::new(ActivationPlugin)
}
