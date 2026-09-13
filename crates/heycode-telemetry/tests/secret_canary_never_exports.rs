//! TEL03's acceptance, as an adversarial suite rather than a confirmation.
//!
//! The row says `Secret canary never exports`. The weak way to satisfy that is
//! to plant a secret in the one field the redactor already covers and watch it
//! disappear. This file tries to get a canary out through every channel a
//! secret plausibly arrives on, and it is built so that a field added tomorrow
//! is covered without anyone editing it.
//!
//! Three things make the coverage structural rather than a list:
//!
//! * **The sink is walked, not enumerated.** Every test that asserts absence
//!   serializes the document and walks the whole JSON tree — every object key,
//!   every string, every number — instead of naming fields. A new field on the
//!   wire is visited the moment it exists.
//! * **The strongest claim is a property of every node, not of a field.**
//!   `every_string_an_exported_document_puts_on_the_wire_is_a_bounded_screened_identifier`
//!   asserts that *each* string in the document passes `Label::new` — the
//!   identifier character set and S15's shared credential screen. A field
//!   holding free text fails it whether or not anyone thought to plant into it.
//! * **The sources are iterated over their closed sets.** Planting loops walk
//!   `Dimension::ALL` and `OtlpAttributeKey::ALL`, so a new axis or attribute is
//!   planted into automatically.
//!
//! **Two canaries, because one would prove the wrong thing.**
//! [`RECOGNIZED_CANARY`] is credential material S15 knows, so it tests the
//! screens. [`UNRECOGNIZED_CANARY`] is a real secret S15 has no recognizer for
//! — and it is what tests *structure*, because every channel that still
//! contains it does so by shape rather than by recognition. The one place it
//! does get out is pinned by its own test, so the boundary of this guarantee is
//! written down instead of assumed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use heycode_telemetry::{
    AttributeRefusal, Dimension, Label, OtelExporter, OtlpAttributeKey, OtlpAuth, OtlpCancellation,
    OtlpEndpoint, OtlpPayload, OtlpResource, OtlpSendFault, OtlpTransport, SERVICE_TELEMETRY,
    TelemetryEvent, TelemetryEventName, TelemetryExporter, TelemetryFault, TelemetryService,
    otel_telemetry_plugin, payload_for, published_attribute_keys, published_static_names,
};
use serde_json::Value;

/// Credential material S15's shared screen recognizes.
const RECOGNIZED_CANARY: &str = "sk-ant-api03-CANARY000000000000000";

/// A real secret with no published issuer prefix, so no recognizer can see it.
/// It is also a perfectly well-formed identifier, which is the point: anything
/// that keeps it out does so structurally.
const UNRECOGNIZED_CANARY: &str = "hunter2-correct-horse-battery-staple";

/// A benign value used to prove the walks below are not vacuous.
const SENTINEL: &str = "heycode-sentinel-marker";

/// Both canaries, so every absence assertion covers recognition *and* shape.
const CANARIES: [&str; 2] = [RECOGNIZED_CANARY, UNRECOGNIZED_CANARY];

// ---------------------------------------------------------------- the walk --

/// Every string this document puts on the wire: object keys, string values, and
/// numbers and booleans rendered as they serialize.
fn wire_strings(value: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect(value, &mut found);
    found
}

fn collect(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                into.push(key.clone());
                collect(child, into);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, into);
            }
        }
        Value::String(text) => into.push(text.clone()),
        Value::Number(number) => into.push(number.to_string()),
        Value::Bool(flag) => into.push(flag.to_string()),
        Value::Null => {}
    }
}

/// Whether `needle` appears anywhere in the serialized document.
///
/// Checks the walk *and* the raw serialization. The walk is what the structural
/// claims are built on; the raw check is the belt that catches a shape the walk
/// might not descend into.
fn document_mentions(payload: &OtlpPayload, needle: &str) -> bool {
    let document = serde_json::to_value(payload).unwrap();
    let raw = serde_json::to_string(payload).unwrap();
    raw.contains(needle)
        || wire_strings(&document)
            .iter()
            .any(|text| text.contains(needle))
}

