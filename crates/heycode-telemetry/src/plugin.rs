//! The local-off provider plugin.
//!
//! One constructor, no arguments, and no way to hand it an exporter. That is
//! the whole design: a world composed from this plugin holds a service that
//! cannot emit, and the only way to change that is to compose a *different*
//! plugin publishing the same key — which the duplicate-service check makes a
//! loud, deliberate swap rather than an addition.

use heycode_core::{
    Context, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};

use crate::{SERVICE_TELEMETRY, TelemetryService};

/// Plugin name of the default provider.
pub const PLUGIN_TELEMETRY_LOCAL_OFF: &str = "telemetry-local-off";

/// Publish the local-off telemetry service.
///
/// Takes no exporter, because a constructor that accepted one would put the
/// egress-enabling call inside the crate whose job is to not have one.
#[must_use]
pub fn telemetry_plugin() -> Box<dyn Plugin> {
    struct TelemetryLocalOffPlugin;

    impl Plugin for TelemetryLocalOffPlugin {
        fn name(&self) -> &'static str {
            PLUGIN_TELEMETRY_LOCAL_OFF
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                PLUGIN_TELEMETRY_LOCAL_OFF,
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_TELEMETRY]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            // The bare value, never an `Arc`: providing `Arc<T>` stores
            // `Arc<Arc<T>>` and every `ctx.get::<T>` then returns `None` with
            // no compile error (GOTCHAS #27).
            context.provide(
                SERVICE_TELEMETRY,
                PLUGIN_TELEMETRY_LOCAL_OFF,
                TelemetryService::local_off(),
            )
        }
    }

    Box::new(TelemetryLocalOffPlugin)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{EgressKind, TelemetryEvent, TelemetryEventName};
    use heycode_core::{ContributionKind, CoreError, PluginSource};

    fn composed() -> heycode_core::Context {
        heycode_core::compose(&[telemetry_plugin()]).unwrap()
    }

    #[test]
    fn the_composed_default_world_publishes_a_service_that_cannot_emit() {
        let context = composed();
        let service = context
            .get::<TelemetryService>(SERVICE_TELEMETRY)
            .expect("the bare value must be retrievable under the owner constant");
        assert_eq!(service.egress(), EgressKind::LocalOff);
    }

    #[test]
    fn recording_through_the_composed_default_world_reaches_no_exporter() {
        let context = composed();
        let service = context.get::<TelemetryService>(SERVICE_TELEMETRY).unwrap();
        for name in TelemetryEventName::ALL {
            service.record(&TelemetryEvent::new(name, 1));
        }
        // There is no exporter to observe, so the observable claim is the pair:
        // every event was counted here, and the service reports no egress.
        assert_eq!(service.total(), TelemetryEventName::COUNT as u64);
        assert_eq!(service.egress(), EgressKind::LocalOff);
    }

    #[test]
    fn the_plugin_declares_the_key_it_publishes() {
        let plugin = telemetry_plugin();
        assert_eq!(plugin.provides(), &[SERVICE_TELEMETRY]);
        assert_eq!(plugin.inject(), &[] as &[ServiceKey]);
        assert_eq!(plugin.name(), PLUGIN_TELEMETRY_LOCAL_OFF);
        assert_eq!(plugin.descriptor().id, plugin.name());
        assert_eq!(plugin.descriptor().source, PluginSource::BuiltIn);
        assert_eq!(
            plugin.descriptor().contributions,
            &[PluginContributionKind::Service]
        );
    }

    #[test]
    fn the_service_row_is_attributed_to_this_plugin_in_the_inventory() {
        let context = composed();
        let snapshot = context.plugin_inventory().snapshot().unwrap();
        let row = snapshot
            .contributions
            .iter()
            .find(|row| row.kind == ContributionKind::Service)
            .expect("the service must appear as an exact inventory row");
        assert_eq!(row.plugin, PLUGIN_TELEMETRY_LOCAL_OFF);
        assert_eq!(row.name, SERVICE_TELEMETRY.as_str());
        assert_eq!(
            snapshot.contributions.len(),
            1,
            "the plugin declares exactly one row"
        );
        assert_eq!(
            context.owner_of(SERVICE_TELEMETRY),
            Some("telemetry-local-off")
        );
    }

    /// An outbound provider, as a future crate would ship it: a different
    /// plugin claiming the same key.
    struct RivalTelemetryPlugin;

    impl Plugin for RivalTelemetryPlugin {
        fn name(&self) -> &'static str {
            "telemetry-rival"
        }
        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_TELEMETRY]
        }
        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(
                SERVICE_TELEMETRY,
                "telemetry-rival",
                TelemetryService::local_off(),
            )
        }
    }

    #[test]
    fn a_second_provider_of_the_key_fails_composition_instead_of_layering() {
        let plugins: Vec<Box<dyn Plugin>> =
            vec![telemetry_plugin(), Box::new(RivalTelemetryPlugin)];
        let error = heycode_core::compose(&plugins)
            .err()
            .expect("two providers of one key must not both apply");
        match error {
            CoreError::DuplicateService {
                key,
                existing,
                claimant,
            } => assert_eq!(
                (key.as_str(), existing.as_str(), claimant.as_str()),
                (
                    SERVICE_TELEMETRY.as_str(),
                    PLUGIN_TELEMETRY_LOCAL_OFF,
                    "telemetry-rival"
                ),
                "enabling egress must be a swap, never an addition"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_consumer_that_injects_telemetry_fails_loud_when_no_provider_composed() {
        struct Consumer;
        impl Plugin for Consumer {
            fn name(&self) -> &'static str {
                "telemetry-consumer"
            }
            fn inject(&self) -> &'static [ServiceKey] {
                &[SERVICE_TELEMETRY]
            }
            fn apply(&self, _context: &mut Context) -> CoreResult<()> {
                Ok(())
            }
        }
        let alone: Vec<Box<dyn Plugin>> = vec![Box::new(Consumer)];
        match heycode_core::compose(&alone) {
            Err(CoreError::UnsatisfiedInject { plugin, missing }) => {
                assert_eq!(plugin, "telemetry-consumer");
                assert_eq!(missing, vec![SERVICE_TELEMETRY.as_str().to_owned()]);
            }
            Err(other) => panic!("expected an unsatisfied inject, got {other:?}"),
            Ok(_) => panic!("a consumer must not compose without its provider"),
        }
        let composed: Vec<Box<dyn Plugin>> = vec![telemetry_plugin(), Box::new(Consumer)];
        assert!(heycode_core::compose(&composed).is_ok());
    }

    #[test]
    fn the_dry_graph_reports_the_declared_key_without_applying_anything() {
        let report = heycode_core::inspect_composition(&[heycode_core::ScopedPlugin::new(
            heycode_core::PluginScope::BuiltIn,
            telemetry_plugin(),
        )]);
        assert!(report.healthy, "{}", report.render_human());
        assert_eq!(report.plugins.len(), 1);
        assert_eq!(report.plugins[0].id, PLUGIN_TELEMETRY_LOCAL_OFF);
        assert_eq!(
            report.plugins[0].provides,
            vec![SERVICE_TELEMETRY.as_str().to_owned()]
        );
        assert!(report.plugins[0].injects.is_empty());
    }
}
