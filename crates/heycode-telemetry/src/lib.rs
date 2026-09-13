//! TEL02 — the telemetry service, and a default provider that cannot emit.
//!
//! The acceptance for this row is a privacy guarantee: **no outbound telemetry
//! by default**. There are two ways to make that true, and only one of them is
//! worth having.
//!
//! The weak way is a switch. A `[telemetry] enabled = false` that ships off,
//! and a send path guarded by it. Every part of that is fine until one of them
//! is not: a default that a config layer overrides, a check that a refactor
//! moves below the send, an environment variable someone adds for debugging.
//! The guarantee lasts exactly as long as everyone remembers it exists.
//!
//! The way taken here is to remove the ability. [`TelemetryService::local_off`]
//! holds no exporter — not a disabled one, none — so the branch that would send
//! has nothing to send through, and there is no setter that could give it one.
//! Egress arrives only by [`TelemetryService::exporting`], which the caller
//! reaches by *constructing an exporter and handing it over*. There is no such
//! exporter here, and one cannot be written here: the four names this crate can
//! reach are `heycode-core`, `heycode-settings`, `serde` and `serde_json`, and none of
//! them exposes a socket or a process launcher. Be precise about what that does
//! and does not say — `tokio` is in the transitive tree under `heycode-settings`
//! (resolved without the feature that would give it a socket, but with process
//! spawning). A transitive crate is not *nameable* from here, so reaching it
//! takes a manifest edit, and the manifest is exactly what the integration test
//! `no_egress_by_construction` pins, alongside a scan of these sources. Widening
//! either turns a named test red instead of quietly shipping.
//!
//! **What an event may contain** follows Q08 (`heycode-live-artifact`) and takes
//! it one step further. Q08's asymmetry is metadata always, content only when
//! opted in; telemetry has no opt-in, because data that leaves the machine has
//! no bounded audience — so instead of gating content, [`TelemetryEvent`]
//! cannot hold any. Names and axes are closed enums, and [`Label`] is the one
//! place text enters: bounded, restricted to the characters a registry
//! identifier uses, and screened by the shared
//! `heycode_settings::screen_text_for_credentials` rather than a second copy of
//! what a credential looks like (GOTCHAS #154).
//!
//! **What TEL03 added, and what it did not.** [`otel`] ships a second provider
//! that *does* emit, and none of the above weakens: the exporter it publishes
//! converts events to an OTLP document and hands it to an [`otel::OtlpTransport`]
//! the caller constructed elsewhere, so this crate still names four things and
//! still cannot reach a wire. Mounting it is a swap of the same service key,
//! which composition rejects as a duplicate rather than accepting as a layer.
//! Its bounded batch worker is owned by that plugin effect: flush is an ordered
//! barrier, normal shutdown flushes and joins, and a blocked transport receives
//! cooperative cancellation before the worker is joined.
//!
//! **What this service is not.** It is not a dependency of anything that works
//! without it. TEL01's `/usage` projection reads the durable log and needs no
//! service, no counter and no live observation; the in-process counts here are
//! a live convenience that a restart forgets, and the projection is the thing
//! that survives. Nothing that already works may start requiring telemetry to
//! be composed.

pub(crate) mod event;
pub mod otel;
mod plugin;
mod service;

pub use event::{
    Dimension, LABEL_MAX_BYTES, Label, MAX_DIMENSIONS, TELEMETRY_SCHEMA_VERSION, TelemetryEvent,
    TelemetryEventName, TelemetryFault,
};
pub use otel::{
    ATTRIBUTE_SPEC_MAX_BYTES, AttributeRefusal, AttributeSpecRefusals, MAX_RESOURCE_ATTRIBUTES,
    OtelExporter, OtelExporterStartFault, OtlpAttributeKey, OtlpAuth, OtlpCancellation,
    OtlpEndpoint, OtlpPayload, OtlpResource, OtlpSendFault, OtlpTransport, PLUGIN_TELEMETRY_OTEL,
    SCOPE_NAME, otel_telemetry_plugin, payload_for, published_attribute_keys,
    published_static_names,
};
pub use plugin::{PLUGIN_TELEMETRY_LOCAL_OFF, telemetry_plugin};
pub use service::{EgressKind, TelemetryExporter, TelemetryService};

/// Plugin-published telemetry recording point.
///
/// Owned here; consumers import this constant instead of repeating the literal
/// so a rename is a compile error rather than a runtime `None` (GOTCHAS #27).
pub const SERVICE_TELEMETRY: heycode_core::ServiceKey = heycode_core::ServiceKey::new("telemetry");