fn assert_no_canary_anywhere(payload: &OtlpPayload, planted: &str, site: &str) {
    for canary in CANARIES {
        assert!(
            !document_mentions(payload, canary),
            "{canary} reached the wire after being planted in {site}"
        );
    }
    assert!(
        !document_mentions(payload, planted),
        "the value planted in {site} reached the wire"
    );
}

// ------------------------------------------------------------- the fixtures --

fn resource() -> OtlpResource {
    OtlpResource::new(Label::new("heycode").unwrap())
}

fn event() -> TelemetryEvent {
    TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1_700_000_000_000)
        .with_dimension(Dimension::Provider, Label::new("openrouter").unwrap())
        .unwrap()
        .with_duration(Duration::from_millis(42))
}

/// A document with every closed vocabulary populated, so a walk over it visits
/// every field this crate can write.
fn fully_populated(value: &str) -> (OtlpPayload, usize) {
    let label = Label::new(value).unwrap();
    let mut resource = OtlpResource::new(label.clone());
    for key in OtlpAttributeKey::ALL {
        if resource.attribute(key).is_none() {
            resource = resource.with_attribute(key, label.clone()).unwrap();
        }
    }
    let mut events = Vec::new();
    for name in TelemetryEventName::ALL {
        let mut event = TelemetryEvent::new(name, 1_700_000_000_000);
        for axis in Dimension::ALL {
            event = event.with_dimension(axis, label.clone()).unwrap();
        }
        events.push(event.with_duration(Duration::from_millis(7)));
    }
    // Derived, not counted by hand: the resource attributes that actually took
    // the value (`telemetry.sdk.*` are seeded with this build's own constants),
    // plus one per dimension on both the count and the duration data point.
    let planted = resource
        .attributes()
        .iter()
        .filter(|(_, held)| held.as_str() == value)
        .count()
        + Dimension::ALL.len() * TelemetryEventName::COUNT * 2;
    (payload_for(&resource, &events), planted)
}

struct StubTransport {
    destination: OtlpEndpoint,
    authentication: OtlpAuth,
    verdict: Option<OtlpSendFault>,
    sent: std::sync::Mutex<Vec<OtlpPayload>>,
    shutdowns: AtomicU64,
}

impl StubTransport {
    fn new(authentication: OtlpAuth) -> Arc<Self> {
        Arc::new(Self {
            destination: OtlpEndpoint::parse("https://collector.example.com:4318/v1/metrics")
                .unwrap(),
            authentication,
            verdict: None,
            sent: std::sync::Mutex::new(Vec::new()),
            shutdowns: AtomicU64::new(0),
        })
    }

    fn failing(fault: OtlpSendFault) -> Arc<Self> {
        Arc::new(Self {
            destination: OtlpEndpoint::parse("https://collector.example.com/v1/metrics").unwrap(),
            authentication: OtlpAuth::NoneConfigured,
            verdict: Some(fault),
            sent: std::sync::Mutex::new(Vec::new()),
            shutdowns: AtomicU64::new(0),
        })
    }

    fn captured(&self) -> Vec<OtlpPayload> {
        self.sent.lock().unwrap().clone()
    }

    fn shutdowns(&self) -> u64 {
        self.shutdowns.load(Ordering::Relaxed)
    }
}

impl OtlpTransport for StubTransport {
    fn destination(&self) -> &OtlpEndpoint {
        &self.destination
    }
    fn authentication(&self) -> &OtlpAuth {
        &self.authentication
    }
    fn send(
        &self,
        payload: &OtlpPayload,
        _cancellation: &OtlpCancellation,
    ) -> Result<(), OtlpSendFault> {
        match self.verdict {
            Some(fault) => Err(fault),
            None => {
                self.sent.lock().unwrap().push(payload.clone());
                Ok(())
            }
        }
    }
    fn shutdown(&self, _cancellation: &OtlpCancellation) {
        self.shutdowns.fetch_add(1, Ordering::Relaxed);
    }
}

// ----------------------------------------------- the structural claims first --

