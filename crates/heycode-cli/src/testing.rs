//! Shared isolated real-composition harness for integration and conformance
//! tests. Every product plugin still goes through the production factory
//! table and loader; only inference is replaced by a caller-owned provider,
//! defaulting to the sanctioned fake.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use heycode_config::Config;
use heycode_llm::testing::FakeProvider;
use heycode_llm::{CatalogPersistence as _, CatalogSnapshot, Provider};

use crate::{WorldOptions, compose_world};

/// Isolated inputs for one production-loader composition.
pub struct RealCompositionHarness {
    root: tempfile::TempDir,
    config: Config,
    profile_layers: Vec<heycode_config::ProfileLayer>,
    provider: Option<Arc<dyn Provider>>,
    use_fake_provider: bool,
    onboarding_required: bool,
    workspace_trusted: bool,
    transport: Option<Arc<dyn heycode_http::HttpTransport>>,
}

impl RealCompositionHarness {
    /// Create one isolated filesystem world with compiled defaults.
    ///
    /// # Errors
    /// Temporary root or workspace directory creation failure.
    pub fn new() -> anyhow::Result<Self> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("workspace"))?;
        Ok(Self {
            root,
            config: Config::defaults(),
            profile_layers: Vec::new(),
            provider: None,
            use_fake_provider: true,
            onboarding_required: false,
            workspace_trusted: false,
            transport: None,
        })
    }

    /// Isolated world root retained until the composed world shuts down.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Sessions root used by the production session plugin.
    #[must_use]
    pub fn sessions_dir(&self) -> PathBuf {
        self.root().join("sessions")
    }

    /// Owner-only attachment root used by the production attachment plugin.
    #[must_use]
    pub fn attachments_dir(&self) -> PathBuf {
        self.root().join("attachments")
    }

    /// User settings path used by the production settings provider.
    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.root().join("settings.toml")
    }

    /// Credential-provider root used by the production credential stack.
    #[must_use]
    pub fn credentials_root(&self) -> PathBuf {
        self.root().join("credentials-home")
    }

    /// Durable model-catalog cache path used by the production cache plugin.
    #[must_use]
    pub fn catalog_cache_path(&self) -> PathBuf {
        self.root().join("cache/models.json")
    }

    /// Seed one validated catalog generation into the isolated durable cache.
    /// Any prior test generation is replaced atomically before composition.
    ///
    /// # Errors
    /// Unsafe cache paths, invalid snapshots, serialization or I/O failure.
    pub fn seed_catalog_snapshot(&self, snapshot: CatalogSnapshot) -> anyhow::Result<()> {
        let persistence = heycode_catalog_file::FileCatalogPersistence::open(
            heycode_catalog_file::FileCatalogConfig::new(self.catalog_cache_path()),
        )?;
        persistence.save(
            &[Arc::new(snapshot)],
            &tokio_util::sync::CancellationToken::new(),
        )?;
        Ok(())
    }

    fn trust(&self) -> anyhow::Result<heycode_trust::WorkspaceTrustService> {
        let service = heycode_trust::WorkspaceTrustService::memory(
            self.root().join("workspace"),
            crate::project_content_policy(),
        )?;
        if self.workspace_trusted {
            service.set_session(
                heycode_trust::WorkspaceTrustDecision::Trusted,
                service.snapshot()?.revision(),
            )?;
        }
        Ok(service)
    }

    /// Explicit fixture-only trust for testing project guidance and declarations.
    #[must_use]
    pub fn with_trusted_workspace(mut self) -> Self {
        self.workspace_trusted = true;
        self
    }

    /// Mutable configuration before composition.
    #[must_use]
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    /// Add one already-discovered profile layer. The production resolver owns
    /// ordering and conflict validation during composition.
    #[must_use]
    pub fn with_profile_layer(mut self, layer: heycode_config::ProfileLayer) -> Self {
        self.profile_layers.push(layer);
        self
    }

    /// Replace the default scripted fake with one caller-owned provider while
    /// retaining the complete production factory and loader path.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self.use_fake_provider = true;
        self
    }

    /// Use the configured production inference route instead of swapping in a
    /// fake. Tests selecting this path must seed isolated credentials and must
    /// not dispatch network I/O.
    #[must_use]
    pub fn without_fake_provider(mut self) -> Self {
        self.provider = None;
        self.use_fake_provider = false;
        self
    }

    /// Substitute only HTTP transport, retaining production provider factories,
    /// catalogs, settings and the Agent. Process-environment credentials are
    /// replaced by an empty map; seed keys in the isolated credential store.
    #[must_use]
    pub fn with_http_transport(mut self, transport: Arc<dyn heycode_http::HttpTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Exercise interactive first-run or saved-connection recovery composition.
    #[must_use]
    pub fn with_onboarding_required(mut self) -> Self {
        self.onboarding_required = true;
        self
    }

    fn inference_provider(&self) -> Option<Arc<dyn Provider>> {
        self.use_fake_provider.then(|| {
            self.provider
                .clone()
                .unwrap_or_else(|| Arc::new(FakeProvider::new(Vec::new())))
        })
    }

    /// Resolve and inspect the production graph without invoking plugin apply.
    ///
    /// # Errors
    /// World/factory/profile resolution failure.
    pub fn inspect(&self) -> anyhow::Result<heycode_core::CompositionReport> {
        let options = WorldOptions {
            config: &self.config,
            trust: self.trust()?,
            config_migration: None,
            profile_layers: &self.profile_layers,
            sessions_dir: self.sessions_dir(),
            attachments_dir: self.attachments_dir(),
            attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            session_source: if self.onboarding_required {
                heycode_session::SessionSource::Interactive
            } else {
                heycode_session::SessionSource::Headless
            },
            approval_prompter: crate::ApprovalPrompter::Proxied,
            settings_user_path: self.settings_path(),
            credentials_root: self.credentials_root(),
            catalog_cache_path: self.catalog_cache_path(),
            settings_watch: false,
            onboarding_required: self.onboarding_required,
            credential_validated_at_ms: None,
            cwd: self.root().join("workspace"),
            fake: self.inference_provider(),
            resume: None,
        };
        let plugins = crate::resolve_world_plugins(&options)?;
        Ok(heycode_core::inspect_composition(&plugins))
    }

    /// Consume the harness and compose through the complete production loader.
    ///
    /// The returned owner shuts down plugin effects before deleting its
    /// isolated root.
    ///
    /// # Errors
    /// Any production composition/config/plugin failure.
    pub fn compose(self) -> anyhow::Result<ComposedTestWorld> {
        let provider = self.inference_provider();
        let options = WorldOptions {
            config: &self.config,
            trust: self.trust()?,
            config_migration: None,
            profile_layers: &self.profile_layers,
            sessions_dir: self.sessions_dir(),
            attachments_dir: self.attachments_dir(),
            attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            session_source: if self.onboarding_required {
                heycode_session::SessionSource::Interactive
            } else {
                heycode_session::SessionSource::Headless
            },
            approval_prompter: crate::ApprovalPrompter::Proxied,
            settings_user_path: self.settings_path(),
            credentials_root: self.credentials_root(),
            catalog_cache_path: self.catalog_cache_path(),
            settings_watch: false,
            onboarding_required: self.onboarding_required,
            credential_validated_at_ms: None,
            cwd: self.root().join("workspace"),
            fake: provider,
            resume: None,
        };
        let context = if let Some(transport) = &self.transport {
            let empty_environment =
                heycode_credentials_env::EnvironmentCredentialProvider::from_map(
                    Default::default(),
                )?;
            let plugins = crate::resolve_world_plugins(&options)?
                .into_iter()
                .map(|plugin| {
                    if plugin.plugin().name() == "http-reqwest" {
                        heycode_core::ScopedPlugin::new(
                            plugin.scope(),
                            Box::new(TestHttpPlugin(transport.clone())),
                        )
                    } else if plugin.plugin().name() == "credentials-env" {
                        heycode_core::ScopedPlugin::new(
                            plugin.scope(),
                            heycode_credentials_env::environment_credentials_plugin(
                                empty_environment.clone(),
                            ),
                        )
                    } else {
                        plugin
                    }
                })
                .collect::<Vec<_>>();
            heycode_core::compose_scoped(&plugins)?
        } else {
            compose_world(&options)?
        };
        Ok(ComposedTestWorld {
            context,
            root: Some(self.root),
        })
    }
}

