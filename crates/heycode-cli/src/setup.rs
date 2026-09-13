//! Service-backed provider/model choices for terminal setup consumers.

use std::sync::Arc;

use heycode_llm::{
    CatalogError, CatalogFreshness, CatalogRefreshMode, CatalogRegistry, ChatRequest, ChunkStream,
    LlmError, LlmSelection, Provider, ProviderInfo, ProviderProfile, ProviderRegistry,
    SERVICE_MODELS, SERVICE_PROVIDERS, llm_plugin, model_catalog_plugin,
};
use tokio_util::sync::CancellationToken;

/// One provider row projected from the composed inference-provider service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupProviderChoice {
    /// Stable provider id.
    pub id: String,
    /// Provider-owned display label.
    pub display_name: String,
    /// Provider-owned model default.
    pub default_model: String,
    /// Provider-owned non-secret credential reference.
    pub credential_reference: Option<String>,
    /// Whether a model-catalog plugin is registered for this provider.
    pub has_catalog: bool,
}

/// Provenance of model choices shown by setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupModelSource {
    /// A provider fetch completed during this request.
    LiveCatalog,
    /// A still-fresh cached generation was used.
    FreshCatalogCache,
    /// Last-good cached rows were used after a visible refresh warning.
    StaleCatalogFallback,
    /// No usable catalog exists; only the provider-owned default is known.
    ProviderDefault,
}

/// One selectable model row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupModelChoice {
    /// Provider-native id persisted on selection.
    pub id: String,
    /// Safe display label.
    pub display_name: String,
}

/// Complete model selection page for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupModelChoices {
    /// Provider id queried.
    pub provider: String,
    /// Selectable rows sorted by id.
    pub models: Vec<SetupModelChoice>,
    /// Default shown by the prompt.
    pub recommended: String,
    /// Evidence source.
    pub source: SetupModelSource,
    /// Whether a caller may enter an id absent from the rows. This is true only
    /// when no catalog could prove the provider's model set.
    pub custom_allowed: bool,
    /// Safe visible catalog/fallback warning.
    pub warning: Option<String>,
}

/// Immutable setup projection over composed provider and model services.
pub struct SetupCatalog {
    providers: Vec<SetupProviderChoice>,
    models: Arc<CatalogRegistry>,
}

/// Provider-owned hosted and local connection metadata for the product wizard.
#[must_use]
pub fn connection_profiles() -> Vec<heycode_llm::ConnectionProfile> {
    let mut profiles = vec![
        heycode_provider_anthropic::anthropic_profile().into(),
        heycode_llm::DeepSeekProvider::setup_profile().into(),
        heycode_provider_google::google_connection_profile(),
        heycode_provider_openai::openai_profile().into(),
        heycode_provider_openrouter::openrouter_profile().into(),
        heycode_provider_aws::bedrock_connection_profile(),
        heycode_provider_google::vertex_google_connection_profile(),
        heycode_provider_azure::azure_openai_connection_profile(),
        heycode_provider_openai_compatible::custom_openai_connection_profile(),
    ];
    profiles.extend(
        heycode_provider_compatible::builtin_specs()
            .iter()
            .map(|spec| spec.profile().into()),
    );
    profiles.extend(heycode_provider_lmstudio::local_connection_profiles());
    profiles
}

/// Filesystem roots used by the restricted setup discovery world.
pub struct SetupWorldOptions {
    /// Non-watching user settings path.
    pub settings_user_path: std::path::PathBuf,
    /// Credential-provider root. Setup may perform the normal safe legacy migration.
    pub credentials_root: std::path::PathBuf,
    /// Model-catalog cache path.
    pub catalog_cache_path: std::path::PathBuf,
}

/// Owned restricted provider/catalog world. Dropping it unwinds every registration.
pub struct SetupWorld {
    context: Option<heycode_core::Context>,
    catalog: SetupCatalog,
}

impl SetupWorld {
    /// Service-backed choices consumed by the terminal wizard.
    #[must_use]
    pub fn catalog(&self) -> &SetupCatalog {
        &self.catalog
    }

    /// Unwind all setup-world effects. Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(mut context) = self.context.take() {
            context.shutdown();
        }
    }
}