#[test]
fn the_walk_reaches_every_value_a_document_carries_so_its_absences_mean_something() {
    // A walk that visits nothing proves nothing. This pins that the sentinel,
    // planted at every admitting site, comes back from every one of them.
    let (payload, planted) = fully_populated(SENTINEL);
    let document = serde_json::to_value(&payload).unwrap();
    let strings = wire_strings(&document);
    assert!(
        strings.len() > 40,
        "the walk found only {} strings; it is not reaching the document",
        strings.len()
    );
    let sentinels = strings.iter().filter(|text| *text == SENTINEL).count();
    assert_eq!(
        sentinels, planted,
        "the sentinel must come back from every resource attribute and every \
         dimension on both the count and the duration data point"
    );

    for key in published_attribute_keys() {
        assert!(strings.contains(&key.to_owned()), "{key} was never visited");
    }
    for name in published_static_names() {
        assert!(
            strings.contains(&name.to_owned()),
            "{name} was never visited"
        );
    }
}

#[test]
fn every_string_an_exported_document_puts_on_the_wire_is_a_bounded_screened_identifier() {
    // The strongest claim in this file, and the one that survives new fields:
    // it says nothing about *which* fields exist, only that every string in the
    // document passes the same two screens a `Label` passes — the identifier
    // character set, which refuses prose, and S15's shared credential screen.
    // A field added later that carries free text fails this without anyone
    // remembering to plant into it.
    for value in [SENTINEL, "heycode", "z-ai/glm-5.3-flash"] {
        let document = serde_json::to_value(&fully_populated(value).0).unwrap();
        for text in wire_strings(&document) {
            assert!(
                Label::new(text.clone()).is_ok(),
                "{text} is on the wire and is not a bounded screened identifier"
            );
        }
    }
}

#[test]
fn a_document_never_carries_the_destination_it_is_being_sent_to() {
    let transport = StubTransport::new(OtlpAuth::Configured {
        header: Label::new("authorization").unwrap(),
        credential: Label::new("otlp/collector").unwrap(),
    });
    let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
    exporter.export(&event());
    exporter.flush();
    let sent = transport.captured();
    assert_eq!(sent.len(), 1);
    for fragment in [
        "collector.example.com",
        "4318",
        "https",
        "authorization",
        "otlp/collector",
    ] {
        assert!(
            !document_mentions(&sent[0], fragment),
            "{fragment} describes where the document goes; it must not be in it"
        );
    }
}

// -------------------------------------------------- planting into the event --

#[test]
fn a_canary_cannot_become_a_dimension_value_on_any_axis() {
    for axis in Dimension::ALL {
        for canary in CANARIES {
            let attempt = Label::new(canary).and_then(|label| {
                TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1).with_dimension(axis, label)
            });
            match attempt {
                Err(fault) => assert_eq!(
                    fault,
                    TelemetryFault::CredentialMaterial,
                    "{axis} refused {canary} for the wrong reason"
                ),
                Ok(event) => {
                    // Only the unrecognized canary can get this far, and only
                    // because it is a well-formed identifier no screen can see
                    // through. Recorded rather than hidden.
                    assert_eq!(canary, UNRECOGNIZED_CANARY);
                    assert!(document_mentions(
                        &payload_for(&resource(), &[event]),
                        canary
                    ));
                }
            }
        }
    }
}

#[test]
fn a_canary_cannot_become_an_attribute_key_because_keys_are_compile_time_constants() {
    // There is no `String` on the key side of the wire, so this is asserted
    // against a fully-populated document rather than by trying to plant: every
    // key that appears is one of the published constants.
    let document = serde_json::to_value(&fully_populated(SENTINEL).0).unwrap();
    let published = published_attribute_keys();
    let mut keys = Vec::new();
    collect_attribute_keys(&document, &mut keys);
    assert!(!keys.is_empty());
    for key in keys {
        assert!(
            published.contains(&key.as_str()),
            "{key} is an attribute key that is not a published constant"
        );
        for canary in CANARIES {
            assert!(!key.contains(canary), "{canary} became an attribute key");
        }
    }
}

