//! Effect-owned product plugin for the explicit OTLP/HTTP provider.

use std::sync::Arc;

use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor, ServiceKey,
};
use heycode_telemetry::{OtelExporter, TelemetryExporter, TelemetryService};

use crate::{HttpOtlpTransport, OtlpHttpConfig, settings_definition};

/// Product plugin id for the opt-in OTLP/HTTP JSON provider.
pub const PLUGIN_TELEMETRY_OTLP_HTTP: &str = "telemetry-otlp-http";

/// Build the explicit OTLP/HTTP telemetry provider.
///
/// This plugin is intentionally absent from the built-in default profile. A
/// user/profile layer must disable `telemetry-local-off` and enable this row;
/// composing both fails on the shared service key.
#[must_use]
pub fn telemetry_otlp_http_plugin() -> Box<dyn Plugin> {
    struct TelemetryOtlpHttpPlugin;

    impl Plugin for TelemetryOtlpHttpPlugin {
        fn name(&self) -> &'static str {
            PLUGIN_TELEMETRY_OTLP_HTTP
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "telemetry-otlp",
            )]
        }

        fn inject(&self) -> &'static [ServiceKey] {
            &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_http::SERVICE_HTTP,
            ]
        }

        fn provides(&self) -> &'static [ServiceKey] {
            &[heycode_telemetry::SERVICE_TELEMETRY]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| CoreError::other("settings service type mismatch"))?;
            let credentials = context
                .get::<heycode_credentials::CredentialsService>(
                    heycode_credentials::SERVICE_CREDENTIALS,
                )
                .ok_or_else(|| CoreError::other("credentials service type mismatch"))?;
            let http = context
                .get::<heycode_http::HttpService>(heycode_http::SERVICE_HTTP)
                .ok_or_else(|| CoreError::other("HTTP service type mismatch"))?;
            let snapshot = settings
                .register(
                    context,
                    settings_definition()
                        .map_err(|_| CoreError::other("telemetry OTLP schema is invalid"))?,
                )
                .map_err(|_| CoreError::other("telemetry OTLP settings are invalid"))?;
            let config = OtlpHttpConfig::from_value(snapshot.resolved())
                .map_err(|_| CoreError::other("telemetry OTLP settings are invalid"))?;
            let resource = config.resource().clone();
            let transport = Arc::new(HttpOtlpTransport::new(
                config,
                http.as_ref().clone(),
                credentials.as_ref().clone(),
            ));
            let exporter = Arc::new(
                OtelExporter::new(resource, transport)
                    .map_err(|_| CoreError::other("telemetry OTLP worker is unavailable"))?,
            );
            if let Err(error) = context.provide(
                heycode_telemetry::SERVICE_TELEMETRY,
                self.name(),
                TelemetryService::exporting(Arc::clone(&exporter) as Arc<dyn TelemetryExporter>),
            ) {
                exporter.shutdown();
                return Err(error);
            }
            context.effect(move || exporter.shutdown());
            Ok(())
        }
    }

    Box::new(TelemetryOtlpHttpPlugin)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn product_plugin_declares_the_exact_dependencies_and_service() {
        let plugin = telemetry_otlp_http_plugin();
        assert_eq!(plugin.name(), PLUGIN_TELEMETRY_OTLP_HTTP);
        assert_eq!(
            plugin.inject(),
            &[
                heycode_settings::SERVICE_SETTINGS,
                heycode_credentials::SERVICE_CREDENTIALS,
                heycode_http::SERVICE_HTTP,
            ]
        );
        assert_eq!(plugin.provides(), &[heycode_telemetry::SERVICE_TELEMETRY]);
        assert_eq!(plugin.inventory().len(), 1);
        assert_eq!(
            plugin.inventory()[0].kind,
            heycode_core::ContributionKind::SettingsNamespace
        );
        assert_eq!(plugin.inventory()[0].name, "telemetry-otlp");
    }
}
