//! The telemetry service, and why its default cannot emit.
//!
//! "No outbound telemetry by default" is only worth as much as the thing that
//! makes it true. A `bool` that happens to be `false` is worth very little: it
//! has a setter, a config file, a code path that reads it wrong once. So the
//! default here is not a disabled exporter — it is the **absence** of one.
//!
//! [`Egress::LocalOff`] is a unit variant. When [`TelemetryService::record`]
//! matches it there is no exporter in scope, no vtable, no address: the arm
//! that would send has nothing to send *through*, and no edit to that arm can
//! make it emit without first changing the type. Egress is reached exactly one
//! way — [`TelemetryService::exporting`], which the caller can only reach by
//! constructing an exporter and handing it over.
//!
//! Local counting still happens, because telemetry that is off is not telemetry
//! that is blind. Counts live in a fixed array sized from the closed event-name
//! set, so counting needs no lock, has no poisoned state and cannot fail — the
//! kind of failure mode a recording path should not have. They never leave the
//! process; TEL01's durable `/usage` projection is the thing that survives it,
//! and it deliberately needs no service at all.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{TelemetryEvent, TelemetryEventName};

/// Something that takes a telemetry event somewhere this process is not.
///
/// The OTEL implementation lives in this crate, but its wire transport does
/// not. That is the boundary, not a convention: nothing `heycode-telemetry` can
/// name exposes a socket or a process launcher, so actual egress must still be
/// supplied from somewhere that does.
pub trait TelemetryExporter: Send + Sync {
    /// Hand over one event. Called on the recording path, so an implementation
    /// that blocks blocks the caller.
    fn export(&self, event: &TelemetryEvent);

    /// Settle every event admitted before this call.
    ///
    /// The default is a no-op for exporters that do not batch. The OTEL
    /// provider overrides it with an ordered worker barrier; local-off has no
    /// exporter to call at all.
    fn flush(&self) {}
}

/// Whether a service can emit, for diagnostics.
///
/// Reporting is all this is for. Nothing branches on it to decide whether to
/// send — that decision is [`Egress`] itself, which is why this type cannot be
/// used to turn egress on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EgressKind {
    /// No exporter exists. Events are counted in this process and go nowhere.
    LocalOff,
    /// An exporter was supplied and receives every recorded event.
    Exporting,
}

impl EgressKind {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalOff => "local_off",
            Self::Exporting => "exporting",
        }
    }
}

impl std::fmt::Display for EgressKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where recorded events go. Private on purpose: the only public route to the
/// exporting variant is [`TelemetryService::exporting`], which requires an
/// exporter value.
enum Egress {
    LocalOff,
    Exporting(Arc<dyn TelemetryExporter>),
}

/// Plugin-published telemetry recording point.
///
/// One service key, one type — a provider swap replaces the plugin that
/// publishes it, not a field inside it (GOTCHAS #27: a consumer's `ctx.get::<T>`
/// matches the concrete type exactly, so two service types would mean two
/// consumers).
pub struct TelemetryService {
    egress: Egress,
    counts: [AtomicU64; TelemetryEventName::COUNT],
}