impl Drop for SetupWorld {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SetupMetadataProvider {
    name: &'static str,
    profile: ProviderProfile,
}

impl Provider for SetupMetadataProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.to_owned(),
            default_model: self.profile.default_model.clone(),
        }
    }

    fn credential_reference(&self) -> Option<&str> {
        self.profile.credential_reference.as_deref()
    }

    fn descriptor(&self) -> heycode_llm::ProviderDescriptor {
        self.profile.descriptor.clone()
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::iter([Err(LlmError::Transport(
            "setup metadata providers cannot dispatch inference".to_owned(),
        ))]))
    }
}

/// Compose only services needed to discover setup provider/model choices.
/// No session, agent, tools, TUI, watcher or MCP process is created, and no
/// live inference provider/client is constructed. Credential-file migration is
/// intentionally allowed because setup owns credential repair.
///
/// # Errors
/// Settings/credential/catalog/plugin construction or service projection failures.
pub fn compose_setup_world(options: SetupWorldOptions) -> anyhow::Result<SetupWorld> {
    let anthropic_profile = heycode_provider_anthropic::anthropic_profile();
    let deepseek_profile = heycode_llm::DeepSeekProvider::setup_profile();
    let google_profile = heycode_provider_google::google_profile();
    let openai_profile = heycode_provider_openai::openai_profile();
    let openrouter_profile = heycode_provider_openrouter::openrouter_profile();
    let mut providers: Vec<Arc<dyn Provider>> = vec![
        Arc::new(SetupMetadataProvider {
            name: "anthropic",
            profile: anthropic_profile.clone(),
        }),
        Arc::new(SetupMetadataProvider {
            name: heycode_llm::DeepSeekProvider::NAME,
            profile: deepseek_profile.clone(),
        }),
        Arc::new(SetupMetadataProvider {
            name: "google",
            profile: google_profile.clone(),
        }),
        Arc::new(SetupMetadataProvider {
            name: "openai",
            profile: openai_profile.clone(),
        }),
        Arc::new(SetupMetadataProvider {
            name: heycode_llm::OpenRouterProvider::NAME,
            profile: openrouter_profile,
        }),
    ];
    let reference = deepseek_profile
        .credential_reference
        .ok_or_else(|| anyhow::anyhow!("DeepSeek setup profile has no credential reference"))?;
    providers.extend(
        heycode_provider_compatible::builtin_specs()
            .iter()
            .map(|spec| {
                Arc::new(SetupMetadataProvider {
                    name: spec.id,
                    profile: spec.profile(),
                }) as Arc<dyn Provider>
            }),
    );
    let query = heycode_credentials::CredentialQuery::new(
        heycode_credentials::CredentialReference::new(reference)?,
        heycode_credentials::CredentialKind::new("api-key")?,
    );
    let catalog_query = |profile: &ProviderProfile| -> anyhow::Result<_> {
        let reference = profile
            .credential_reference
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("setup provider has no credential reference"))?;
        Ok(heycode_credentials::CredentialQuery::new(
            heycode_credentials::CredentialReference::new(reference)?,
            heycode_credentials::CredentialKind::new("api-key")?,
        ))
    };
    let anthropic_query = catalog_query(&anthropic_profile)?;
    let google_query = catalog_query(&google_profile)?;
    let openai_query = catalog_query(&openai_profile)?;
    let azure_query = heycode_credentials::CredentialQuery::new(
        heycode_credentials::CredentialReference::new(
            heycode_provider_azure::AZURE_OPENAI_API_KEY_REFERENCE,
        )?,
        heycode_credentials::CredentialKind::new("api-key")?,
    );
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_settings_file::file_settings_plugin(
            heycode_settings_file::FileSettingsConfig::user(options.settings_user_path)
                .without_watch(),
        ),
        heycode_http::http_plugin(),
        heycode_credentials::credentials_plugin(),
        heycode_credentials_env::environment_credentials_plugin(
            heycode_credentials_env::EnvironmentCredentialProvider::process()?,
        ),
        heycode_credentials_file::file_credentials_plugin(
            heycode_credentials_file::FileCredentialConfig::new(options.credentials_root),
        ),
        model_catalog_plugin(std::time::Duration::from_secs(5 * 60)),
        heycode_catalog_file::file_catalog_persistence_plugin(
            heycode_catalog_file::FileCatalogConfig::new(options.catalog_cache_path),
        ),
        heycode_provider_deepseek::deepseek_catalog_plugin(
            heycode_provider_deepseek::DeepSeekCatalogConfig::official(query),
        ),
        heycode_provider_openrouter::openrouter_catalog_plugin(
            heycode_provider_openrouter::OpenRouterCatalogConfig::official(),
        ),
        heycode_provider_anthropic::anthropic_catalog_plugin(
            heycode_provider_anthropic::AnthropicCatalogConfig::official(anthropic_query),
        ),
        heycode_provider_openai::openai_catalog_plugin(
            heycode_provider_openai::OpenAiCatalogConfig::official(openai_query),
        ),
        heycode_provider_google::google_catalog_plugin(
            heycode_provider_google::GoogleCatalogConfig::api_key(google_query),
        ),
        heycode_provider_azure::azure_openai_catalog_plugin(
            heycode_provider_azure::AzureOpenAiCatalogConfig::api_key(azure_query),
        ),
        heycode_provider_openai_compatible::custom_openai_catalog_plugin(
            heycode_provider_openai_compatible::CustomOpenAiCatalogConfig::discovery(),
        ),
        heycode_provider_compatible::compatible_catalog_plugin(
            heycode_provider_compatible::builtin_specs()
                .iter()
                .map(|spec| {
                    (
                        *spec,
                        spec.models_url.into(),
                        spec.credential_reference.into(),
                    )
                })
                .collect(),
        ),
        llm_plugin(
            LlmSelection {
                provider_name: heycode_llm::DeepSeekProvider::NAME.to_owned(),
                model: heycode_llm::DeepSeekProvider::DEFAULT_MODEL.to_owned(),
            },
            providers,
        ),
    ];
    let context = heycode_core::compose(&plugins)?;
    let catalog = SetupCatalog::from_context(&context)?;
    Ok(SetupWorld {
        context: Some(context),
        catalog,
    })
}

