//! Effect-owned production GitHub release-manager service.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use heycode_core::{CoreError, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey};

use crate::{GhAttestationVerifier, ReleaseManager, ReleasePlatform, ReleaseTrustPolicy};

/// Effect-owned release operation service.
pub const SERVICE_RELEASE_MANAGER: ServiceKey = ServiceKey::new("release-manager");

/// Explicit production verifier/install configuration.
#[derive(Clone)]
pub struct GhReleaseManagerConfig {
    install_root: PathBuf,
    scratch_root: PathBuf,
    program: OsString,
    platform: ReleasePlatform,
    trust: ReleaseTrustPolicy,
}

impl std::fmt::Debug for GhReleaseManagerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GhReleaseManagerConfig")
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl GhReleaseManagerConfig {
    /// Bind explicit install/scratch/program/platform/trust inputs.
    #[must_use]
    pub fn new(
        install_root: PathBuf,
        scratch_root: PathBuf,
        program: OsString,
        platform: ReleasePlatform,
        trust: ReleaseTrustPolicy,
    ) -> Self {
        Self {
            install_root,
            scratch_root,
            program,
            platform,
            trust,
        }
    }
}

/// Build the production GitHub-attestation release-manager plugin.
#[must_use]
pub fn gh_release_manager_plugin(config: GhReleaseManagerConfig) -> Box<dyn Plugin> {
    struct GhReleaseManagerPlugin(GhReleaseManagerConfig);

    impl Plugin for GhReleaseManagerPlugin {
        fn name(&self) -> &'static str {
            "release-manager-gh"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[heycode_exec::SERVICE_SUBPROCESS]
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_RELEASE_MANAGER]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let subprocess = context
                .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service missing"))?;
            let verifier = Arc::new(
                GhAttestationVerifier::new(
                    subprocess.as_ref().clone(),
                    &self.0.program,
                    &self.0.scratch_root,
                )
                .map_err(|error| CoreError::other(error.to_string()))?,
            );
            let verifier_trait: Arc<dyn crate::ReleaseSignatureVerifier> = verifier.clone();
            let manager = ReleaseManager::new(
                &self.0.install_root,
                self.0.platform.clone(),
                self.0.trust.clone(),
                verifier_trait,
            )
            .map_err(|error| CoreError::other(error.to_string()))?;
            let closed = manager.close_handle();
            context.effect(move || {
                closed.store(true, Ordering::Release);
                verifier.close();
            });
            context.provide(SERVICE_RELEASE_MANAGER, self.name(), manager)
        }
    }

    Box::new(GhReleaseManagerPlugin(config))
}
