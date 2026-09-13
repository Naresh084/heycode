//! The exporter, and the boundary the wire crate implements.
//!
//! [`OtelExporter`] admits [`TelemetryEvent`] values into a bounded queue,
//! converts them into OTLP batches on one owned worker, and hands each document
//! to an [`OtlpTransport`]. It does not implement a wire. That split is what keeps
//! TEL02's guarantee intact while TEL03 exists: this crate still names only the
//! four data crates `no_egress_by_construction` pins, and egress is still a
//! value the caller constructs elsewhere and hands over — now one level further
//! out, since [`OtelExporter::new`] cannot be called without a transport.
//!
//! Two shapes of the trait are load-bearing.
//!
//! **`send` returns a closed class, not an error.** Every HTTP client's error
//! type renders the request that failed — the full URL with its query string,
//! and often the request headers. `error!("otlp export failed: {e}")` is the
//! line people actually write, and on a collector configured with
//! `?api-key=…` it publishes the key to whatever reads that log. An
//! [`OtlpSendFault`] has no text field, so the transport has nothing to hand
//! back that could carry one, and the line cannot be written. This is the same
//! rule Q08 keeps for `FailureClass` and MCP04 for its token-endpoint faults:
//! a denial carries its class and nothing else.
//!
//! **The transport states its own destination.** [`OtlpTransport::destination`]
//! and [`OtlpTransport::authentication`] are asked of the thing that actually
//! sends, rather than stored beside it. A diagnostic that reads a separately
//! configured endpoint is reporting a value that *happens* to agree with where
//! bytes go; one transport-owned contract keeps the values together. A hostile
//! implementation can still lie about its own wire, which is outside what this
//! data-only crate can observe (GOTCHAS #167).
//!
//! **The plugin owns settlement.** An explicit flush is an ordered barrier in
//! the same command stream as event admission. Shutdown closes that stream,
//! gives the worker a bounded grace period to flush, cancels a blocked send
//! through [`OtlpCancellation`] and joins the worker. No send task or thread is
//! detached.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::otel::endpoint::{OtlpAuth, OtlpEndpoint};
use crate::otel::payload::{OtlpPayload, payload_for};
use crate::otel::resource::OtlpResource;
use crate::{TelemetryEvent, TelemetryExporter};

const DEFAULT_BATCH_EVENTS: usize = 64;
const DEFAULT_QUEUE_EVENTS: usize = 1_024;
const DEFAULT_SCHEDULED_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
const PHASE_ACTIVE: u8 = 0;
const PHASE_SHUTTING_DOWN: u8 = 1;
const PHASE_CLOSED: u8 = 2;

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerConfig {
    max_batch_events: usize,
    max_queue_events: usize,
    scheduled_delay: Duration,
    shutdown_grace: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_batch_events: DEFAULT_BATCH_EVENTS,
            max_queue_events: DEFAULT_QUEUE_EVENTS,
            scheduled_delay: DEFAULT_SCHEDULED_DELAY,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

/// Failure to start the exporter lifecycle.
///
/// The operating-system error is deliberately discarded: raw thread/runtime
/// diagnostics can contain host paths and implementation detail, while the
/// caller can act only on whether the worker exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OtelExporterStartFault {
    /// The owned batch worker could not be started.
    WorkerUnavailable,
}

impl std::fmt::Display for OtelExporterStartFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the telemetry export worker is unavailable")
    }
}

impl std::error::Error for OtelExporterStartFault {}

#[derive(Default)]
struct CancellationState {
    cancelled: AtomicBool,
    gate: Mutex<()>,
    changed: Condvar,
}

/// Cooperative cancellation supplied to every transport send.
///
/// A transport must observe this value while blocked and return promptly after
/// cancellation. The exporter then joins the worker before shutdown returns;
/// dropping a future or a thread handle is never treated as settlement.
#[derive(Clone, Default)]
pub struct OtlpCancellation {
    state: Arc<CancellationState>,
}