impl SetupCatalog {
    /// Resolve and validate the two setup services from a composed context.
    ///
    /// # Errors
    /// Missing services, poisoned catalog state, mismatched provider identity,
    /// blank defaults or an empty provider registry fail loud.
    pub fn from_context(context: &heycode_core::Context) -> anyhow::Result<Self> {
        let provider_registry = context
            .get::<ProviderRegistry>(SERVICE_PROVIDERS)
            .ok_or_else(|| anyhow::anyhow!("setup provider service missing"))?;
        let models = context
            .get::<CatalogRegistry>(SERVICE_MODELS)
            .ok_or_else(|| anyhow::anyhow!("setup model-catalog service missing"))?;
        let catalog_ids: std::collections::BTreeSet<_> = models
            .descriptors()?
            .into_iter()
            .map(|descriptor| descriptor.id)
            .collect();
        let mut providers = Vec::new();
        for profile in provider_registry.profiles() {
            let id = profile.descriptor.id.trim();
            if id != profile.registry_name
                || id.is_empty()
                || profile.default_model.trim().is_empty()
            {
                return Err(anyhow::anyhow!(
                    "setup provider registry name, descriptor id and default model are inconsistent"
                ));
            }
            providers.push(SetupProviderChoice {
                id: id.to_owned(),
                display_name: profile.descriptor.display_name,
                default_model: profile.default_model,
                credential_reference: profile.credential_reference,
                has_catalog: catalog_ids.contains(id),
            });
        }
        if providers.is_empty() {
            return Err(anyhow::anyhow!(
                "setup provider service has no registered providers"
            ));
        }
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self { providers, models })
    }

    /// Stable provider rows.
    #[must_use]
    pub fn providers(&self) -> Vec<SetupProviderChoice> {
        self.providers.clone()
    }

    /// Refresh/select model choices for one provider.
    ///
    /// A missing/unusable catalog falls back visibly to the provider-owned
    /// default. Cancellation never degrades into fallback.
    ///
    /// # Errors
    /// Unknown provider, cancellation or registry failures.
    pub async fn models(
        &self,
        provider: &str,
        cancellation: CancellationToken,
    ) -> anyhow::Result<SetupModelChoices> {
        let profile = self
            .providers
            .iter()
            .find(|profile| profile.id == provider)
            .ok_or_else(|| anyhow::anyhow!("unknown setup provider `{provider}`"))?;
        match self
            .models
            .refresh(
                provider,
                CatalogRefreshMode::PreferCache,
                cancellation.clone(),
            )
            .await
        {
            Ok(view) => {
                let at_ms = unix_time_ms();
                let mut models: Vec<_> = view
                    .snapshot
                    .models
                    .iter()
                    .filter(|model| model.lifecycle.is_selectable(at_ms))
                    .map(|model| SetupModelChoice {
                        id: model.id.clone(),
                        display_name: model.display_name.clone(),
                    })
                    .collect();
                models.sort_by(|left, right| left.id.cmp(&right.id));
                if models.is_empty() {
                    return Ok(provider_default(
                        profile,
                        "model catalog has no selectable rows; using the provider default",
                    ));
                }
                let recommended = if models.iter().any(|model| model.id == profile.default_model) {
                    profile.default_model.clone()
                } else {
                    models[0].id.clone()
                };
                let warning = view.warning.map(|warning| warning.to_string());
                Ok(SetupModelChoices {
                    provider: provider.to_owned(),
                    models,
                    recommended,
                    source: match view.freshness {
                        CatalogFreshness::Live => SetupModelSource::LiveCatalog,
                        CatalogFreshness::FreshCache => SetupModelSource::FreshCatalogCache,
                        CatalogFreshness::StaleFallback => SetupModelSource::StaleCatalogFallback,
                    },
                    custom_allowed: false,
                    warning,
                })
            }
            Err(CatalogError::Cancelled { .. }) if cancellation.is_cancelled() => {
                Err(anyhow::anyhow!("setup model discovery cancelled"))
            }
            Err(error) => Ok(provider_default(profile, &error.to_string())),
        }
    }
}

