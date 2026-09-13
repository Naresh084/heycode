//! Explicit opt-in OTLP/HTTP transport provider for heycode telemetry.
//!
//! `heycode-telemetry` deliberately cannot name HTTP or credentials, which makes
//! its local-off default unable to emit. This separate crate owns those
//! dependencies and publishes the same `telemetry` service only when plugin
//! [`PLUGIN_TELEMETRY_OTLP_HTTP`] is explicitly selected.
//!
//! Protocol facts follow the official OpenTelemetry sources:
//! <https://opentelemetry.io/docs/specs/otlp/> and
//! <https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/protocol/exporter.md>.

mod config;
mod plugin;
mod transport;

pub use config::{
    DEFAULT_METRICS_ENDPOINT, OtlpAuthScheme, OtlpHttpAuthConfig, OtlpHttpConfig,
    OtlpHttpConfigFault, settings_definition, settings_namespace,
};
pub use plugin::{PLUGIN_TELEMETRY_OTLP_HTTP, telemetry_otlp_http_plugin};
pub use transport::HttpOtlpTransport;