impl TelemetryService {
    /// The default provider: counts locally, holds no exporter.
    #[must_use]
    pub fn local_off() -> Self {
        Self {
            egress: Egress::LocalOff,
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// A service that hands every recorded event to `exporter`.
    ///
    /// Egress is a value the caller supplies, so no configuration reachable
    /// from the default world can produce one of these.
    #[must_use]
    pub fn exporting(exporter: Arc<dyn TelemetryExporter>) -> Self {
        Self {
            egress: Egress::Exporting(exporter),
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Whether this service can emit.
    #[must_use]
    pub const fn egress(&self) -> EgressKind {
        match self.egress {
            Egress::LocalOff => EgressKind::LocalOff,
            Egress::Exporting(_) => EgressKind::Exporting,
        }
    }

    /// Record one event: always counted here, exported only when an exporter
    /// was supplied.
    ///
    /// Infallible by construction. A recording path that can fail is a
    /// recording path callers start ignoring.
    pub fn record(&self, event: &TelemetryEvent) {
        self.counts[event.name().index()].fetch_add(event.count(), Ordering::Relaxed);
        match &self.egress {
            // No exporter is in scope here. There is nothing to send through.
            Egress::LocalOff => {}
            Egress::Exporting(exporter) => exporter.export(event),
        }
    }

    /// Settle every event this service admitted before this call.
    ///
    /// Local-off returns immediately because it owns no exporter. Exporting
    /// providers decide what settlement means through [`TelemetryExporter::flush`].
    pub fn flush(&self) {
        match &self.egress {
            Egress::LocalOff => {}
            Egress::Exporting(exporter) => exporter.flush(),
        }
    }

    /// How many events of `name` this process recorded.
    #[must_use]
    pub fn count(&self, name: TelemetryEventName) -> u64 {
        self.counts[name.index()].load(Ordering::Relaxed)
    }

    /// Every name with its count, in [`TelemetryEventName::ALL`] order.
    #[must_use]
    pub fn counts(&self) -> Vec<(TelemetryEventName, u64)> {
        TelemetryEventName::ALL
            .into_iter()
            .map(|name| (name, self.count(name)))
            .collect()
    }

    /// Total events recorded in this process.
    #[must_use]
    pub fn total(&self) -> u64 {
        TelemetryEventName::ALL
            .into_iter()
            .fold(0, |total, name| total.saturating_add(self.count(name)))
    }
}

impl std::fmt::Debug for TelemetryService {
    /// Renders the egress kind and the counts, never the exporter: a `Debug`
    /// that printed an exporter would publish wherever this build sends data.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelemetryService")
            .field("egress", &self.egress().as_str())
            .field("total", &self.total())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{Dimension, Label};
    use std::sync::Mutex;

    /// Stands in for anything that would leave the process. It records what it
    /// was handed, so "nothing was emitted" is an observation rather than an
    /// assumption.
    #[derive(Default)]
    struct RecordingExporter {
        seen: Mutex<Vec<TelemetryEventName>>,
        flushes: AtomicU64,
    }

    impl RecordingExporter {
        fn seen(&self) -> Vec<TelemetryEventName> {
            self.seen.lock().unwrap().clone()
        }

        fn flushes(&self) -> u64 {
            self.flushes.load(Ordering::Relaxed)
        }
    }

    impl TelemetryExporter for RecordingExporter {
        fn export(&self, event: &TelemetryEvent) {
            self.seen.lock().unwrap().push(event.name());
        }

        fn flush(&self) {
            self.flushes.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn event(name: TelemetryEventName) -> TelemetryEvent {
        TelemetryEvent::new(name, 1)
    }

    #[test]
    fn the_default_service_reports_local_off_egress() {
        assert_eq!(TelemetryService::local_off().egress(), EgressKind::LocalOff);
    }

    #[test]
    fn the_default_service_emits_nothing_to_an_exporter_that_was_not_supplied() {
        let exporter = Arc::new(RecordingExporter::default());
        let service = TelemetryService::local_off();
        for name in TelemetryEventName::ALL {
            service.record(&event(name));
        }
        assert_eq!(
            exporter.seen(),
            Vec::new(),
            "a local-off service has no exporter to reach"
        );
        assert_eq!(service.total(), TelemetryEventName::COUNT as u64);
    }

    #[test]
    fn egress_requires_supplying_an_exporter() {
        let exporter = Arc::new(RecordingExporter::default());
        let service = TelemetryService::exporting(exporter.clone());
        assert_eq!(service.egress(), EgressKind::Exporting);
        service.record(&event(TelemetryEventName::TurnCompleted));
        service.flush();
        assert_eq!(exporter.seen(), vec![TelemetryEventName::TurnCompleted]);
        assert_eq!(exporter.flushes(), 1);
    }

    #[test]
    fn a_local_off_service_still_counts_what_it_recorded() {
        let service = TelemetryService::local_off();
        service.record(&event(TelemetryEventName::ToolInvoked));
        service.record(&event(TelemetryEventName::ToolInvoked));
        service.record(&event(TelemetryEventName::RequestFailed));
        assert_eq!(service.count(TelemetryEventName::ToolInvoked), 2);
        assert_eq!(service.count(TelemetryEventName::RequestFailed), 1);
        assert_eq!(service.count(TelemetryEventName::SessionStarted), 0);
        assert_eq!(
            service.counts(),
            vec![
                (TelemetryEventName::SessionStarted, 0),
                (TelemetryEventName::TurnCompleted, 0),
                (TelemetryEventName::ToolInvoked, 2),
                (TelemetryEventName::RequestFailed, 1),
                (TelemetryEventName::CommandInvoked, 0),
                (TelemetryEventName::ProviderRequest, 0),
                (TelemetryEventName::CompactionCompleted, 0),
                (TelemetryEventName::CacheObserved, 0),
            ]
        );
        assert_eq!(service.total(), 3);
    }

    #[test]
    fn an_aggregate_event_increments_by_its_exact_count() {
        let service = TelemetryService::local_off();
        let event = TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1)
            .with_count(4)
            .unwrap();
        service.record(&event);
        assert_eq!(service.count(TelemetryEventName::ToolInvoked), 4);
        assert_eq!(service.total(), 4);
    }

    #[test]
    fn an_exporting_service_counts_the_same_events_it_exports() {
        let exporter = Arc::new(RecordingExporter::default());
        let service = TelemetryService::exporting(exporter.clone());
        service.record(&event(TelemetryEventName::SessionStarted));
        service.record(&event(TelemetryEventName::SessionStarted));
        assert_eq!(service.count(TelemetryEventName::SessionStarted), 2);
        assert_eq!(exporter.seen().len(), 2);
    }

    #[test]
    fn the_exporter_receives_the_dimensions_the_event_carried() {
        let exporter = Arc::new(CapturingExporter::default());
        let service = TelemetryService::exporting(exporter.clone());
        let recorded = event(TelemetryEventName::TurnCompleted)
            .with_dimension(Dimension::Model, Label::new("glm-5.3-flash").unwrap())
            .unwrap();
        service.record(&recorded);
        assert_eq!(exporter.captured(), vec![recorded]);
    }

    #[derive(Default)]
    struct CapturingExporter {
        seen: Mutex<Vec<TelemetryEvent>>,
    }

    impl CapturingExporter {
        fn captured(&self) -> Vec<TelemetryEvent> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl TelemetryExporter for CapturingExporter {
        fn export(&self, event: &TelemetryEvent) {
            self.seen.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn debug_reports_egress_and_totals_without_naming_the_exporter() {
        struct NamedExporter;
        impl TelemetryExporter for NamedExporter {
            fn export(&self, _event: &TelemetryEvent) {}
        }
        let service = TelemetryService::exporting(Arc::new(NamedExporter));
        service.record(&event(TelemetryEventName::CommandInvoked));
        let rendered = format!("{service:?}");
        assert!(
            rendered.contains(EgressKind::Exporting.as_str()),
            "{rendered}"
        );
        assert!(rendered.contains('1'), "{rendered}");
        assert!(!rendered.contains("NamedExporter"), "{rendered}");
    }

    #[test]
    fn counting_is_shared_across_threads() {
        let service = Arc::new(TelemetryService::local_off());
        let mut handles = Vec::new();
        for _ in 0..4 {
            let service = service.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..250 {
                    service.record(&event(TelemetryEventName::ToolInvoked));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(service.count(TelemetryEventName::ToolInvoked), 1_000);
    }
}