struct TestHttpPlugin(Arc<dyn heycode_http::HttpTransport>);
impl heycode_core::Plugin for TestHttpPlugin {
    fn name(&self) -> &'static str {
        "http-reqwest"
    }
    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_http::http_plugin().descriptor()
    }
    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[heycode_http::SERVICE_HTTP]
    }
    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        context.provide(
            heycode_http::SERVICE_HTTP,
            self.name(),
            heycode_http::HttpService::new(self.0.clone()),
        )
    }
}

/// Context owner that guarantees effect teardown precedes temp-root removal.
pub struct ComposedTestWorld {
    context: heycode_core::Context,
    root: Option<tempfile::TempDir>,
}

impl ComposedTestWorld {
    /// Borrow the composed production context.
    #[must_use]
    pub const fn context(&self) -> &heycode_core::Context {
        &self.context
    }

    /// Mutably borrow the context for explicit test actions.
    #[must_use]
    pub const fn context_mut(&mut self) -> &mut heycode_core::Context {
        &mut self.context
    }

    /// Explicitly unwind all effects and remove the isolated root.
    pub fn shutdown(mut self) {
        self.context.shutdown();
        drop(self.root.take());
    }
}

impl Drop for ComposedTestWorld {
    fn drop(&mut self) {
        self.context.shutdown();
        drop(self.root.take());
    }
}
