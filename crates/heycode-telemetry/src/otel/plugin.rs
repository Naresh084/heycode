//! The OTEL provider plugin.
//!
//! TEL02's plugin publishes a service that cannot emit and takes no arguments,
//! so no configuration can turn egress on. This one publishes a service that
//! does emit, and it takes a transport — which is what makes enabling egress a
//! deliberate act at the composition root rather than a flag somewhere. The two
//! plugins claim the same service key, so mounting this one is a **swap** the
//! duplicate-service check makes loud, never an addition (GOTCHAS #27).
//!
//! The registration is an effect with an exact disposer: `apply` registers one
//! disposer that closes admission, flushes the owned batch worker, cooperatively
//! cancels a blocked send, joins it and shuts down exactly the transport it
//! mounted. The exporter is captured by handle rather than looked up. Nothing
//! global is touched, so the disposer removes precisely what the registration
//! added — and if a later plugin's apply fails, `rollback_activation` disposes
//! it along with the service row.
//!
//! `inventory()` stays empty: `Context::provide` already records the exact
//! `Service` row and attributes it to this plugin, and declaring it again is a
//! duplicate that fails at the Declaration stage (GOTCHAS #162).

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};

use crate::otel::exporter::{OtelExporter, OtlpTransport};
use crate::otel::resource::OtlpResource;
use crate::{SERVICE_TELEMETRY, TelemetryExporter, TelemetryService};

/// Plugin name of the OTEL provider.
pub const PLUGIN_TELEMETRY_OTEL: &str = "telemetry-otel";

