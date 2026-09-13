//! The OTLP document, and why nothing in it can be free text.
//!
//! This is an `ExportMetricsServiceRequest` as OTLP/JSON spells it: one
//! `resourceMetrics` entry, one `scopeMetrics` entry, and a delta `Sum` per
//! recorded event with an optional `Gauge` carrying the measured duration. It
//! is built here rather than by an SDK because the shape is small, stable and
//! published, and because building it here is what keeps the wire crate a
//! choice we can still make: whichever transport wins either sends this
//! document as its body or maps it field-for-field, and neither changes the
//! redaction or the tests that pin it.
//!
//! Two properties do the redaction work, and both are properties of the types
//! rather than of a function anyone has to remember to call.
//!
//! **Keys are `&'static str`.** `Attribute::key` is not a `String`, so the key
//! side of every attribute in this document is a compile-time constant drawn
//! from [`OtlpAttributeKey`], [`Dimension`] or a literal below. An OTEL
//! integration that builds attributes from a map is the ordinary way `api_key`
//! becomes an attribute *name*; there is no `String` here to hold one.
//!
//! **Values are [`Label`]s.** The only `String` in the document is
//! `AnyValue::string_value`, and every one of them is written from a `Label` or
//! a decimal integer. A `Label` survived the identifier character set and S15's
//! shared credential screen before it existed, so a value in this document
//! passed both.
//!
//! There is no `Deserialize` anywhere in this module. Nothing reads OTLP here,
//! so there is no second constructor for the invariants above to be bypassed
//! through (GOTCHAS #161).

use serde::Serialize;

use crate::otel::resource::{OtlpAttributeKey, OtlpResource};
use crate::{Dimension, Label, TelemetryEvent, TelemetryEventName};

/// Instrumentation scope name every document produced here declares.
pub const SCOPE_NAME: &str = "heycode-telemetry";

/// Metric carrying one count per recorded event.
const EVENT_METRIC: &str = "heycode.telemetry.event";
/// Metric carrying the duration the emitting layer measured, when it did.
const DURATION_METRIC: &str = "heycode.telemetry.duration";
/// Attribute naming which event a data point counts.
const EVENT_NAME_ATTRIBUTE: &str = "heycode.event.name";
/// `AGGREGATION_TEMPORALITY_DELTA`, per the OTLP metrics protocol.
const DELTA_TEMPORALITY: u8 = 1;

/// One OTLP metrics export document.
///
/// Serialize-only. Build it with [`payload_for`]; there is no other route in,
/// and no route back out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpPayload {
    resource_metrics: Vec<ResourceMetrics>,
}