impl OtlpCancellation {
    /// Whether the exporter has cancelled this lifecycle.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Block until cancellation is requested.
    ///
    /// Intended for deterministic transports whose send operation waits on an
    /// external collector. A production transport may instead poll
    /// [`Self::is_cancelled`] in its own cancellation-aware client.
    pub fn wait_cancelled(&self) {
        let mut guard = lock_unpoisoned(&self.state.gate);
        while !self.is_cancelled() {
            guard = match self.state.changed.wait(guard) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    fn cancel(&self) {
        let _guard = lock_unpoisoned(&self.state.gate);
        if !self.state.cancelled.swap(true, Ordering::AcqRel) {
            self.state.changed.notify_all();
        }
    }
}

impl std::fmt::Debug for OtlpCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OtlpCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Why one export did not reach the collector, from a closed set.
///
/// No message field, and that absence is the contract: a transport error from a
/// real client renders the request it failed on, credentials included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum OtlpSendFault {
    /// The collector could not be reached at all.
    Unreachable,
    /// The collector answered and rejected the request.
    Refused,
    /// The collector asked for less traffic.
    Throttled,
    /// The collector could not read what was sent.
    Malformed,
    /// The exporter cancelled an in-flight send during lifecycle teardown.
    Cancelled,
}

impl OtlpSendFault {
    /// Every fault, in stable order. The counter array is sized from this.
    pub const ALL: [Self; 5] = [
        Self::Unreachable,
        Self::Refused,
        Self::Throttled,
        Self::Malformed,
        Self::Cancelled,
    ];

    /// How many faults exist.
    pub const COUNT: usize = Self::ALL.len();

    /// Position of this fault in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Unreachable => 0,
            Self::Refused => 1,
            Self::Throttled => 2,
            Self::Malformed => 3,
            Self::Cancelled => 4,
        }
    }

    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Refused => "refused",
            Self::Throttled => "throttled",
            Self::Malformed => "malformed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for OtlpSendFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::error::Error for OtlpSendFault {}

/// Something that puts an OTLP document on a wire.
///
/// Implemented outside this crate, in whichever crate ends up owning the HTTP
/// client. Everything the TEL03 acceptance is about is settled before this
/// trait is called, so the choice of client changes no test here.
pub trait OtlpTransport: Send + Sync {
    /// Where this transport sends. Diagnostics render it; no payload contains it.
    fn destination(&self) -> &OtlpEndpoint;

    /// How this transport authenticates, as a descriptor holding no value.
    ///
    /// The credential itself is resolved by the transport at send time, from
    /// the credentials service, under the id [`OtlpAuth::credential`] names.
    fn authentication(&self) -> &OtlpAuth;

    /// Send one document.
    ///
    /// # Errors
    /// A closed [`OtlpSendFault`]. An implementation must classify whatever its
    /// client reported and drop the client's own message: that message renders
    /// the request, and the request carries the credential. It must also
    /// observe `cancellation` while blocked and return
    /// [`OtlpSendFault::Cancelled`] promptly when it fires, so the lifecycle
    /// owner can join rather than detach the send.
    fn send(
        &self,
        payload: &OtlpPayload,
        cancellation: &OtlpCancellation,
    ) -> Result<(), OtlpSendFault>;

    /// Release whatever this transport holds. Called from the owned worker
    /// during plugin disposal.
    ///
    /// Default no-op so a transport with nothing to flush implements nothing.
    fn shutdown(&self, _cancellation: &OtlpCancellation) {}
}

/// A [`TelemetryExporter`] that speaks OTLP.
///
/// Holds the resource every document is attributed to and the transport that
/// sends them, and counts what it sent and what it lost, by class. The counts
/// are the honest half of "telemetry is configured": an exporter that has been
/// failing to reach its collector for an hour should be able to say so without
/// anyone reading a log.
pub struct OtelExporter {
    resource: OtlpResource,
    transport: Arc<dyn OtlpTransport>,
    sender: Sender<WorkerCommand>,
    admission: Mutex<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
    cancellation: OtlpCancellation,
    counters: Arc<ExporterCounters>,
    phase: AtomicU8,
    max_queue_events: usize,
    shutdown_grace: Duration,
}

struct ExporterCounters {
    sent: AtomicU64,
    refusals: [AtomicU64; OtlpSendFault::COUNT],
    queued: AtomicUsize,
    dropped: AtomicU64,
}

enum WorkerCommand {
    Event(TelemetryEvent),
    Flush(SyncSender<()>),
    Shutdown(SyncSender<()>),
    Wake,
}