fn collect_attribute_keys(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == "key"
                    && let Some(key) = child.as_str()
                {
                    into.push(key.to_owned());
                }
                collect_attribute_keys(child, into);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_attribute_keys(item, into);
            }
        }
        _ => {}
    }
}

// ----------------------------------------------- planting into the resource --

#[test]
fn a_canary_cannot_become_a_resource_attribute_value_on_any_key() {
    // Iterated over the closed key set, so an attribute added later is planted
    // into without this test being edited.
    let mut settable = 0;
    for key in OtlpAttributeKey::ALL {
        assert!(
            resource()
                .with_attribute(key, Label::new("x").unwrap())
                .is_err()
                || Label::new(RECOGNIZED_CANARY).is_err(),
            "{key} must either be already set or refuse a recognized credential"
        );
        if !key.operator_settable() {
            continue;
        }
        settable += 1;
        let (extended, refusals) =
            resource().extend_from_spec(&format!("{}={RECOGNIZED_CANARY}", key.as_str()));
        assert_eq!(
            refusals.count(AttributeRefusal::CredentialMaterial)
                + refusals.count(AttributeRefusal::DuplicateKey),
            1,
            "{key} admitted a recognized credential"
        );
        assert_no_canary_anywhere(
            &payload_for(&extended, &[event()]),
            RECOGNIZED_CANARY,
            key.as_str(),
        );
    }
    assert!(
        settable >= 5,
        "only {settable} keys were actually planted into"
    );
}

#[test]
fn a_canary_cannot_become_a_resource_attribute_key_from_an_environment_spec() {
    for canary in CANARIES {
        let (resource, refusals) = resource().extend_from_spec(&format!("{canary}=1"));
        assert_eq!(
            refusals.count(AttributeRefusal::UnknownKey),
            1,
            "{canary} must be refused as a key outside the closed set"
        );
        assert_no_canary_anywhere(
            &payload_for(&resource, &[event()]),
            canary,
            "an environment spec attribute key",
        );
    }
}

#[test]
fn an_unrecognized_canary_in_an_environment_spec_is_refused_by_shape_not_by_recognition() {
    // The interesting half. S15 has no recognizer for this value, so nothing
    // screens it out — it is kept off the wire because the key it arrives under
    // is not in the closed set, and because the keys that *are* describe a
    // deployment rather than accepting arbitrary text.
    let hostile = format!(
        "api_key={UNRECOGNIZED_CANARY},\
         process.command_line=heycode --api-key {UNRECOGNIZED_CANARY},\
         host.name={UNRECOGNIZED_CANARY},\
         OTEL_EXPORTER_OTLP_HEADERS=Authorization={UNRECOGNIZED_CANARY}"
    );
    let (resource, refusals) = resource().extend_from_spec(&hostile);
    assert_eq!(refusals.count(AttributeRefusal::UnknownKey), 4);
    assert_eq!(refusals.total(), 4);
    assert_no_canary_anywhere(
        &payload_for(&resource, &[event()]),
        UNRECOGNIZED_CANARY,
        "a hostile OTEL_RESOURCE_ATTRIBUTES spec",
    );
    let rendered = format!("{refusals:?} {:?}", refusals.counts());
    assert!(
        !rendered.contains(UNRECOGNIZED_CANARY),
        "a refusal report must count, never quote: {rendered}"
    );
}

#[test]
fn a_refusal_report_says_how_many_and_never_which() {
    let (_, refusals) = resource().extend_from_spec(&format!(
        "service.namespace={RECOGNIZED_CANARY},{UNRECOGNIZED_CANARY}=x,os.type=two words,nopair"
    ));
    assert_eq!(refusals.count(AttributeRefusal::CredentialMaterial), 1);
    assert_eq!(refusals.count(AttributeRefusal::UnknownKey), 1);
    assert_eq!(refusals.count(AttributeRefusal::MalformedValue), 1);
    assert_eq!(refusals.count(AttributeRefusal::MalformedPair), 1);
    let rendered = format!("{refusals:?}");
    for canary in CANARIES {
        assert!(!rendered.contains(canary), "{rendered}");
    }
    assert!(!rendered.contains("two words"), "{rendered}");
}

