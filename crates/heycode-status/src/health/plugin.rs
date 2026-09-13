//! The `/health` command and the service a support bundle reads.
//!
//! This is a separate plugin from `status` on purpose. Recording a durable
//! file is a capability a composed world may legitimately not want, and the
//! house pattern for an optional edge is a plugin that is simply not mounted —
//! not a service lookup that quietly succeeds or quietly does nothing
//! (`status-web` is the same shape). Mounting it publishes
//! [`SERVICE_HEALTH_HISTORY`], which is what makes the retained history
//! *reachable*: a support bundle asks the context for the store rather than
//! knowing where the file lives.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use heycode_agent::{Command, CommandDescriptor, CommandSource, UiEvent};
use heycode_core::{Context, CoreResult, Plugin};
use heycode_doctor::DoctorRegistry;
use tokio_util::sync::CancellationToken;

use super::{HealthEntry, HealthHistoryStore};
use crate::{core_error, descriptor, get, require_no_args};

/// Durable bounded health history published for `/health` and support bundles.
pub const SERVICE_HEALTH_HISTORY: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("health-history");

/// Mount the retained health history and the `/health` command over `path`.
///
/// The path is supplied by the composition root, exactly as the session log's
/// is: a plugin that chose its own location would be a second opinion about
/// where the product's state lives.
#[must_use]
pub fn health_history_plugin(path: PathBuf) -> Box<dyn Plugin> {
    struct HealthHistoryPlugin(PathBuf);

    impl Plugin for HealthHistoryPlugin {
        fn name(&self) -> &'static str {
            "health-history"
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
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::Command,
                "health",
            )]
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_HEALTH_HISTORY]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_agent::SERVICE_COMMANDS,
                heycode_doctor::SERVICE_DOCTOR,
            ]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands =
                get::<heycode_agent::CommandRegistry>(context, heycode_agent::SERVICE_COMMANDS)?;
            let doctor = get::<DoctorRegistry>(context, heycode_doctor::SERVICE_DOCTOR)?;
            context.provide(
                SERVICE_HEALTH_HISTORY,
                self.name(),
                HealthHistoryStore::new(self.0.clone()),
            )?;
            let store = get::<HealthHistoryStore>(context, SERVICE_HEALTH_HISTORY)?;
            let lifecycle = CancellationToken::new();
            let shutdown = lifecycle.clone();
            context.effect(move || shutdown.cancel());
            let source = CommandSource::from_plugin(self.name()).map_err(core_error)?;
            let command = Arc::new(HealthCommand {
                descriptor: descriptor(
                    "health",
                    "Record this health check and show the retained history",
                    source,
                )
                .map_err(core_error)?,
                doctor,
                store,
                lifecycle,
            });
            commands
                .register_effect(context, command)
                .map_err(core_error)
        }
    }

    Box::new(HealthHistoryPlugin(path))
}

struct HealthCommand {
    descriptor: CommandDescriptor,
    doctor: Arc<DoctorRegistry>,
    store: Arc<HealthHistoryStore>,
    lifecycle: CancellationToken,
}

#[async_trait]
impl Command for HealthCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        require_no_args(args, "health")?;
        let started = Instant::now();
        let run = self.doctor.run_timed(self.lifecycle.child_token()).await?;
        let entry = HealthEntry::from_run(
            env!("CARGO_PKG_VERSION"),
            &run,
            unix_millis(SystemTime::now()),
            started.elapsed(),
        );
        let history = self.store.record(entry)?;
        agent.ui().emit(UiEvent::Info {
            text: history.render_human(),
        });
        Ok(())
    }
}

/// Milliseconds since the Unix epoch, or zero for a clock set before it.
///
/// A timestamp that cannot be read is recorded as zero rather than refusing
/// the entry: the ordering that matters is the file's, and a run that happened
/// is worth keeping even when the host cannot say when.
fn unix_millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .as_ref()
        .map_or(0, |elapsed: &Duration| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