impl OtelExporter {
    /// An exporter attributing every document to `resource` and sending through
    /// `transport`.
    ///
    /// The transport is a value the caller constructs. There is no constructor
    /// here that builds one, because there is nothing in this crate's
    /// dependency set that could.
    ///
    /// # Errors
    /// [`OtelExporterStartFault::WorkerUnavailable`] when the owned batching
    /// worker cannot be started. The underlying operating-system text is never
    /// retained or rendered.
    pub fn new(
        resource: OtlpResource,
        transport: Arc<dyn OtlpTransport>,
    ) -> Result<Self, OtelExporterStartFault> {
        Self::with_config(resource, transport, WorkerConfig::default())
    }

    pub(crate) fn with_config(
        resource: OtlpResource,
        transport: Arc<dyn OtlpTransport>,
        config: WorkerConfig,
    ) -> Result<Self, OtelExporterStartFault> {
        let (sender, receiver) = mpsc::channel();
        let cancellation = OtlpCancellation::default();
        let counters = Arc::new(ExporterCounters {
            sent: AtomicU64::new(0),
            refusals: std::array::from_fn(|_| AtomicU64::new(0)),
            queued: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        });
        let worker_resource = resource.clone();
        let worker_transport = Arc::clone(&transport);
        let worker_cancellation = cancellation.clone();
        let worker_counters = Arc::clone(&counters);
        let worker = std::thread::Builder::new()
            .name("heycode-otel-export".to_owned())
            .spawn(move || {
                run_worker(
                    receiver,
                    worker_resource,
                    worker_transport,
                    worker_cancellation,
                    worker_counters,
                    config,
                );
            })
            .map_err(|_| OtelExporterStartFault::WorkerUnavailable)?;
        Ok(Self {
            resource,
            transport,
            sender,
            admission: Mutex::new(()),
            worker: Mutex::new(Some(worker)),
            cancellation,
            counters,
            phase: AtomicU8::new(PHASE_ACTIVE),
            max_queue_events: config.max_queue_events,
            shutdown_grace: config.shutdown_grace,
        })
    }

    /// The resource every document from this exporter is attributed to.
    #[must_use]
    pub const fn resource(&self) -> &OtlpResource {
        &self.resource
    }

    /// Where this exporter's transport sends.
    #[must_use]
    pub fn destination(&self) -> &OtlpEndpoint {
        self.transport.destination()
    }

    /// How this exporter's transport authenticates.
    #[must_use]
    pub fn authentication(&self) -> &OtlpAuth {
        self.transport.authentication()
    }

    /// The document this exporter would send for `events`.
    ///
    /// Public so an audit can inspect exactly what would go out. The owned
    /// worker uses the same pure builder for live batches.
    #[must_use]
    pub fn payload_for(&self, events: &[TelemetryEvent]) -> OtlpPayload {
        payload_for(&self.resource, events)
    }

    /// How many documents reached the collector.
    #[must_use]
    pub fn sent(&self) -> u64 {
        self.counters.sent.load(Ordering::Relaxed)
    }

    /// How many were lost to `fault`.
    #[must_use]
    pub fn refused(&self, fault: OtlpSendFault) -> u64 {
        self.counters.refusals[fault.index()].load(Ordering::Relaxed)
    }

    /// Every fault with its count, in [`OtlpSendFault::ALL`] order.
    #[must_use]
    pub fn refusals(&self) -> Vec<(OtlpSendFault, u64)> {
        OtlpSendFault::ALL
            .into_iter()
            .map(|fault| (fault, self.refused(fault)))
            .collect()
    }

    /// Total documents lost, whatever the class.
    #[must_use]
    pub fn total_refused(&self) -> u64 {
        OtlpSendFault::ALL
            .into_iter()
            .fold(0, |total, fault| total.saturating_add(self.refused(fault)))
    }

