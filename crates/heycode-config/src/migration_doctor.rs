//! Config-owned redacted migration doctor contribution.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_doctor::{
    ConfigMigrationChangeEvidence, ConfigMigrationDispositionEvidence, ConfigMigrationEvidence,
    ConfigVersionEvidence, DoctorCheck, DoctorCheckId, DoctorError, DoctorOutcome, DoctorRegistry,
    SERVICE_DOCTOR,
};
use tokio_util::sync::CancellationToken;

use crate::{
    ConfigMigrationChange, ConfigMigrationDisposition, ConfigMigrationNotice, ConfigVersionState,
};

enum MigrationCheckState {
    Current,
    Applied(ConfigMigrationEvidence),
    Pending(ConfigMigrationEvidence),
}

struct MigrationCheck {
    id: DoctorCheckId,
    state: MigrationCheckState,
}

#[async_trait]
impl DoctorCheck for MigrationCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.id
    }

    async fn run(&self, _cancellation: CancellationToken) -> Result<DoctorOutcome, DoctorError> {
        match &self.state {
            MigrationCheckState::Current => DoctorOutcome::pass(
                "config.current",
                "The selected configuration uses the current schema.",
            ),
            MigrationCheckState::Applied(evidence) => DoctorOutcome::pass(
                "config.migration-applied",
                "A safe configuration migration was applied with a backup.",
            )
            .map(|outcome| outcome.with_config_migration(evidence.clone())),
            MigrationCheckState::Pending(evidence) => DoctorOutcome::warning(
                "config.migration-pending",
                "A user-owned configuration migration is pending.",
            )?
            .with_repair("Review and apply the semantic migration before relying on new defaults.")
            .map(|outcome| outcome.with_config_migration(evidence.clone())),
        }
    }
}

/// Contribute current/applied/pending config migration health to doctor.
///
/// The input notice already excludes raw source/rendered bytes. This plugin
/// maps its closed semantic changes into the closed doctor evidence schema.
#[must_use]
pub fn config_migration_doctor_plugin(notice: Option<ConfigMigrationNotice>) -> Box<dyn Plugin> {
    struct ConfigMigrationDoctorPlugin(Option<ConfigMigrationNotice>);
    impl Plugin for ConfigMigrationDoctorPlugin {
        fn name(&self) -> &'static str {
            "doctor-config"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "doctor-config",
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
                "config-migration",
            )]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let doctor = context
                .get::<DoctorRegistry>(SERVICE_DOCTOR)
                .ok_or_else(|| CoreError::other("doctor registry missing"))?;
            let id = DoctorCheckId::new("config-migration")
                .map_err(|error| CoreError::other(error.to_string()))?;
            let state = self
                .0
                .as_ref()
                .map_or(MigrationCheckState::Current, |notice| {
                    let evidence = migration_evidence(notice);
                    match notice.disposition {
                        ConfigMigrationDisposition::Applied { .. } => {
                            MigrationCheckState::Applied(evidence)
                        }
                        ConfigMigrationDisposition::Pending => {
                            MigrationCheckState::Pending(evidence)
                        }
                    }
                });
            doctor
                .register(context, Arc::new(MigrationCheck { id, state }))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }
    Box::new(ConfigMigrationDoctorPlugin(notice))
}

fn migration_evidence(notice: &ConfigMigrationNotice) -> ConfigMigrationEvidence {
    ConfigMigrationEvidence {
        path: notice.path.display().to_string(),
        from: match notice.from {
            ConfigVersionState::Unversioned => ConfigVersionEvidence::Unversioned,
            ConfigVersionState::Older(version) => ConfigVersionEvidence::Older { version },
            ConfigVersionState::Current(version) => ConfigVersionEvidence::Current { version },
            ConfigVersionState::Newer(version) => ConfigVersionEvidence::Newer { version },
        },
        to: notice.to,
        disposition: match &notice.disposition {
            ConfigMigrationDisposition::Applied { backup_path } => {
                ConfigMigrationDispositionEvidence::HomeApplied {
                    backup_path: backup_path.display().to_string(),
                }
            }
            ConfigMigrationDisposition::Pending => {
                ConfigMigrationDispositionEvidence::UserOwnedPending
            }
        },
        changes: notice.changes.iter().map(change_evidence).collect(),
    }
}

fn change_evidence(change: &ConfigMigrationChange) -> ConfigMigrationChangeEvidence {
    match change {
        ConfigMigrationChange::RenameLegacyAutoApproval => {
            ConfigMigrationChangeEvidence::RenameLegacyAutoApproval
        }
        ConfigMigrationChange::UseHomeCredentialStore => {
            ConfigMigrationChangeEvidence::UseHomeCredentialStore
        }
        ConfigMigrationChange::SetSchemaVersion { from, to } => {
            ConfigMigrationChangeEvidence::SetSchemaVersion {
                from: *from,
                to: *to,
            }
        }
        ConfigMigrationChange::UseBuiltinProfile {
            frozen_plugins,
            activated_plugins,
        } => ConfigMigrationChangeEvidence::UseBuiltinProfile {
            frozen_plugins: frozen_plugins.clone(),
            activated_plugins: activated_plugins.clone(),
        },
        ConfigMigrationChange::AddRequiredProfilePlugin {
            plugin,
            required_by,
        } => ConfigMigrationChangeEvidence::AddRequiredProfilePlugin {
            plugin: plugin.clone(),
            required_by: required_by.clone(),
        },
        ConfigMigrationChange::MoveAuthorizationFlowToProviderPlugin {
            flow,
            from_plugin,
            to_plugin,
        } => ConfigMigrationChangeEvidence::MoveAuthorizationFlowToProviderPlugin {
            flow: flow.clone(),
            from_plugin: from_plugin.clone(),
            to_plugin: to_plugin.clone(),
        },
        ConfigMigrationChange::ReplaceRetiredDeepSeekDefault { from, to } => {
            ConfigMigrationChangeEvidence::ReplaceRetiredDeepSeekDefault {
                from: from.clone(),
                to: to.clone(),
            }
        }
    }
}