/// Publish a telemetry service that exports through `transport`.
///
/// Mount this **instead of** `telemetry_plugin`, never alongside it: both claim
/// [`SERVICE_TELEMETRY`], and composing both fails naming them, which is the
/// intended shape of turning egress on.
#[must_use]
pub fn otel_telemetry_plugin(
    resource: OtlpResource,
    transport: Arc<dyn OtlpTransport>,
) -> Box<dyn Plugin> {
    struct TelemetryOtelPlugin {
        resource: OtlpResource,
        transport: Arc<dyn OtlpTransport>,
    }

    impl Plugin for TelemetryOtelPlugin {
        fn name(&self) -> &'static str {
            PLUGIN_TELEMETRY_OTEL
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                PLUGIN_TELEMETRY_OTEL,
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[SERVICE_TELEMETRY]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let exporter = Arc::new(
                OtelExporter::new(self.resource.clone(), Arc::clone(&self.transport))
                    .map_err(|_| CoreError::other("telemetry export worker unavailable"))?,
            );
            // The bare value, never an `Arc`: providing `Arc<T>` stores
            // `Arc<Arc<T>>` and every `ctx.get::<T>` then returns `None` with
            // no compile error (GOTCHAS #27).
            if let Err(error) = context.provide(
                SERVICE_TELEMETRY,
                PLUGIN_TELEMETRY_OTEL,
                TelemetryService::exporting(Arc::clone(&exporter) as Arc<dyn TelemetryExporter>),
            ) {
                exporter.shutdown();
                return Err(error);
            }
            // Context keeps provided service values until it drops, so the
            // explicit effect is the lifecycle owner: it closes admission,
            // flushes or cancels, releases the transport and joins the worker.
            context.effect(move || exporter.shutdown());
            Ok(())
        }
    }

    Box::new(TelemetryOtelPlugin {
        resource,
        transport,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::otel::endpoint::{OtlpAuth, OtlpEndpoint};
    use crate::otel::exporter::{OtlpCancellation, OtlpSendFault};
    use crate::otel::payload::OtlpPayload;
    use crate::{EgressKind, Label, TelemetryEvent, TelemetryEventName, telemetry_plugin};
    use heycode_core::{ContributionKind, CoreError, PluginSource};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct CountingTransport {
        destination: OtlpEndpoint,
        authentication: OtlpAuth,
        sent: AtomicU64,
        shutdowns: AtomicU64,
    }

    impl CountingTransport {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                destination: OtlpEndpoint::parse("https://collector.example.com/v1/metrics")
                    .unwrap(),
                authentication: OtlpAuth::NoneConfigured,
                sent: AtomicU64::new(0),
                shutdowns: AtomicU64::new(0),
            })
        }
        fn sent(&self) -> u64 {
            self.sent.load(Ordering::Relaxed)
        }
        fn shutdowns(&self) -> u64 {
            self.shutdowns.load(Ordering::Relaxed)
        }
    }

    impl OtlpTransport for CountingTransport {
        fn destination(&self) -> &OtlpEndpoint {
            &self.destination
        }
        fn authentication(&self) -> &OtlpAuth {
            &self.authentication
        }
        fn send(
            &self,
            _payload: &OtlpPayload,
            _cancellation: &OtlpCancellation,
        ) -> Result<(), OtlpSendFault> {
            self.sent.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn shutdown(&self, _cancellation: &OtlpCancellation) {
            self.shutdowns.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn resource() -> OtlpResource {
        OtlpResource::new(Label::new("heycode").unwrap())
    }

    fn plugin(transport: Arc<dyn OtlpTransport>) -> Box<dyn Plugin> {
        otel_telemetry_plugin(resource(), transport)
    }

    #[test]
    fn the_composed_otel_world_publishes_a_service_that_exports() {
        let transport = CountingTransport::new();
        let context = heycode_core::compose(&[plugin(transport.clone())]).unwrap();
        let service = context
            .get::<TelemetryService>(SERVICE_TELEMETRY)
            .expect("the bare value must be retrievable under the owner constant");
        assert_eq!(service.egress(), EgressKind::Exporting);
        for name in TelemetryEventName::ALL {
            service.record(&TelemetryEvent::new(name, 1));
        }
        service.flush();
        assert_eq!(transport.sent(), 1, "the events must share one batch");
        assert_eq!(service.total(), TelemetryEventName::COUNT as u64);
    }

    #[test]
    fn the_plugin_declares_the_key_it_publishes_and_no_static_rows_of_its_own() {
        let plugin = plugin(CountingTransport::new());
        assert_eq!(plugin.provides(), &[SERVICE_TELEMETRY]);
        assert_eq!(plugin.inject(), &[] as &[ServiceKey]);
        assert_eq!(plugin.name(), PLUGIN_TELEMETRY_OTEL);
        assert_eq!(plugin.descriptor().id, plugin.name());
        assert_eq!(plugin.descriptor().source, PluginSource::BuiltIn);
        assert_eq!(
            plugin.descriptor().contributions,
            &[PluginContributionKind::Service]
        );
        assert!(
            plugin.inventory().is_empty(),
            "provide already records the Service row (GOTCHAS #162)"
        );
    }

    #[test]
    fn the_service_row_is_attributed_to_this_plugin_in_the_inventory() {
        let context = heycode_core::compose(&[plugin(CountingTransport::new())]).unwrap();
        let snapshot = context.plugin_inventory().snapshot().unwrap();
        assert_eq!(snapshot.contributions.len(), 1);
        let row = &snapshot.contributions[0];
        assert_eq!(row.kind, ContributionKind::Service);
        assert_eq!(row.plugin, PLUGIN_TELEMETRY_OTEL);
        assert_eq!(row.name, SERVICE_TELEMETRY.as_str());
        assert_eq!(
            context.owner_of(SERVICE_TELEMETRY),
            Some(PLUGIN_TELEMETRY_OTEL)
        );
    }

    #[test]
    fn mounting_egress_beside_the_local_off_default_fails_instead_of_layering() {
        let plugins: Vec<Box<dyn Plugin>> =
            vec![telemetry_plugin(), plugin(CountingTransport::new())];
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
                    crate::PLUGIN_TELEMETRY_LOCAL_OFF,
                    PLUGIN_TELEMETRY_OTEL
                ),
                "enabling egress must be a swap, never an addition"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn shutdown_disposes_exactly_the_transport_this_plugin_mounted() {
        let mounted = CountingTransport::new();
        let unmounted = CountingTransport::new();
        let mut context = heycode_core::compose(&[plugin(mounted.clone())]).unwrap();
        assert_eq!(mounted.shutdowns(), 0, "nothing disposes before shutdown");
        context.shutdown();
        assert_eq!(mounted.shutdowns(), 1);
        assert_eq!(
            unmounted.shutdowns(),
            0,
            "the disposer must reach the transport it was handed and nothing else"
        );
    }

    #[test]
    fn a_service_handle_held_past_context_shutdown_cannot_export_again() {
        let transport = CountingTransport::new();
        let mut context = heycode_core::compose(&[plugin(transport.clone())]).unwrap();
        let service = context.get::<TelemetryService>(SERVICE_TELEMETRY).unwrap();
        service.record(&TelemetryEvent::new(TelemetryEventName::SessionStarted, 1));
        context.shutdown();
        assert_eq!(transport.sent(), 1, "shutdown must flush admitted work");

        service.record(&TelemetryEvent::new(TelemetryEventName::SessionStarted, 2));
        service.flush();
        assert_eq!(
            transport.sent(),
            1,
            "a held service must not outlive the provider lifecycle"
        );
        assert_eq!(service.total(), 2, "local accounting remains honest");
    }

    #[test]
    fn a_later_plugin_failing_to_apply_disposes_this_one_and_leaves_no_service() {
        struct Failing;
        impl Plugin for Failing {
            fn name(&self) -> &'static str {
                "telemetry-otel-failing-neighbour"
            }
            fn apply(&self, _context: &mut Context) -> CoreResult<()> {
                Err(CoreError::ContributionOutsideApply)
            }
        }
        let transport = CountingTransport::new();
        let plugins: Vec<Box<dyn Plugin>> = vec![plugin(transport.clone()), Box::new(Failing)];
        assert!(heycode_core::compose(&plugins).is_err());
        assert_eq!(
            transport.shutdowns(),
            1,
            "a failed composition must unwind the effect this plugin registered"
        );
    }

    #[test]
    fn the_dry_graph_reports_the_declared_key_without_sending_anything() {
        let transport = CountingTransport::new();
        let report = heycode_core::inspect_composition(&[heycode_core::ScopedPlugin::new(
            heycode_core::PluginScope::BuiltIn,
            plugin(transport.clone()),
        )]);
        assert!(report.healthy, "{}", report.render_human());
        assert_eq!(report.plugins.len(), 1);
        assert_eq!(report.plugins[0].id, PLUGIN_TELEMETRY_OTEL);
        assert_eq!(
            report.plugins[0].provides,
            vec![SERVICE_TELEMETRY.as_str().to_owned()]
        );
        assert_eq!(transport.sent(), 0, "inspection must not apply anything");
        assert_eq!(transport.shutdowns(), 0);
    }

    #[test]
    fn a_consumer_of_telemetry_is_satisfied_by_the_otel_provider_too() {
        struct Consumer;
        impl Plugin for Consumer {
            fn name(&self) -> &'static str {
                "telemetry-otel-consumer"
            }
            fn inject(&self) -> &'static [ServiceKey] {
                &[SERVICE_TELEMETRY]
            }
            fn apply(&self, _context: &mut Context) -> CoreResult<()> {
                Ok(())
            }
        }
        let plugins: Vec<Box<dyn Plugin>> =
            vec![plugin(CountingTransport::new()), Box::new(Consumer)];
        assert!(
            heycode_core::compose(&plugins).is_ok(),
            "a provider swap must not change what consumers can inject"
        );
    }
}