impl OtlpPayload {
    /// How many data points this document carries across every metric.
    ///
    /// Published so a transport can size a batch without deserializing what it
    /// is about to send.
    #[must_use]
    pub fn data_point_count(&self) -> usize {
        self.resource_metrics
            .iter()
            .flat_map(|resource| &resource.scope_metrics)
            .flat_map(|scope| &scope.metrics)
            .map(|metric| match &metric.data {
                MetricData::Sum(sum) => sum.data_points.len(),
                MetricData::Gauge(gauge) => gauge.data_points.len(),
            })
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceMetrics {
    resource: WireResource,
    scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireResource {
    attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeMetrics {
    scope: Scope,
    metrics: Vec<Metric>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scope {
    name: &'static str,
    version: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metric {
    name: &'static str,
    unit: &'static str,
    #[serde(flatten)]
    data: MetricData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum MetricData {
    Sum(Sum),
    Gauge(Gauge),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Sum {
    aggregation_temporality: u8,
    is_monotonic: bool,
    data_points: Vec<NumberDataPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Gauge {
    data_points: Vec<NumberDataPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct NumberDataPoint {
    time_unix_nano: String,
    as_int: String,
    attributes: Vec<Attribute>,
}

/// One OTLP attribute.
///
/// `key` is `&'static str` on purpose, and it is the single most load-bearing
/// type decision in this module: it makes "no caller text is ever an attribute
/// key" a fact the compiler enforces rather than a rule a reviewer checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Attribute {
    key: &'static str,
    value: AnyValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnyValue {
    string_value: String,
}

impl Attribute {
    fn new(key: &'static str, value: &Label) -> Self {
        Self {
            key,
            value: AnyValue {
                string_value: value.as_str().to_owned(),
            },
        }
    }
}

/// Build the OTLP document for `events` under `resource`.
///
/// One `Sum` data point per event, plus a `Gauge` data point for each event the
/// emitting layer measured a duration on. An empty slice still produces a
/// well-formed document with no metrics, because a transport asked to send
/// nothing should send an empty batch rather than decide for itself.
#[must_use]
pub fn payload_for(resource: &OtlpResource, events: &[TelemetryEvent]) -> OtlpPayload {
    let mut counts = Vec::with_capacity(events.len());
    let mut durations = Vec::new();
    for event in events {
        let attributes = attributes_for(event);
        let time = nanos(event.at_unix_ms());
        counts.push(NumberDataPoint {
            time_unix_nano: time.clone(),
            as_int: event.count().to_string(),
            attributes: attributes.clone(),
        });
        if let Some(duration_ms) = event.duration_ms() {
            durations.push(NumberDataPoint {
                time_unix_nano: time,
                as_int: duration_ms.to_string(),
                attributes,
            });
        }
    }

    let mut metrics = Vec::with_capacity(2);
    if !counts.is_empty() {
        metrics.push(Metric {
            name: EVENT_METRIC,
            unit: "1",
            data: MetricData::Sum(Sum {
                aggregation_temporality: DELTA_TEMPORALITY,
                is_monotonic: true,
                data_points: counts,
            }),
        });
    }
    if !durations.is_empty() {
        metrics.push(Metric {
            name: DURATION_METRIC,
            unit: "ms",
            data: MetricData::Gauge(Gauge {
                data_points: durations,
            }),
        });
    }

    OtlpPayload {
        resource_metrics: vec![ResourceMetrics {
            resource: WireResource {
                attributes: resource
                    .attributes()
                    .iter()
                    .map(|(key, value)| Attribute::new(key.as_str(), value))
                    .collect(),
            },
            scope_metrics: vec![ScopeMetrics {
                scope: Scope {
                    name: SCOPE_NAME,
                    version: env!("CARGO_PKG_VERSION"),
                },
                metrics,
            }],
        }],
    }
}

/// Which axes a data point carries: the event name, then each dimension.
///
/// Both key sides are `&'static str` — [`TelemetryEventName`] and
/// [`Dimension`] publish theirs as constants — so a new dimension added
/// upstream arrives here as an attribute with a compile-time key and a screened
/// value, with nothing to update.
fn attributes_for(event: &TelemetryEvent) -> Vec<Attribute> {
    let mut attributes = Vec::with_capacity(1 + event.dimensions().len());
    attributes.push(Attribute {
        key: EVENT_NAME_ATTRIBUTE,
        value: AnyValue {
            string_value: event.name().as_str().to_owned(),
        },
    });
    for (dimension, label) in event.dimensions() {
        attributes.push(Attribute::new(dimension.as_str(), label));
    }
    attributes
}

/// Milliseconds to the nanoseconds OTLP wants, saturating rather than wrapping.
fn nanos(at_unix_ms: u64) -> String {
    at_unix_ms.saturating_mul(1_000_000).to_string()
}

/// Every attribute key this module can ever write, for a caller auditing the
/// key side of an export.
///
/// Assembled from the closed vocabularies rather than listed, so a new
/// dimension or resource attribute appears here without an edit — and a test
/// that walks a document against this list stays honest as the document grows.
#[must_use]
pub fn published_attribute_keys() -> Vec<&'static str> {
    let mut keys = vec![EVENT_NAME_ATTRIBUTE];
    keys.extend(OtlpAttributeKey::ALL.iter().map(|key| key.as_str()));
    keys.extend(Dimension::ALL.iter().map(|axis| axis.as_str()));
    keys
}

/// Every metric and scope name this module can ever write.
#[must_use]
pub fn published_static_names() -> Vec<&'static str> {
    let mut names = vec![SCOPE_NAME, EVENT_METRIC, DURATION_METRIC, "1", "ms"];
    names.extend(TelemetryEventName::ALL.iter().map(|name| name.as_str()));
    names
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Duration;

    fn resource() -> OtlpResource {
        OtlpResource::new(Label::new("heycode").unwrap())
    }

    fn event() -> TelemetryEvent {
        TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1_700_000_000_000)
            .with_dimension(Dimension::Provider, Label::new("openrouter").unwrap())
            .unwrap()
            .with_dimension(Dimension::Model, Label::new("glm-5.3-flash").unwrap())
            .unwrap()
    }

    fn json(payload: &OtlpPayload) -> Value {
        serde_json::to_value(payload).unwrap()
    }

    #[test]
    fn a_document_carries_the_resource_scope_and_one_delta_sum_per_event() {
        let payload = payload_for(&resource(), &[event()]);
        let document = json(&payload);
        let metrics = &document["resourceMetrics"][0]["scopeMetrics"][0]["metrics"];
        assert_eq!(metrics[0]["name"], EVENT_METRIC);
        assert_eq!(
            metrics[0]["sum"]["aggregationTemporality"],
            DELTA_TEMPORALITY
        );
        assert_eq!(metrics[0]["sum"]["isMonotonic"], true);
        assert_eq!(metrics[0]["sum"]["dataPoints"][0]["asInt"], "1");
        assert_eq!(
            metrics[0]["sum"]["dataPoints"][0]["timeUnixNano"],
            "1700000000000000000"
        );
        assert_eq!(
            document["resourceMetrics"][0]["scopeMetrics"][0]["scope"]["name"],
            SCOPE_NAME
        );
        assert_eq!(payload.data_point_count(), 1);
    }

    #[test]
    fn an_aggregate_event_keeps_its_count_in_the_delta_sum() {
        let aggregate = event().with_count(4).unwrap();
        let document = json(&payload_for(&resource(), &[aggregate]));
        assert_eq!(
            document["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]["dataPoints"][0]
                ["asInt"],
            "4"
        );
    }

    #[test]
    fn a_data_point_names_the_event_and_every_dimension_it_carried() {
        let payload = payload_for(&resource(), &[event()]);
        let document = json(&payload);
        let attributes = &document["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]["dataPoints"]
            [0]["attributes"];
        let pairs: Vec<(String, String)> = attributes
            .as_array()
            .unwrap()
            .iter()
            .map(|attribute| {
                (
                    attribute["key"].as_str().unwrap().to_owned(),
                    attribute["value"]["stringValue"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        assert_eq!(
            pairs,
            vec![
                (EVENT_NAME_ATTRIBUTE.to_owned(), "turn_completed".to_owned()),
                ("provider".to_owned(), "openrouter".to_owned()),
                ("model".to_owned(), "glm-5.3-flash".to_owned()),
            ]
        );
    }

    #[test]
    fn a_measured_duration_becomes_a_second_metric_and_an_unmeasured_one_becomes_nothing() {
        let measured = event().with_duration(Duration::from_millis(1_250));
        let payload = payload_for(&resource(), &[measured]);
        let document = json(&payload);
        let metrics = document["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
            .as_array()
            .unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[1]["name"], DURATION_METRIC);
        assert_eq!(metrics[1]["unit"], "ms");
        assert_eq!(metrics[1]["gauge"]["dataPoints"][0]["asInt"], "1250");
        assert_eq!(payload.data_point_count(), 2);

        let unmeasured = payload_for(&resource(), &[event()]);
        assert_eq!(
            json(&unmeasured)["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_empty_batch_is_a_well_formed_document_with_no_metrics() {
        let payload = payload_for(&resource(), &[]);
        let document = json(&payload);
        assert_eq!(payload.data_point_count(), 0);
        assert_eq!(
            document["resourceMetrics"][0]["scopeMetrics"][0]["metrics"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(
            !document["resourceMetrics"][0]["resource"]["attributes"]
                .as_array()
                .unwrap()
                .is_empty(),
            "the resource still says who produced the empty batch"
        );
    }

    #[test]
    fn a_batch_carries_every_event_it_was_given() {
        let events: Vec<TelemetryEvent> = TelemetryEventName::ALL
            .into_iter()
            .map(|name| TelemetryEvent::new(name, 1))
            .collect();
        let payload = payload_for(&resource(), &events);
        assert_eq!(payload.data_point_count(), TelemetryEventName::COUNT);
    }

    #[test]
    fn a_timestamp_past_the_nanosecond_range_saturates_rather_than_wrapping() {
        // u64::MAX milliseconds times a million overflows; wrapping would place
        // the point at an arbitrary instant instead of the far future.
        assert_eq!(nanos(u64::MAX), u64::MAX.to_string());
        assert_eq!(nanos(1_700_000_000_000), "1700000000000000000");
        assert_eq!(nanos(0), "0");
    }

    #[test]
    fn the_published_key_list_is_exactly_what_a_document_can_write() {
        let mut event = TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1);
        for axis in Dimension::ALL {
            event = event
                .with_dimension(axis, Label::new(axis.as_str()).unwrap())
                .unwrap();
        }
        let mut resource = resource();
        for key in OtlpAttributeKey::ALL {
            if resource.attribute(key).is_none() {
                resource = resource
                    .with_attribute(key, Label::new("x").unwrap())
                    .unwrap();
            }
        }
        let document = json(&payload_for(
            &resource,
            &[event.with_duration(Duration::ZERO)],
        ));
        let published = published_attribute_keys();
        let mut seen = Vec::new();
        collect_attribute_keys(&document, &mut seen);
        assert!(!seen.is_empty(), "the walk must actually find keys");
        for key in &seen {
            assert!(
                published.contains(&key.as_str()),
                "{key} is written but not published"
            );
        }
        for key in published {
            assert!(
                seen.contains(&key.to_owned()),
                "{key} is published but a fully-populated document never wrote it"
            );
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

    #[test]
    fn every_static_name_a_document_writes_is_published_for_an_auditor() {
        let names = published_static_names();
        for expected in [SCOPE_NAME, EVENT_METRIC, DURATION_METRIC] {
            assert!(names.contains(&expected), "{expected}");
        }
        for name in TelemetryEventName::ALL {
            assert!(names.contains(&name.as_str()), "{name}");
        }
    }
}