    /// Events refused before a batch because the bounded queue was full or the
    /// lifecycle had already closed.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
    }

    /// Settle every event admitted before this call.
    pub fn flush(&self) {
        let (acknowledge, settled) = mpsc::sync_channel(1);
        {
            let _admission = lock_unpoisoned(&self.admission);
            if self.phase.load(Ordering::Acquire) != PHASE_ACTIVE
                || self.sender.send(WorkerCommand::Flush(acknowledge)).is_err()
            {
                return;
            }
        }
        if settled.recv_timeout(self.shutdown_grace).is_err() {
            self.cancel_and_join();
        }
    }

    /// Flush admitted events, release the transport and join the owned worker.
    /// The plugin's disposer calls this; it is idempotent.
    ///
    /// A cooperative transport gets a grace period to settle the ordered
    /// shutdown barrier. If it remains blocked, the lifecycle token is
    /// cancelled and the worker is still joined before this method returns.
    pub fn shutdown(&self) {
        let mut settlement = None;
        {
            let _admission = lock_unpoisoned(&self.admission);
            if self
                .phase
                .compare_exchange(
                    PHASE_ACTIVE,
                    PHASE_SHUTTING_DOWN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                let (acknowledge, settled) = mpsc::sync_channel(1);
                if self
                    .sender
                    .send(WorkerCommand::Shutdown(acknowledge))
                    .is_ok()
                {
                    settlement = Some(settled);
                }
            }
        }
        if settlement
            .as_ref()
            .is_some_and(|settled| settled.recv_timeout(self.shutdown_grace).is_err())
        {
            self.cancellation.cancel();
            let _ = self.sender.send(WorkerCommand::Wake);
        }
        self.join_worker();
        self.phase.store(PHASE_CLOSED, Ordering::Release);
    }

    fn cancel_and_join(&self) {
        {
            let _admission = lock_unpoisoned(&self.admission);
            if self.phase.load(Ordering::Acquire) != PHASE_CLOSED {
                self.phase.store(PHASE_SHUTTING_DOWN, Ordering::Release);
                self.cancellation.cancel();
                let _ = self.sender.send(WorkerCommand::Wake);
            }
        }
        self.join_worker();
        self.phase.store(PHASE_CLOSED, Ordering::Release);
    }

    fn join_worker(&self) {
        let worker = lock_unpoisoned(&self.worker).take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }

    fn admit(&self, event: &TelemetryEvent) {
        let _admission = lock_unpoisoned(&self.admission);
        if self.phase.load(Ordering::Acquire) != PHASE_ACTIVE || !self.reserve_queue_slot() {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self
            .sender
            .send(WorkerCommand::Event(event.clone()))
            .is_err()
        {
            release_queue_slot(&self.counters);
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn reserve_queue_slot(&self) -> bool {
        let mut queued = self.counters.queued.load(Ordering::Acquire);
        loop {
            if queued >= self.max_queue_events {
                return false;
            }
            match self.counters.queued.compare_exchange_weak(
                queued,
                queued + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => queued = observed,
            }
        }
    }
}

impl TelemetryExporter for OtelExporter {
    fn export(&self, event: &TelemetryEvent) {
        self.admit(event);
    }

    fn flush(&self) {
        OtelExporter::flush(self);
    }
}

impl Drop for OtelExporter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for OtelExporter {
    /// Renders the destination, whether a credential is configured, and the
    /// counts — never the transport and never a credential.
    ///
    /// Written rather than derived. A derive would print the transport, and a
    /// transport is exactly the object holding a live HTTP client with its
    /// default headers in it; whatever `Debug` that client derived would come
    /// out here. `TelemetryService` refuses to print its exporter for the same
    /// reason, and this is that rule one level down.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OtelExporter")
            .field("destination", &self.destination().as_address())
            .field("authenticated", &self.authentication().configured())
            .field("sent", &self.sent())
            .field("refused", &self.total_refused())
            .finish()
    }
}

fn run_worker(
    receiver: Receiver<WorkerCommand>,
    resource: OtlpResource,
    transport: Arc<dyn OtlpTransport>,
    cancellation: OtlpCancellation,
    counters: Arc<ExporterCounters>,
    config: WorkerConfig,
) {
    let acknowledge = catch_unwind(AssertUnwindSafe(|| {
        worker_loop(
            &receiver,
            &resource,
            transport.as_ref(),
            &cancellation,
            &counters,
            config,
        )
    }))
    .ok()
    .flatten();
    counters.queued.store(0, Ordering::Release);
    let _ = catch_unwind(AssertUnwindSafe(|| transport.shutdown(&cancellation)));
    if let Some(acknowledge) = acknowledge {
        let _ = acknowledge.send(());
    }
}

fn worker_loop(
    receiver: &Receiver<WorkerCommand>,
    resource: &OtlpResource,
    transport: &dyn OtlpTransport,
    cancellation: &OtlpCancellation,
    counters: &ExporterCounters,
    config: WorkerConfig,
) -> Option<SyncSender<()>> {
    let mut batch = Vec::with_capacity(config.max_batch_events);
    let mut deadline = None;
    loop {
        if cancellation.is_cancelled() {
            return None;
        }
        let command = if batch.is_empty() {
            match receiver.recv() {
                Ok(command) => Some(command),
                Err(_) => return None,
            }
        } else {
            let wait = deadline
                .map(|at: Instant| at.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO);
            match receiver.recv_timeout(wait) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    send_batch(resource, transport, cancellation, counters, &mut batch);
                    return None;
                }
            }
        };
        match command {
            Some(WorkerCommand::Event(event)) => {
                release_queue_slot(counters);
                if cancellation.is_cancelled() {
                    return None;
                }
                if batch.is_empty() {
                    deadline = Instant::now().checked_add(config.scheduled_delay);
                }
                batch.push(event);
                if batch.len() >= config.max_batch_events {
                    send_batch(resource, transport, cancellation, counters, &mut batch);
                    deadline = None;
                }
            }
            Some(WorkerCommand::Flush(acknowledge)) => {
                send_batch(resource, transport, cancellation, counters, &mut batch);
                deadline = None;
                let _ = acknowledge.send(());
            }
            Some(WorkerCommand::Shutdown(acknowledge)) => {
                send_batch(resource, transport, cancellation, counters, &mut batch);
                return Some(acknowledge);
            }
            Some(WorkerCommand::Wake) => {
                if cancellation.is_cancelled() {
                    return None;
                }
            }
            None => {
                send_batch(resource, transport, cancellation, counters, &mut batch);
                deadline = None;
            }
        }
    }
}

