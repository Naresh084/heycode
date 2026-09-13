//! Credential-registry-owned doctor contribution.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_doctor::{
    DoctorCheck, DoctorCheckId, DoctorError, DoctorOutcome, DoctorRegistry, SERVICE_DOCTOR,
};
use tokio_util::sync::CancellationToken;

use crate::{CredentialsService, SERVICE_CREDENTIALS};

struct CredentialsCheck {
    id: DoctorCheckId,
    credentials: Arc<CredentialsService>,
}

#[async_trait]
impl DoctorCheck for CredentialsCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.id
    }

    async fn run(&self, _cancellation: CancellationToken) -> Result<DoctorOutcome, DoctorError> {
        match self.credentials.provider_count() {
            Ok(0) => DoctorOutcome::warning(
                "credentials.no-providers",
                "The credential registry has no active providers.",
            )?
            .with_repair("Enable at least one credential provider before connecting a runtime."),
            Ok(_) => DoctorOutcome::pass(
                "credentials.providers-ready",
                "The credential registry has active providers.",
            ),
            Err(_) => DoctorOutcome::failure(
                "credentials.registry-unavailable",
                "The credential registry is unavailable.",
            )?
            .with_repair("Restart heycode and rerun doctor before resolving credentials."),
        }
    }
}

/// Contribute safe credential-registry health without resolving any secret.
#[must_use]
pub fn credentials_doctor_plugin() -> Box<dyn Plugin> {
    struct CredentialsDoctorPlugin;
    impl Plugin for CredentialsDoctorPlugin {
        fn name(&self) -> &'static str {
            "doctor-credentials"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "doctor-credentials",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Diagnostic],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_DOCTOR, SERVICE_CREDENTIALS]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::DoctorCheck,
                "credentials",
            )]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let doctor = context
                .get::<DoctorRegistry>(SERVICE_DOCTOR)
                .ok_or_else(|| CoreError::other("doctor registry missing"))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::other("credential registry missing"))?;
            let id = DoctorCheckId::new("credentials")
                .map_err(|error| CoreError::other(error.to_string()))?;
            doctor
                .register(context, Arc::new(CredentialsCheck { id, credentials }))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(CredentialsDoctorPlugin)
}
