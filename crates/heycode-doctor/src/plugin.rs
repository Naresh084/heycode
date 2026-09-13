//! Built-in doctor service and K08 composition-check plugins.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};

use crate::{DoctorCheck, DoctorCheckId, DoctorOutcome, DoctorRegistry, SERVICE_DOCTOR};

/// Provide the plugin-contributed doctor registry.
#[must_use]
pub fn doctor_plugin() -> Box<dyn Plugin> {
    struct DoctorPlugin;
    impl Plugin for DoctorPlugin {
        fn name(&self) -> &'static str {
            "doctor"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "doctor",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_DOCTOR]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(SERVICE_DOCTOR, "doctor", DoctorRegistry::new())
        }
    }
    Box::new(DoctorPlugin)
}

struct CompositionCheck {
    id: DoctorCheckId,
    report: heycode_core::CompositionReport,
}

#[async_trait]
impl DoctorCheck for CompositionCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.id
    }

    async fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<DoctorOutcome, crate::DoctorError> {
        let outcome = if self.report.healthy {
            DoctorOutcome::pass("composition.healthy", "The selected plugin graph is valid.")
        } else {
            DoctorOutcome::failure(
                "composition.invalid",
                "The selected plugin graph has blocking diagnostics.",
            )
        };
        outcome
            .and_then(|outcome| {
                outcome
                    .with_repair("Review the attached composition diagnostics and profile sources.")
            })
            .map(|outcome| outcome.with_composition(self.report.clone()))
    }
}

/// Contribute K08's side-effect-free graph report as one typed doctor check.
#[must_use]
pub fn composition_doctor_plugin(report: heycode_core::CompositionReport) -> Box<dyn Plugin> {
    struct CompositionDoctorPlugin(heycode_core::CompositionReport);
    impl Plugin for CompositionDoctorPlugin {
        fn name(&self) -> &'static str {
            "doctor-composition"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "doctor-composition",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Diagnostic],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_DOCTOR]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::DoctorCheck,
                "composition",
            )]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let doctor = context
                .get::<DoctorRegistry>(SERVICE_DOCTOR)
                .ok_or_else(|| CoreError::other("doctor registry missing"))?;
            let id = DoctorCheckId::new("composition")
                .map_err(|error| CoreError::other(error.to_string()))?;
            doctor
                .register(
                    context,
                    Arc::new(CompositionCheck {
                        id,
                        report: self.0.clone(),
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(CompositionDoctorPlugin(report))
}