fn send_batch(
    resource: &OtlpResource,
    transport: &dyn OtlpTransport,
    cancellation: &OtlpCancellation,
    counters: &ExporterCounters,
    batch: &mut Vec<TelemetryEvent>,
) {
    if batch.is_empty() || cancellation.is_cancelled() {
        batch.clear();
        return;
    }
    let events = std::mem::take(batch);
    let payload = payload_for(resource, &events);
    match transport.send(&payload, cancellation) {
        Ok(()) => {
            counters.sent.fetch_add(1, Ordering::Relaxed);
        }
        Err(fault) => {
            counters.refusals[fault.index()].fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn release_queue_slot(counters: &ExporterCounters) {
    let _ = counters
        .queued
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |queued| {
            queued.checked_sub(1)
        });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::otel::resource::OtlpAttributeKey;
    use crate::{Dimension, Label, TelemetryEvent, TelemetryEventName, TelemetryService};
    use std::sync::Mutex;

    const SECRET: &str = "sk-ant-api03-0123456789abcdef";

    /// A transport that keeps what it was handed instead of sending it, so
    /// "this is what would have gone out" is an observation.
    struct CapturingTransport {
        destination: OtlpEndpoint,
        authentication: OtlpAuth,
        sent: Mutex<Vec<OtlpPayload>>,
        verdict: Mutex<Result<(), OtlpSendFault>>,
        shutdowns: AtomicU64,
    }

    impl CapturingTransport {
        fn new() -> Self {
            Self {
                destination: OtlpEndpoint::parse("https://collector.example.com:4318/v1/metrics")
                    .unwrap(),
                authentication: OtlpAuth::NoneConfigured,
                sent: Mutex::new(Vec::new()),
                verdict: Mutex::new(Ok(())),
                shutdowns: AtomicU64::new(0),
            }
        }

        fn failing(fault: OtlpSendFault) -> Self {
            let transport = Self::new();
            *transport.verdict.lock().unwrap() = Err(fault);
            transport
        }

        fn captured(&self) -> Vec<OtlpPayload> {
            self.sent.lock().unwrap().clone()
        }

        fn shutdowns(&self) -> u64 {
            self.shutdowns.load(Ordering::Relaxed)
        }
    }

    impl OtlpTransport for CapturingTransport {
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
            let verdict = *self.verdict.lock().unwrap();
            if verdict.is_ok() {
                self.sent.lock().unwrap().push(payload.clone());
            }
            verdict
        }
        fn shutdown(&self, _cancellation: &OtlpCancellation) {
            self.shutdowns.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct BlockingTransport {
        destination: OtlpEndpoint,
        authentication: OtlpAuth,
        entered: SyncSender<()>,
        cancelled: SyncSender<()>,
        release: (Mutex<bool>, Condvar),
        returned: AtomicBool,
        shutdowns: AtomicU64,
        shutdown_cancelled: AtomicBool,
    }

    impl BlockingTransport {
        fn new() -> (Arc<Self>, Receiver<()>, Receiver<()>) {
            let (entered, entered_rx) = mpsc::sync_channel(1);
            let (cancelled, cancelled_rx) = mpsc::sync_channel(1);
            (
                Arc::new(Self {
                    destination: OtlpEndpoint::parse(
                        "https://collector.example.com:4318/v1/metrics",
                    )
                    .unwrap(),
                    authentication: OtlpAuth::NoneConfigured,
                    entered,
                    cancelled,
                    release: (Mutex::new(false), Condvar::new()),
                    returned: AtomicBool::new(false),
                    shutdowns: AtomicU64::new(0),
                    shutdown_cancelled: AtomicBool::new(false),
                }),
                entered_rx,
                cancelled_rx,
            )
        }

        fn release(&self) {
            let mut released = self.release.0.lock().unwrap();
            *released = true;
            self.release.1.notify_all();
        }
    }

    impl OtlpTransport for BlockingTransport {
        fn destination(&self) -> &OtlpEndpoint {
            &self.destination
        }

        fn authentication(&self) -> &OtlpAuth {
            &self.authentication
        }

        fn send(
            &self,
            _payload: &OtlpPayload,
            cancellation: &OtlpCancellation,
        ) -> Result<(), OtlpSendFault> {
            let _ = self.entered.send(());
            cancellation.wait_cancelled();
            let _ = self.cancelled.send(());
            let mut released = self.release.0.lock().unwrap();
            while !*released {
                released = self.release.1.wait(released).unwrap();
            }
            self.returned.store(true, Ordering::Release);
            Err(OtlpSendFault::Cancelled)
        }

        fn shutdown(&self, cancellation: &OtlpCancellation) {
            self.shutdown_cancelled
                .store(cancellation.is_cancelled(), Ordering::Release);
            self.shutdowns.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn resource() -> OtlpResource {
        OtlpResource::new(Label::new("heycode").unwrap())
    }

    fn event() -> TelemetryEvent {
        TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1_700_000_000_000)
            .with_dimension(Dimension::Tool, Label::new("read_file").unwrap())
            .unwrap()
    }

    #[test]
    fn an_exporter_hands_the_transport_the_document_for_the_event_it_was_given() {
        let transport = Arc::new(CapturingTransport::new());
        let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
        exporter.export(&event());
        exporter.flush();
        let captured = transport.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0], exporter.payload_for(&[event()]));
        assert_eq!(exporter.sent(), 1);
        assert_eq!(exporter.total_refused(), 0);
    }

    #[test]
    fn an_ordered_flush_coalesces_every_preceding_event_into_one_batch() {
        let transport = Arc::new(CapturingTransport::new());
        let exporter = OtelExporter::with_config(
            resource(),
            transport.clone(),
            WorkerConfig {
                max_batch_events: 64,
                max_queue_events: 64,
                scheduled_delay: Duration::from_secs(60),
                shutdown_grace: Duration::from_millis(20),
            },
        )
        .unwrap();
        for name in [
            TelemetryEventName::SessionStarted,
            TelemetryEventName::ToolInvoked,
            TelemetryEventName::TurnCompleted,
        ] {
            exporter.export(&TelemetryEvent::new(name, 1));
        }

        exporter.flush();

        let captured = transport.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].data_point_count(), 3);
        assert_eq!(exporter.sent(), 1, "sent counts documents, not events");
    }

    #[test]
    fn the_scheduled_deadline_flushes_a_partial_batch() {
        struct NotifyingTransport {
            destination: OtlpEndpoint,
            authentication: OtlpAuth,
            sent: SyncSender<OtlpPayload>,
        }
        impl OtlpTransport for NotifyingTransport {
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
                let _ = self.sent.send(payload.clone());
                Ok(())
            }
        }
        let (sent, received) = mpsc::sync_channel(1);
        let exporter = OtelExporter::with_config(
            resource(),
            Arc::new(NotifyingTransport {
                destination: OtlpEndpoint::parse("https://collector.example.com/v1/metrics")
                    .unwrap(),
                authentication: OtlpAuth::NoneConfigured,
                sent,
            }),
            WorkerConfig {
                max_batch_events: 64,
                max_queue_events: 64,
                scheduled_delay: Duration::from_millis(10),
                shutdown_grace: Duration::from_millis(20),
            },
        )
        .unwrap();
        exporter.export(&event());

        let payload = received
            .recv_timeout(Duration::from_secs(1))
            .expect("the scheduled deadline must flush a partial batch");
        assert_eq!(payload.data_point_count(), 1);
        assert_eq!(exporter.sent(), 1);
    }

    #[test]
    fn the_event_queue_is_bounded_while_a_transport_is_blocked() {
        let (transport, entered, _cancelled) = BlockingTransport::new();
        let exporter = OtelExporter::with_config(
            resource(),
            transport.clone(),
            WorkerConfig {
                max_batch_events: 1,
                max_queue_events: 2,
                scheduled_delay: Duration::from_secs(60),
                shutdown_grace: Duration::from_millis(10),
            },
        )
        .unwrap();
        exporter.export(&event());
        entered
            .recv_timeout(Duration::from_secs(1))
            .expect("the first batch must reach the blocking transport");
        for _ in 0..5 {
            exporter.export(&event());
        }
        assert_eq!(
            exporter.dropped(),
            3,
            "two events fit behind the in-flight batch and the rest are refused"
        );
        transport.release();
        exporter.shutdown();
    }

    #[test]
    fn cancellation_is_observed_and_the_worker_is_joined_before_shutdown_returns() {
        let (transport, entered, cancelled) = BlockingTransport::new();
        let exporter = Arc::new(
            OtelExporter::with_config(
                resource(),
                transport.clone(),
                WorkerConfig {
                    max_batch_events: 1,
                    max_queue_events: 8,
                    scheduled_delay: Duration::from_secs(60),
                    shutdown_grace: Duration::from_millis(10),
                },
            )
            .unwrap(),
        );
        exporter.export(&event());
        entered
            .recv_timeout(Duration::from_secs(1))
            .expect("the send must be in flight before shutdown");
        let (finished, finished_rx) = mpsc::sync_channel(1);
        let shutting_down = exporter.clone();
        let shutdown = std::thread::spawn(move || {
            shutting_down.shutdown();
            let _ = finished.send(());
        });

        cancelled
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown must cancel the in-flight transport");
        assert!(
            finished_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "signalling cancellation is not settlement; shutdown must still join"
        );
        transport.release();
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown must return after the transport settles");
        shutdown.join().unwrap();

        assert!(transport.returned.load(Ordering::Acquire));
        assert_eq!(transport.shutdowns.load(Ordering::Relaxed), 1);
        assert!(transport.shutdown_cancelled.load(Ordering::Acquire));
        assert_eq!(exporter.refused(OtlpSendFault::Cancelled), 1);
    }

    #[test]
    fn raw_sdk_failure_text_has_no_route_out_of_the_closed_transport_fault() {
        struct RawFailureTransport {
            destination: OtlpEndpoint,
            authentication: OtlpAuth,
            raw_sdk_text: String,
        }
        impl OtlpTransport for RawFailureTransport {
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
                assert!(self.raw_sdk_text.contains(SECRET));
                Err(OtlpSendFault::Refused)
            }
        }
        let exporter = OtelExporter::new(
            resource(),
            Arc::new(RawFailureTransport {
                destination: OtlpEndpoint::parse("https://collector.example.com/v1/metrics")
                    .unwrap(),
                authentication: OtlpAuth::NoneConfigured,
                raw_sdk_text: format!("request failed: response body echoed {SECRET}"),
            }),
        )
        .unwrap();
        exporter.export(&event());
        exporter.flush();
        assert_eq!(exporter.refused(OtlpSendFault::Refused), 1);
        let rendered = format!("{exporter:?} {:?}", exporter.refusals());
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains("response body"), "{rendered}");
    }

    #[test]
    fn a_failed_send_is_counted_by_class_and_carries_no_message_to_count_it_by() {
        for fault in OtlpSendFault::ALL {
            let transport = Arc::new(CapturingTransport::failing(fault));
            let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
            exporter.export(&event());
            exporter.flush();
            exporter.export(&event());
            exporter.flush();
            assert_eq!(exporter.refused(fault), 2, "{fault}");
            assert_eq!(exporter.total_refused(), 2, "{fault}");
            assert_eq!(exporter.sent(), 0, "{fault}");
            assert!(transport.captured().is_empty(), "{fault}");
        }
    }

    #[test]
    fn send_faults_report_every_class_in_stable_order() {
        let exporter = OtelExporter::new(resource(), Arc::new(CapturingTransport::new())).unwrap();
        let reported: Vec<OtlpSendFault> =
            exporter.refusals().into_iter().map(|(f, _)| f).collect();
        assert_eq!(reported, OtlpSendFault::ALL.to_vec());
        for (position, fault) in OtlpSendFault::ALL.into_iter().enumerate() {
            assert_eq!(fault.index(), position, "{fault} is misindexed");
        }
        let mut identifiers: Vec<&str> = OtlpSendFault::ALL.iter().map(|f| f.as_str()).collect();
        identifiers.sort_unstable();
        let count = identifiers.len();
        identifiers.dedup();
        assert_eq!(identifiers.len(), count, "duplicate send fault identifier");
    }

    #[test]
    fn an_exporter_reports_the_destination_the_transport_actually_uses() {
        let transport = Arc::new(CapturingTransport::new());
        let exporter = OtelExporter::new(resource(), transport.clone()).unwrap();
        assert_eq!(
            exporter.destination().as_address(),
            "https://collector.example.com:4318/v1/metrics",
            "the destination is asked of the transport, so the two cannot disagree"
        );
        assert!(!exporter.authentication().configured());
    }

    #[test]
    fn debug_reports_destination_counts_and_whether_a_credential_exists_only() {
        struct NamedTransport {
            destination: OtlpEndpoint,
            authentication: OtlpAuth,
        }
        impl OtlpTransport for NamedTransport {
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
                Ok(())
            }
        }
        let exporter = OtelExporter::new(
            resource(),
            Arc::new(NamedTransport {
                destination: OtlpEndpoint::parse("https://collector.example.com/v1/metrics")
                    .unwrap(),
                authentication: OtlpAuth::Configured {
                    header: Label::new("authorization").unwrap(),
                    credential: Label::new("otlp/collector").unwrap(),
                },
            }),
        )
        .unwrap();
        exporter.export(&event());
        exporter.flush();
        let rendered = format!("{exporter:?}");
        assert!(rendered.contains("collector.example.com"), "{rendered}");
        assert!(rendered.contains("authenticated: true"), "{rendered}");
        assert!(rendered.contains("sent: 1"), "{rendered}");
        assert!(!rendered.contains("NamedTransport"), "{rendered}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains("Bearer"), "{rendered}");
    }

    #[test]
    fn the_exporter_reaches_a_transport_only_through_an_exporting_service() {
        let transport = Arc::new(CapturingTransport::new());
        let exporter = Arc::new(OtelExporter::new(resource(), transport.clone()).unwrap());
        let service = TelemetryService::exporting(exporter);
        service.record(&event());
        service.flush();
        assert_eq!(transport.captured().len(), 1);

        let local_off = TelemetryService::local_off();
        local_off.record(&event());
        assert_eq!(
            transport.captured().len(),
            1,
            "a local-off service has no exporter to reach, whatever exists elsewhere"
        );
    }

    #[test]
    fn shutdown_releases_exactly_the_transport_this_exporter_was_given() {
        let mine = Arc::new(CapturingTransport::new());
        let other = Arc::new(CapturingTransport::new());
        let exporter = OtelExporter::new(resource(), mine.clone()).unwrap();
        exporter.shutdown();
        assert_eq!(mine.shutdowns(), 1);
        assert_eq!(other.shutdowns(), 0, "a disposer must not reach elsewhere");
    }

    #[test]
    fn the_resource_an_exporter_was_built_with_attributes_every_document() {
        let resource = resource()
            .with_attribute(
                OtlpAttributeKey::DeploymentEnvironment,
                Label::new("staging").unwrap(),
            )
            .unwrap();
        let transport = Arc::new(CapturingTransport::new());
        let exporter = OtelExporter::new(resource, transport.clone()).unwrap();
        exporter.export(&event());
        exporter.flush();
        let document = serde_json::to_value(&transport.captured()[0]).unwrap();
        let attributes = document["resourceMetrics"][0]["resource"]["attributes"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            attributes.iter().any(|attribute| {
                attribute["key"] == "deployment.environment"
                    && attribute["value"]["stringValue"] == "staging"
            }),
            "{attributes:?}"
        );
        assert_eq!(
            exporter
                .resource()
                .attribute(OtlpAttributeKey::DeploymentEnvironment)
                .map(Label::as_str),
            Some("staging")
        );
    }
}
