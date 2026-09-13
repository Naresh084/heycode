//! Settings-owned doctor contribution.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_doctor::{
    DoctorCheck, DoctorCheckId, DoctorError, DoctorOutcome, DoctorRegistry, SERVICE_DOCTOR,
};
use tokio_util::sync::CancellationToken;

use crate::{SERVICE_SETTINGS, SettingsService};

struct SettingsCheck {
    id: DoctorCheckId,
    settings: Arc<SettingsService>,
}

#[async_trait]
impl DoctorCheck for SettingsCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.id
    }

    async fn run(&self, _cancellation: CancellationToken) -> Result<DoctorOutcome, DoctorError> {
        if self.settings.writable() {
            DoctorOutcome::pass(
                "settings.writable",
                "The effective settings service has a durable user writer.",
            )
        } else {
            DoctorOutcome::warning(
                "settings.read-only",
                "The effective settings service is read-only.",
            )?
            .with_repair("Enable a writable user settings provider before changing settings.")
        }
    }
}

/// Contribute settings service health to the shared doctor registry.
#[must_use]
pub fn settings_doctor_plugin() -> Box<dyn Plugin> {
    struct SettingsDoctorPlugin;
    impl Plugin for SettingsDoctorPlugin {
        fn name(&self) -> &'static str {
            "doctor-settings"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "doctor-settings",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Diagnostic],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_DOCTOR, SERVICE_SETTINGS]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::DoctorCheck,
                "settings",
            )]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let doctor = context
                .get::<DoctorRegistry>(SERVICE_DOCTOR)
                .ok_or_else(|| CoreError::other("doctor registry missing"))?;
            let settings = context
                .get::<SettingsService>(SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service missing"))?;
            let id = DoctorCheckId::new("settings")
                .map_err(|error| CoreError::other(error.to_string()))?;
            doctor
                .register(context, Arc::new(SettingsCheck { id, settings }))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(SettingsDoctorPlugin)
}
