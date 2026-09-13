//! TEL03 — the OTEL provider, and where its redaction actually lives.
//!
//! The row's acceptance is `Secret canary never exports`, and the weak reading
//! of it is "call a redactor at the export site". That reading buys almost
//! nothing: it protects the fields somebody remembered to pass through the
//! redactor on the day they wrote it, and it fails silently the first time a
//! field is added. The property worth having is the stronger one — **a secret
//! planted anywhere upstream must not appear in the exported payload, whatever
//! path it travelled** — and that is a statement about what the payload is able
//! to hold, not about which function ran before it was serialized.
//!
//! So the redaction here is structural, in four moves:
//!
//! 1. **Every attribute key on the wire is a `&'static str`.** Not a screened
//!    string, not a validated newtype: a compile-time constant drawn from
//!    [`resource::OtlpAttributeKey`], [`crate::Dimension`] or a literal in
//!    [`payload`]. Keys leak exactly as readily as values, and an OTEL
//!    integration that builds keys from a map is the usual way `api_key` ends
//!    up as a dimension name. There is no `String` on the key side to put one
//!    in.
//! 2. **Every attribute value on the wire is a [`crate::Label`].** TEL02 already
//!    made that the one place text enters an event; TEL03 does not widen it for
//!    the resource. A value therefore survived the identifier character set and
//!    S15's shared `screen_text_for_credentials` before it existed.
//! 3. **The destination is not in the payload.** No endpoint, no headers, no
//!    authentication. [`endpoint::OtlpEndpoint`] and [`endpoint::OtlpAuth`]
//!    describe where a transport sends and how it authenticates, for
//!    diagnostics only, and neither is reachable from
//!    [`payload::OtlpPayload`]. `OtlpAuth` holds no value at all — the same
//!    discipline as `SettingsField::Secret { path, configured, origin }`, which
//!    deliberately has nowhere to put a secret rather than a rule about
//!    redacting one before rendering.
//! 4. **A failed send returns a closed class, never a message.** A transport
//!    error from any real HTTP client renders the request it failed on — URL,
//!    query string and `Authorization` header included — and the reflex line
//!    `error!("export failed: {e}")` publishes all of it. [`OtlpSendFault`] has
//!    no text field, so that line cannot be written (GOTCHAS #154, #156).
//! 5. **The plugin owns one bounded batch worker.** Event admission, ordered
//!    flush, shutdown, cooperative cancellation and the worker join are one
//!    lifecycle. A context disposer never drops a live thread handle and calls
//!    transport shutdown only after the worker has settled.
//!
//! **What this deliberately does not claim.** S15's screen can only ever
//! refuse: a token it does not recognize is not thereby declared safe. So a
//! value the operator explicitly passes as `service.name` is exported, secret
//! or not, and no screen could tell that apart from a legitimate service name.
//! What the four moves above close are the channels a secret reaches *without
//! anyone naming it as the service's identity* — an `OTEL_RESOURCE_ATTRIBUTES`
//! string, an endpoint with a token in it, a header, an error body, a `Debug`.
//! Those are the channels secrets actually arrive through.
//!
//! **Keeping the exporter choice late.** Nothing in this module reaches
//! off-process, and the crate's manifest still names the four data crates that
//! TEL02's `no_egress_by_construction` pins. Egress is a value the caller
//! supplies: [`OtlpTransport`] is implemented in whichever crate ends up owning
//! the wire, and [`plugin::otel_telemetry_plugin`] cannot be called without one.
//! Everything the acceptance is about — the payload shape, the redaction and
//! the canary suite — is settled here and does not move when that choice is
//! made.

mod endpoint;
mod exporter;
mod payload;
mod plugin;
mod resource;

pub use endpoint::{OtlpAuth, OtlpEndpoint};
pub use exporter::{
    OtelExporter, OtelExporterStartFault, OtlpCancellation, OtlpSendFault, OtlpTransport,
};
pub use payload::{
    OtlpPayload, SCOPE_NAME, payload_for, published_attribute_keys, published_static_names,
};
pub use plugin::{PLUGIN_TELEMETRY_OTEL, otel_telemetry_plugin};
pub use resource::{
    ATTRIBUTE_SPEC_MAX_BYTES, AttributeRefusal, AttributeSpecRefusals, MAX_RESOURCE_ATTRIBUTES,
    OtlpAttributeKey, OtlpResource,
};