/// Resolve a provider from a one-based row number or exact provider id.
///
/// # Errors
/// Blank/unknown selections fail with the dynamic available ids.
pub fn resolve_setup_provider<'a>(
    choices: &'a [SetupProviderChoice],
    input: &str,
) -> anyhow::Result<&'a SetupProviderChoice> {
    let input = input.trim();
    if let Ok(index) = input.parse::<usize>()
        && let Some(choice) = index.checked_sub(1).and_then(|index| choices.get(index))
    {
        return Ok(choice);
    }
    choices
        .iter()
        .find(|choice| choice.id == input)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown provider `{input}` — choose 1-{} or one of: {}",
                choices.len(),
                choices
                    .iter()
                    .map(|choice| choice.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Resolve a model from a one-based row number, exact id or (only when no
/// catalog exists) a custom provider-native id.
///
/// # Errors
/// Blank or catalog-rejected ids fail loud.
pub fn resolve_setup_model(choices: &SetupModelChoices, input: &str) -> anyhow::Result<String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(choices.recommended.clone());
    }
    if let Ok(index) = input.parse::<usize>()
        && let Some(choice) = index
            .checked_sub(1)
            .and_then(|index| choices.models.get(index))
    {
        return Ok(choice.id.clone());
    }
    if choices.models.iter().any(|choice| choice.id == input) || choices.custom_allowed {
        return Ok(input.to_owned());
    }
    Err(anyhow::anyhow!(
        "model `{input}` is absent from the live catalog for `{}`",
        choices.provider
    ))
}

fn provider_default(profile: &SetupProviderChoice, warning: &str) -> SetupModelChoices {
    SetupModelChoices {
        provider: profile.id.clone(),
        models: vec![SetupModelChoice {
            id: profile.default_model.clone(),
            display_name: profile.default_model.clone(),
        }],
        recommended: profile.default_model.clone(),
        source: SetupModelSource::ProviderDefault,
        custom_allowed: true,
        warning: Some(format!(
            "no model catalog is available for `{}` ({warning}); using provider default",
            profile.id
        )),
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0)
}