// ----------------------------------------------- planting into the endpoint --

/// One endpoint-injection site: where the canary goes, how the address is
/// built around it, and the fault the shape check must raise.
type EndpointSite = (&'static str, fn(&str) -> String, TelemetryFault);

#[test]
fn a_canary_cannot_enter_through_any_position_in_an_endpoint_address() {
    let sites: [EndpointSite; 6] = [
        (
            "userinfo user",
            |canary| format!("https://{canary}@collector.example.com/v1/metrics"),
            TelemetryFault::EndpointCarriesCredential,
        ),
        (
            "userinfo password",
            |canary| format!("https://user:{canary}@collector.example.com/v1/metrics"),
            TelemetryFault::EndpointCarriesCredential,
        ),
        (
            "query string",
            |canary| format!("https://collector.example.com/v1/metrics?api-key={canary}"),
            TelemetryFault::MalformedEndpoint,
        ),
        (
            "fragment",
            |canary| format!("https://collector.example.com/v1/metrics#{canary}"),
            TelemetryFault::MalformedEndpoint,
        ),
        (
            "host label",
            |canary| format!("https://{canary}.example.com/v1/metrics"),
            TelemetryFault::MalformedEndpoint,
        ),
        (
            "path segment",
            |canary| format!("https://collector.example.com/v1/{canary}"),
            TelemetryFault::MalformedEndpoint,
        ),
    ];

    for (site, build, structural_fault) in sites {
        // A recognized credential is reported as one wherever it sits, because
        // the credential screen runs before the structural checks.
        assert_eq!(
            OtlpEndpoint::parse(&build(RECOGNIZED_CANARY)),
            Err(TelemetryFault::EndpointCarriesCredential),
            "{site} admitted a recognized credential"
        );
        // The unrecognized one is refused only where the *shape* refuses it.
        // The two sites where it is not — a host label and a path segment — are
        // the operator's own address, and neither reaches a payload.
        let unrecognized = OtlpEndpoint::parse(&build(UNRECOGNIZED_CANARY));
        match unrecognized {
            Err(fault) => assert_eq!(fault, structural_fault, "{site}"),
            Ok(endpoint) => {
                assert!(
                    matches!(site, "host label" | "path segment"),
                    "{site} admitted an unrecognized canary"
                );
                let transport = Arc::new(StubTransport {
                    destination: endpoint,
                    authentication: OtlpAuth::NoneConfigured,
                    verdict: None,
                    sent: std::sync::Mutex::new(Vec::new()),
                    shutdowns: AtomicU64::new(0),
                });
                let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
                exporter.export(&event());
                exporter.flush();
                assert!(
                    !document_mentions(&transport.captured()[0], UNRECOGNIZED_CANARY),
                    "{site} is part of the address, and the address is not in the document"
                );
            }
        }
    }
}

#[test]
fn an_endpoint_that_a_settings_file_deserializes_is_screened_like_a_parsed_one() {
    for canary in CANARIES {
        let json = format!("\"https://user:{canary}@collector.example.com/v1/metrics\"");
        assert!(
            serde_json::from_str::<OtlpEndpoint>(&json).is_err(),
            "deserialization is a constructor and must run the same screens \
             (GOTCHAS #161)"
        );
    }
    let label = format!("\"{RECOGNIZED_CANARY}\"");
    assert!(serde_json::from_str::<Label>(&label).is_err());
}

// --------------------------------------------------- planting into the auth --

#[test]
fn a_canary_cannot_be_held_as_a_credential_because_there_is_no_field_for_one() {
    for canary in CANARIES {
        // The only two text fields an auth descriptor has are a header name and
        // a credential id, and both are `Label`s.
        let header = Label::new(canary);
        let recognized = canary == RECOGNIZED_CANARY;
        assert_eq!(
            header.is_err(),
            recognized,
            "{canary} as a header name must be refused exactly when recognized"
        );
        assert!(
            Label::new(format!("Bearer {canary}")).is_err(),
            "a whole header line is not an identifier"
        );
    }

    let auth = OtlpAuth::Configured {
        header: Label::new("authorization").unwrap(),
        credential: Label::new("otlp/collector").unwrap(),
    };
    let rendered = format!("{auth:?} {auth} {}", serde_json::to_string(&auth).unwrap());
    for canary in CANARIES {
        assert!(!rendered.contains(canary), "{rendered}");
    }
    assert!(
        !rendered.contains("Bearer") && !rendered.contains("token"),
        "there is nowhere in this type for a credential to be: {rendered}"
    );
    assert!(auth.configured(), "it may say a credential exists");
}

// --------------------------------------- planting into a transport's failure --

#[test]
fn a_transport_failure_carries_a_class_and_has_no_field_to_carry_a_message_in() {
    // The reflex line is `error!("otlp export failed: {e}")`, and on a real
    // client `e` renders the request — the endpoint, its query string and the
    // Authorization header. `OtlpSendFault` has no payload, so the transport
    // has nothing to hand back that could carry the canary.
    assert_eq!(
        std::mem::size_of::<OtlpSendFault>(),
        1,
        "the fault must remain a bare closed tag with no raw error payload"
    );
    for fault in OtlpSendFault::ALL {
        let transport = StubTransport::failing(fault);
        let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
        exporter.export(&event());
        exporter.flush();
        assert_eq!(exporter.refused(fault), 1);
        let rendered = format!("{exporter:?} {fault:?} {fault} {:?}", exporter.refusals());
        for canary in CANARIES {
            assert!(!rendered.contains(canary), "{rendered}");
        }
    }
}

// --------------------------------------------------- planting into `Debug` --

#[test]
fn no_debug_or_display_in_this_crate_renders_a_canary_it_was_shown() {
    let (resource, _) = resource().extend_from_spec(&format!(
        "service.namespace={RECOGNIZED_CANARY},{UNRECOGNIZED_CANARY}=x"
    ));
    let transport = StubTransport::new(OtlpAuth::Configured {
        header: Label::new("x-api-token").unwrap(),
        credential: Label::new("otlp/collector").unwrap(),
    });
    let exporter = Arc::new(OtelExporter::new(resource.clone(), transport.clone()).unwrap());
    let service = TelemetryService::exporting(exporter.clone());
    service.record(&event());
    service.flush();

    let payload = exporter.payload_for(&[event()]);
    let mut rendered = vec![
        format!("{exporter:?}"),
        format!("{service:?}"),
        format!("{resource:?}"),
        format!("{payload:?}"),
        format!("{:?}", exporter.destination()),
        format!("{}", exporter.destination()),
        format!("{:?}", exporter.authentication()),
        format!("{}", exporter.authentication()),
        serde_json::to_string(&resource).unwrap(),
        serde_json::to_string(&payload).unwrap(),
        serde_json::to_string(exporter.destination()).unwrap(),
        serde_json::to_string(exporter.authentication()).unwrap(),
    ];
    for fault in [
        TelemetryFault::CredentialMaterial,
        TelemetryFault::MalformedLabel,
        TelemetryFault::DuplicateDimension,
        TelemetryFault::DuplicateAttribute,
        TelemetryFault::MalformedEndpoint,
        TelemetryFault::EndpointCarriesCredential,
    ] {
        rendered.push(format!("{fault:?} {fault}"));
    }
    for refusal in AttributeRefusal::ALL {
        rendered.push(format!("{refusal:?} {refusal}"));
    }

    assert!(rendered.len() >= 20, "{}", rendered.len());
    for text in &rendered {
        assert!(!text.is_empty());
        for canary in CANARIES {
            assert!(!text.contains(canary), "{canary} appears in: {text}");
        }
    }
}

// ------------------------------------------------------- the composed world --

#[test]
fn context_shutdown_flushes_recorded_events_as_one_batch() {
    let transport = StubTransport::new(OtlpAuth::NoneConfigured);
    let mut context =
        heycode_core::compose(&[otel_telemetry_plugin(resource(), transport.clone())])
            .expect("the otel provider must compose");
    let service = context.get::<TelemetryService>(SERVICE_TELEMETRY).unwrap();
    for name in [
        TelemetryEventName::SessionStarted,
        TelemetryEventName::ToolInvoked,
        TelemetryEventName::TurnCompleted,
    ] {
        service.record(&TelemetryEvent::new(name, 1_700_000_000_000));
    }
    drop(service);

    context.shutdown();

    let captured = transport.captured();
    assert_eq!(
        captured.len(),
        1,
        "the provider lifecycle must batch and flush before it shuts down"
    );
    assert_eq!(captured[0].data_point_count(), 3);
}

#[test]
fn a_canary_planted_before_composition_never_reaches_the_mounted_transport() {
    let (resource, refusals) = resource().extend_from_spec(&format!(
        "api_key={UNRECOGNIZED_CANARY},service.namespace={RECOGNIZED_CANARY},os.type=darwin"
    ));
    assert_eq!(refusals.total(), 2);
    let transport = StubTransport::new(OtlpAuth::NoneConfigured);
    let mut context = heycode_core::compose(&[otel_telemetry_plugin(resource, transport.clone())])
        .expect("the otel provider must compose");
    let service = context.get::<TelemetryService>(SERVICE_TELEMETRY).unwrap();
    for name in TelemetryEventName::ALL {
        service.record(&TelemetryEvent::new(name, 1_700_000_000_000));
    }
    service.flush();
    let captured = transport.captured();
    assert_eq!(captured.len(), 1, "all events must share one flushed batch");
    for payload in &captured {
        for canary in CANARIES {
            assert!(!document_mentions(payload, canary));
        }
        assert!(
            document_mentions(payload, "darwin"),
            "an admitted attribute must still arrive"
        );
    }
    drop(service);
    context.shutdown();
    assert_eq!(
        transport.shutdowns(),
        1,
        "the disposer must run exactly once"
    );
}

#[test]
fn the_otel_provider_still_takes_its_egress_from_the_caller() {
    // TEL02's guarantee is that this crate cannot build an exporter of its own.
    // TEL03 does not weaken it: `otel_telemetry_plugin` cannot be called without
    // an `OtlpTransport`, and there is no implementation of that trait here —
    // this test has to write one to compose at all.
    let transport = StubTransport::new(OtlpAuth::NoneConfigured);
    let context = heycode_core::compose(&[otel_telemetry_plugin(resource(), transport)]).unwrap();
    assert_eq!(
        context.owner_of(SERVICE_TELEMETRY),
        Some(heycode_telemetry::PLUGIN_TELEMETRY_OTEL)
    );
    assert_eq!(
        heycode_core::compose(&[heycode_telemetry::telemetry_plugin()])
            .unwrap()
            .get::<TelemetryService>(SERVICE_TELEMETRY)
            .unwrap()
            .egress(),
        heycode_telemetry::EgressKind::LocalOff,
        "the default provider is unchanged"
    );
}

// ------------------------------------------------ the boundary of the claim --

#[test]
fn a_secret_the_operator_names_as_the_service_identity_does_export_and_that_is_the_limit() {
    // Written down rather than left implicit. S15's screen can only refuse: a
    // token it does not recognize is not thereby declared safe. So an operator
    // who passes an unrecognized secret as their service name exports it, and
    // nothing here can tell that apart from a legitimate service name.
    //
    // What the rest of this file proves is the property that *is* achievable:
    // every channel where a secret arrives without a human naming it as the
    // deployment's identity — an environment spec, an endpoint, a header, an
    // error, a `Debug` — is closed.
    let resource = OtlpResource::new(Label::new(UNRECOGNIZED_CANARY).unwrap());
    let payload = payload_for(&resource, &[event()]);
    assert!(
        document_mentions(&payload, UNRECOGNIZED_CANARY),
        "this is the documented limit; if it changes, the limit changed"
    );
    assert_eq!(
        Label::new(RECOGNIZED_CANARY),
        Err(TelemetryFault::CredentialMaterial),
        "a *recognized* credential is refused even on this path"
    );
}
