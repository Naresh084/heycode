//! Owned Codex app-server connection, handshake and request router.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use heycode_exec::{
    ManagedProcess, ProcessInput, ProcessOutputChunk, ProcessOutputReader, RawInteractiveProcess,
    SubprocessContainment,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::budget::InboundBudget;
use crate::config::CodexClientInfo;
use crate::jsonl::RawJsonLineDecoder;
use crate::wire::{
    ParsedServerMessage, parse_server_frame, serialize_notification, serialize_request,
    serialize_success_response,
};
use crate::{
    CodexAppServerError, CodexAppServerErrorCode, CodexCliVersion, CodexInboundEvent,
    CodexRequestId, CodexResponsePayload, CodexServerRequest,
};

const MAX_PENDING_REQUESTS: usize = 256;
const MAX_ABANDONED_REQUESTS: usize = 1024;
const MAX_SERVER_REQUESTS: usize = 256;
const INBOUND_CAPACITY: usize = 256;

#[cfg(test)]
type AdmissionHook = Arc<dyn Fn() + Send + Sync>;

/// Safe facts proven by initialize, excluding the private Codex home path.
#[derive(Clone, PartialEq, Eq)]
pub struct CodexHandshake {
    version: CodexCliVersion,
    platform_family: String,
    platform_os: String,
    user_agent: String,
}

impl CodexHandshake {
    pub(crate) fn parse(
        payload: CodexResponsePayload,
        version: CodexCliVersion,
    ) -> Result<Self, CodexAppServerError> {
        let value = payload.into_value();
        let object = value.as_object().ok_or_else(protocol)?;
        let _validated_private_codex_home = object
            .get("codexHome")
            .and_then(Value::as_str)
            .filter(|value| valid_one_line(value, 4096))
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(protocol)?;
        let platform_family = required_fact(object, "platformFamily", 64)?;
        let platform_os = required_fact(object, "platformOs", 64)?;
        let user_agent = object
            .get("userAgent")
            .and_then(Value::as_str)
            .filter(|value| valid_one_line(value, 512))
            .ok_or_else(protocol)?
            .to_owned();
        if !version.appears_in_user_agent(&user_agent) {
            return Err(CodexAppServerError::new(
                CodexAppServerErrorCode::UnsupportedVersion,
            ));
        }
        Ok(Self {
            version,
            platform_family,
            platform_os,
            user_agent,
        })
    }

    /// Executable release matched against the initialize user agent.
    #[must_use]
    pub const fn version(&self) -> CodexCliVersion {
        self.version
    }

    /// Runtime platform family such as `unix` or `windows`.
    #[must_use]
    pub fn platform_family(&self) -> &str {
        &self.platform_family
    }

    /// Runtime operating-system id.
    #[must_use]
    pub fn platform_os(&self) -> &str {
        &self.platform_os
    }

    /// Bounded app-server user agent.
    #[must_use]
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

impl std::fmt::Debug for CodexHandshake {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexHandshake")
            .field("version", &self.version)
            .field("platform_family", &self.platform_family)
            .field("platform_os", &self.platform_os)
            .field("user_agent_bytes", &self.user_agent.len())
            .finish()
    }
}

/// Initialized, correlated and contained-process-owned app-server connection.
#[derive(Clone)]
pub struct CodexAppServerClient {
    transport: Arc<TransportInner>,
    handshake: Arc<CodexHandshake>,
}

impl CodexAppServerClient {
    pub(crate) async fn initialize(
        process: RawInteractiveProcess,
        version: CodexCliVersion,
        client_info: &CodexClientInfo,
        lifecycle: CancellationToken,
        cancellation: CancellationToken,
        experimental_api: bool,
    ) -> Result<Self, CodexAppServerError> {
        let transport = TransportInner::new(process, lifecycle);
        let params = json!({
            "clientInfo": {
                "name": client_info.name(),
                "title": client_info.title(),
                "version": client_info.version(),
            },
            "capabilities": {"experimentalApi": experimental_api}
        });
        let result = transport
            .request_raw("initialize", params, cancellation.clone())
            .await;
        let payload = match result {
            Ok(payload) => payload,
            Err(error) => {
                transport.close_internal().await;
                return Err(error);
            }
        };
        let handshake = match CodexHandshake::parse(payload, version) {
            Ok(handshake) => handshake,
            Err(error) => {
                transport.close_internal().await;
                return Err(error);
            }
        };
        if cancellation.is_cancelled() {
            transport.close_internal().await;
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        if let Err(error) = transport
            .send_notification("initialized", json!({}), cancellation)
            .await
        {
            transport.close_internal().await;
            return Err(error);
        }
        transport.ready.store(true, Ordering::Release);
        Ok(Self {
            transport,
            handshake: Arc::new(handshake),
        })
    }

    /// Proven initialize facts.
    #[must_use]
    pub fn handshake(&self) -> &CodexHandshake {
        &self.handshake
    }

    /// Process-tree containment facts attached to this connection.
    ///
    /// A false `resists_session_escape` means close settles the contained
    /// group, while a deliberately detached POSIX session is outside that
    /// guarantee.
    #[must_use]
    pub fn containment(&self) -> &SubprocessContainment {
        &self.transport.containment
    }

    /// Send one correlated stable-API request.
    ///
    /// # Errors
    /// Invalid method/params, cancellation, remote errors, protocol failure or
    /// closed connection fail with fixed body-free diagnostics.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<CodexResponsePayload, CodexAppServerError> {
        if !self.transport.ready.load(Ordering::Acquire) {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Conflict));
        }
        self.transport
            .request_raw(method, params, cancellation)
            .await
    }

    /// Receive the next notification or server request for the single event
    /// consumer.
    ///
    /// # Errors
    /// Cancellation, protocol failure or closed connection fails safely.
    pub async fn next_event(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CodexInboundEvent, CodexAppServerError> {
        if !self.transport.ready.load(Ordering::Acquire) {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Conflict));
        }
        self.transport.next_event(cancellation).await
    }

    /// Answer one outstanding server request with a successful result.
    ///
    /// # Errors
    /// Unknown/duplicate ids, invalid result data, cancellation, process or
    /// closed connection fail safely.
    pub async fn respond_success(
        &self,
        id: &CodexRequestId,
        result: Value,
        cancellation: CancellationToken,
    ) -> Result<(), CodexAppServerError> {
        self.transport
            .respond_success(id, result, cancellation)
            .await
    }

    /// Idempotently cancel and reap the app-server's reported containment group.
    ///
    /// Once admitted, close ignores later caller cancellation and returns only
    /// after the reader/input/process owners have settled. Use
    /// [`Self::containment`] to determine whether the host prevents deliberate
    /// descendant session escape.
    ///
    /// # Errors
    /// Pre-admission cancellation or unconfirmed process teardown fails safely.
    pub async fn close(&self, cancellation: CancellationToken) -> Result<(), CodexAppServerError> {
        self.transport.close(cancellation).await
    }
}

impl std::fmt::Debug for CodexAppServerClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexAppServerClient")
            .field("handshake", &self.handshake)
            .field("state", &self.transport.protocol.status_tag())
            .finish()
    }
}

struct TransportInner {
    protocol: Arc<ProtocolState>,
    input: AsyncMutex<Option<ProcessInput>>,
    process: AsyncMutex<Option<ManagedProcess>>,
    events: Arc<AsyncMutex<mpsc::Receiver<CodexInboundEvent>>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    close_gate: AsyncMutex<()>,
    ready: AtomicBool,
    containment: SubprocessContainment,
}

impl TransportInner {
    fn new(process: RawInteractiveProcess, lifecycle: CancellationToken) -> Arc<Self> {
        let (process, input, output) = process.into_raw_parts();
        let containment = process.containment().clone();
        let (events_tx, events_rx) = mpsc::channel(INBOUND_CAPACITY);
        let protocol = Arc::new(ProtocolState::new(lifecycle, events_tx));
        let transport = Arc::new(Self {
            protocol: protocol.clone(),
            input: AsyncMutex::new(Some(input)),
            process: AsyncMutex::new(Some(process)),
            events: Arc::new(AsyncMutex::new(events_rx)),
            reader: Mutex::new(None),
            close_gate: AsyncMutex::new(()),
            ready: AtomicBool::new(false),
            containment,
        });
        let lifecycle = protocol.lifecycle.clone();
        let reader = tokio::spawn(async move { read_loop(output, protocol, lifecycle).await });
        *lock(&transport.reader) = Some(reader);
        transport
    }

    async fn request_raw(
        self: &Arc<Self>,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<CodexResponsePayload, CodexAppServerError> {
        if cancellation.is_cancelled() {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        self.protocol.ensure_open()?;
        let id = self.protocol.next_request_id()?;
        let line = serialize_request(id, method, params)?;
        let (sender, receiver) = oneshot::channel();
        self.protocol.register_request(id, sender)?;
        let mut guard = PendingRequestGuard::new(self.protocol.clone(), id);
        match self.send_line(&line, &cancellation).await {
            Ok(()) => guard.mark_sent(),
            Err(SendLineFailure::Recoverable(error)) => return Err(error),
            Err(SendLineFailure::Terminal(error)) => {
                self.protocol.fail(error.code());
                self.close_internal().await;
                return Err(error);
            }
        }
        // Once the reader has delivered this request's correlated response,
        // that request boundary is complete. A later frame may fail the
        // connection in the same scheduler turn; keep that failure on the
        // event/next-operation plane instead of racing it with this response.
        let result = tokio::select! {
            biased;
            result = receiver => {
                guard.disarm();
                result.unwrap_or_else(|_| Err(protocol()))
            }
            () = cancellation.cancelled() => {
                Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled))
            }
            () = self.protocol.lifecycle.cancelled() => {
                Err(self.protocol.current_error_or_protocol())
            }
        };
        if result.is_err() && self.protocol.is_failed() {
            self.close_internal().await;
        }
        result
    }

    async fn send_notification(
        &self,
        method: &str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<(), CodexAppServerError> {
        self.protocol.ensure_open()?;
        let line = serialize_notification(method, params)?;
        self.send_line(&line, &cancellation)
            .await
            .map_err(SendLineFailure::into_error)
    }

    async fn respond_success(
        &self,
        id: &CodexRequestId,
        result: Value,
        cancellation: CancellationToken,
    ) -> Result<(), CodexAppServerError> {
        if cancellation.is_cancelled() {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        self.protocol.ensure_open()?;
        let line = serialize_success_response(id, result)?;
        self.protocol.claim_server_request(id)?;
        match self.send_line(&line, &cancellation).await {
            Ok(()) => {}
            Err(SendLineFailure::Recoverable(error)) => {
                self.protocol.restore_server_request(id)?;
                return Err(error);
            }
            Err(SendLineFailure::Terminal(error)) => {
                self.protocol.fail(error.code());
                self.close_internal().await;
                return Err(error);
            }
        }
        Ok(())
    }

    async fn send_line(
        &self,
        line: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), SendLineFailure> {
        let input = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(SendLineFailure::Recoverable(CodexAppServerError::new(
                    CodexAppServerErrorCode::Cancelled,
                )));
            }
            () = self.protocol.lifecycle.cancelled() => {
                return Err(SendLineFailure::Terminal(
                    self.protocol.current_error_or_protocol(),
                ));
            }
            input = self.input.lock() => input,
        };
        let mut input = input;
        self.protocol
            .ensure_open()
            .map_err(SendLineFailure::Terminal)?;
        let input = input
            .as_mut()
            .ok_or_else(|| SendLineFailure::Terminal(closed()))?;
        let write = input.write_line(line);
        tokio::pin!(write);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.protocol.fail(CodexAppServerErrorCode::Cancelled);
                let _settled = write.await;
                Err(SendLineFailure::Terminal(CodexAppServerError::new(
                    CodexAppServerErrorCode::Cancelled,
                )))
            }
            () = self.protocol.lifecycle.cancelled() => {
                let error = self.protocol.current_error_or_protocol();
                let _settled = write.await;
                Err(SendLineFailure::Terminal(error))
            }
            result = &mut write => result
                .map_err(CodexAppServerError::from)
                .map_err(SendLineFailure::Terminal),
        }
    }

    async fn next_event(
        &self,
        cancellation: CancellationToken,
    ) -> Result<CodexInboundEvent, CodexAppServerError> {
        if let Err(error) = self.protocol.ensure_open() {
            if self.protocol.is_failed() {
                self.close_internal().await;
            }
            return Err(error);
        }
        let result = receive_event(
            Arc::clone(&self.events),
            Arc::clone(&self.protocol),
            cancellation,
        )
        .await;
        if result.is_err() && self.protocol.is_failed() {
            self.close_internal().await;
        }
        result
    }

    async fn close(&self, cancellation: CancellationToken) -> Result<(), CodexAppServerError> {
        if let Some(result) = self.protocol.closed_result() {
            return result;
        }
        let gate = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
            }
            gate = self.close_gate.lock() => gate,
        };
        let _gate = gate;
        if let Some(result) = self.protocol.closed_result() {
            return result;
        }
        self.close_admitted().await
    }

    async fn close_internal(&self) {
        let _gate = self.close_gate.lock().await;
        if self.protocol.closed_result().is_none() {
            let _settled = self.close_admitted().await;
        }
    }

    async fn close_admitted(&self) -> Result<(), CodexAppServerError> {
        self.protocol.begin_close();
        let reader = lock(&self.reader).take();
        let reader_result = if let Some(reader) = reader {
            reader.await.map_err(|_| protocol())
        } else {
            Ok(())
        };
        {
            let mut events = self.events.lock().await;
            drain_event_receiver(&mut events);
        }
        if let Some(input) = self.input.lock().await.take() {
            let _input_result = input.finish().await;
        }
        let process_result = if let Some(process) = self.process.lock().await.take() {
            process.cancel().await.map_err(CodexAppServerError::from)
        } else {
            Ok(())
        };
        let result = reader_result.and(process_result);
        self.protocol
            .finish_close(result.as_ref().err().map(CodexAppServerError::code));
        result
    }
}

fn drain_event_receiver(events: &mut mpsc::Receiver<CodexInboundEvent>) {
    events.close();
    while events.try_recv().is_ok() {}
}

async fn receive_event(
    events: Arc<AsyncMutex<mpsc::Receiver<CodexInboundEvent>>>,
    protocol: Arc<ProtocolState>,
    cancellation: CancellationToken,
) -> Result<CodexInboundEvent, CodexAppServerError> {
    let events = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled));
        }
        () = protocol.lifecycle.cancelled() => {
            return Err(protocol.current_error_or_protocol());
        }
        events = events.lock() => events,
    };
    let mut events = events;
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            Err(CodexAppServerError::new(CodexAppServerErrorCode::Cancelled))
        }
        () = protocol.lifecycle.cancelled() => {
            Err(protocol.current_error_or_protocol())
        }
        event = events.recv() => {
            event.ok_or_else(|| protocol.current_error_or_protocol())
        }
    }
}

impl Drop for TransportInner {
    fn drop(&mut self) {
        self.protocol.lifecycle.cancel();
        if let Some(reader) = lock(&self.reader).take() {
            reader.abort();
        }
        let _input = self.input.get_mut().take();
        let _process = self.process.get_mut().take();
    }
}

struct ProtocolState {
    lifecycle: CancellationToken,
    control: Mutex<ControlState>,
    next_id: AtomicI64,
    events: mpsc::Sender<CodexInboundEvent>,
    inbound_budget: Arc<InboundBudget>,
    #[cfg(test)]
    admission_hook: Mutex<Option<AdmissionHook>>,
}

impl ProtocolState {
    fn new(lifecycle: CancellationToken, events: mpsc::Sender<CodexInboundEvent>) -> Self {
        Self {
            lifecycle,
            control: Mutex::new(ControlState {
                status: ConnectionStatus::Open,
                requests: RequestBook::default(),
                server_requests: HashSet::new(),
            }),
            next_id: AtomicI64::new(0),
            events,
            inbound_budget: InboundBudget::new(),
            #[cfg(test)]
            admission_hook: Mutex::new(None),
        }
    }

    fn ensure_open(&self) -> Result<(), CodexAppServerError> {
        match lock(&self.control).status {
            ConnectionStatus::Open => Ok(()),
            ConnectionStatus::Failed(code) => Err(CodexAppServerError::new(code)),
            ConnectionStatus::Closing | ConnectionStatus::Closed(_) => Err(closed()),
        }
    }

    fn next_request_id(&self) -> Result<i64, CodexAppServerError> {
        self.next_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < i64::MAX).then_some(current + 1)
            })
            .map_err(|_| protocol())
    }

    fn register_request(
        &self,
        id: i64,
        sender: oneshot::Sender<Result<CodexResponsePayload, CodexAppServerError>>,
    ) -> Result<(), CodexAppServerError> {
        let mut control = lock(&self.control);
        if !matches!(control.status, ConnectionStatus::Open) {
            return Err(error_for_status(control.status));
        }
        #[cfg(test)]
        if let Some(hook) = lock(&self.admission_hook).as_ref() {
            hook();
        }
        if control.requests.pending.len() >= MAX_PENDING_REQUESTS
            || control.requests.pending.contains_key(&id)
        {
            return Err(CodexAppServerError::new(CodexAppServerErrorCode::Conflict));
        }
        control.requests.pending.insert(id, sender);
        Ok(())
    }

    #[cfg(test)]
    fn set_admission_hook(&self, hook: AdmissionHook) {
        *lock(&self.admission_hook) = Some(hook);
    }

    fn abandon_request(&self, id: i64) {
        let mut control = lock(&self.control);
        if control.requests.pending.remove(&id).is_none() {
            return;
        }
        if control.requests.abandoned.len() >= MAX_ABANDONED_REQUESTS {
            drop(control);
            self.fail(CodexAppServerErrorCode::Protocol);
            return;
        }
        control.requests.abandoned.insert(id);
    }

    fn remove_unsent_request(&self, id: i64) {
        lock(&self.control).requests.pending.remove(&id);
    }

    fn settle_response(
        &self,
        id: CodexRequestId,
        result: Result<CodexResponsePayload, CodexAppServerError>,
    ) -> Result<(), CodexAppServerError> {
        let id = id
            .as_client_integer()
            .filter(|id| *id >= 0)
            .ok_or_else(protocol)?;
        let mut control = lock(&self.control);
        if let Some(sender) = control.requests.pending.remove(&id) {
            let _receiver_closed = sender.send(result);
            return Ok(());
        }
        if control.requests.abandoned.remove(&id) {
            return Ok(());
        }
        if !matches!(control.status, ConnectionStatus::Open) {
            return Ok(());
        }
        Err(protocol())
    }

    fn register_server_request(
        &self,
        request: &CodexServerRequest,
    ) -> Result<(), CodexAppServerError> {
        let mut control = lock(&self.control);
        if !matches!(control.status, ConnectionStatus::Open)
            || control.server_requests.len() >= MAX_SERVER_REQUESTS
            || !control.server_requests.insert(request.id().clone())
        {
            return Err(protocol());
        }
        Ok(())
    }

    fn claim_server_request(&self, id: &CodexRequestId) -> Result<(), CodexAppServerError> {
        let mut control = lock(&self.control);
        if !matches!(control.status, ConnectionStatus::Open) {
            return Err(error_for_status(control.status));
        }
        if control.server_requests.remove(id) {
            Ok(())
        } else {
            Err(CodexAppServerError::new(CodexAppServerErrorCode::Conflict))
        }
    }

    fn restore_server_request(&self, id: &CodexRequestId) -> Result<(), CodexAppServerError> {
        let mut control = lock(&self.control);
        if !matches!(control.status, ConnectionStatus::Open)
            || control.server_requests.len() >= MAX_SERVER_REQUESTS
            || !control.server_requests.insert(id.clone())
        {
            Err(protocol())
        } else {
            Ok(())
        }
    }

    fn fail(&self, code: CodexAppServerErrorCode) {
        let pending = {
            let mut control = lock(&self.control);
            if !matches!(control.status, ConnectionStatus::Open) {
                return;
            }
            control.status = ConnectionStatus::Failed(code);
            std::mem::take(&mut control.requests.pending)
        };
        self.lifecycle.cancel();
        settle_pending(pending, code);
    }

    fn fail_if_open(&self, code: CodexAppServerErrorCode) {
        self.fail(code);
    }

    fn begin_close(&self) {
        let pending = {
            let mut control = lock(&self.control);
            if matches!(control.status, ConnectionStatus::Closed(_)) {
                return;
            }
            control.status = ConnectionStatus::Closing;
            std::mem::take(&mut control.requests.pending)
        };
        self.lifecycle.cancel();
        settle_pending(pending, CodexAppServerErrorCode::Closed);
    }

    fn finish_close(&self, error: Option<CodexAppServerErrorCode>) {
        lock(&self.control).status = ConnectionStatus::Closed(error);
    }

    fn closed_result(&self) -> Option<Result<(), CodexAppServerError>> {
        match lock(&self.control).status {
            ConnectionStatus::Closed(None) => Some(Ok(())),
            ConnectionStatus::Closed(Some(code)) => Some(Err(CodexAppServerError::new(code))),
            ConnectionStatus::Open | ConnectionStatus::Failed(_) | ConnectionStatus::Closing => {
                None
            }
        }
    }

    fn current_error_or_protocol(&self) -> CodexAppServerError {
        error_for_status(lock(&self.control).status)
    }

    fn status_tag(&self) -> &'static str {
        match lock(&self.control).status {
            ConnectionStatus::Open => "open",
            ConnectionStatus::Failed(_) => "failed",
            ConnectionStatus::Closing => "closing",
            ConnectionStatus::Closed(_) => "closed",
        }
    }

    fn is_failed(&self) -> bool {
        matches!(lock(&self.control).status, ConnectionStatus::Failed(_))
    }
}

fn settle_pending(
    pending: BTreeMap<i64, oneshot::Sender<Result<CodexResponsePayload, CodexAppServerError>>>,
    code: CodexAppServerErrorCode,
) {
    for (_, sender) in pending {
        let _receiver_closed = sender.send(Err(CodexAppServerError::new(code)));
    }
}

fn error_for_status(status: ConnectionStatus) -> CodexAppServerError {
    match status {
        ConnectionStatus::Failed(code) | ConnectionStatus::Closed(Some(code)) => {
            CodexAppServerError::new(code)
        }
        ConnectionStatus::Closing | ConnectionStatus::Closed(None) => closed(),
        ConnectionStatus::Open => protocol(),
    }
}

struct ControlState {
    status: ConnectionStatus,
    requests: RequestBook,
    server_requests: HashSet<CodexRequestId>,
}

#[derive(Clone, Copy)]
enum ConnectionStatus {
    Open,
    Failed(CodexAppServerErrorCode),
    Closing,
    Closed(Option<CodexAppServerErrorCode>),
}

#[derive(Default)]
struct RequestBook {
    pending: BTreeMap<i64, oneshot::Sender<Result<CodexResponsePayload, CodexAppServerError>>>,
    abandoned: BTreeSet<i64>,
}

struct PendingRequestGuard {
    protocol: Arc<ProtocolState>,
    id: i64,
    armed: bool,
    sent: bool,
}

impl PendingRequestGuard {
    const fn new(protocol: Arc<ProtocolState>, id: i64) -> Self {
        Self {
            protocol,
            id,
            armed: true,
            sent: false,
        }
    }

    fn mark_sent(&mut self) {
        self.sent = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        if self.armed {
            if self.sent {
                self.protocol.abandon_request(self.id);
            } else {
                self.protocol.remove_unsent_request(self.id);
            }
        }
    }
}

enum SendLineFailure {
    Recoverable(CodexAppServerError),
    Terminal(CodexAppServerError),
}

impl SendLineFailure {
    fn into_error(self) -> CodexAppServerError {
        match self {
            Self::Recoverable(error) | Self::Terminal(error) => error,
        }
    }
}

async fn read_loop(
    mut output: ProcessOutputReader,
    state: Arc<ProtocolState>,
    lifecycle: CancellationToken,
) {
    let mut decoder = RawJsonLineDecoder::default();
    loop {
        let chunk = match output.read_chunk(lifecycle.clone()).await {
            Ok(ProcessOutputChunk::Data(bytes)) => bytes,
            Ok(ProcessOutputChunk::Eof) => {
                let code = decoder
                    .finish()
                    .err()
                    .map_or(CodexAppServerErrorCode::Protocol, |error| error.code());
                state.fail(code);
                return;
            }
            Err(_) => {
                state.fail_if_open(if lifecycle.is_cancelled() {
                    CodexAppServerErrorCode::Cancelled
                } else {
                    CodexAppServerErrorCode::Process
                });
                return;
            }
        };
        let frames = match decoder.push(&chunk) {
            Ok(frames) => frames,
            Err(error) => {
                state.fail(error.code());
                return;
            }
        };
        for frame in frames {
            let (message, weight) = match parse_server_frame(&frame) {
                Ok(parsed) => parsed,
                Err(error) => {
                    state.fail(error.code());
                    return;
                }
            };
            let permit = match state.inbound_budget.acquire(weight) {
                Ok(permit) => permit,
                Err(error) => {
                    state.fail(error.code());
                    return;
                }
            };
            if let Err(error) = route_message(message.with_permit(permit), &state) {
                state.fail(error.code());
                return;
            }
        }
    }
}

fn route_message(
    message: ParsedServerMessage,
    state: &ProtocolState,
) -> Result<(), CodexAppServerError> {
    match message {
        ParsedServerMessage::Response { id, payload } => state.settle_response(id, Ok(payload)),
        ParsedServerMessage::Error { id, code } => {
            let code = if code == -32001 {
                CodexAppServerErrorCode::Overloaded
            } else {
                CodexAppServerErrorCode::Remote
            };
            state.settle_response(id, Err(CodexAppServerError::new(code)))
        }
        ParsedServerMessage::Notification(notification) => state
            .events
            .try_send(CodexInboundEvent::Notification(notification))
            .map_err(|_| protocol()),
        ParsedServerMessage::Request(request) => {
            state.register_server_request(&request).and_then(|()| {
                state
                    .events
                    .try_send(CodexInboundEvent::Request(request))
                    .map_err(|_| protocol())
            })
        }
    }
}

fn required_fact(
    object: &serde_json::Map<String, Value>,
    field: &str,
    limit: usize,
) -> Result<String, CodexAppServerError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| {
            valid_one_line(value, limit)
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        .map(str::to_owned)
        .ok_or_else(protocol)
}

fn valid_one_line(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn protocol() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Protocol)
}

fn closed() -> CodexAppServerError {
    CodexAppServerError::new(CodexAppServerErrorCode::Closed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::budget::MAX_INBOUND_RETAINED_BYTES;

    fn protocol_state() -> (Arc<ProtocolState>, mpsc::Receiver<CodexInboundEvent>) {
        let (sender, receiver) = mpsc::channel(INBOUND_CAPACITY);
        (
            Arc::new(ProtocolState::new(CancellationToken::new(), sender)),
            receiver,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_transition_settles_boundary_request() {
        let (state, _events) = protocol_state();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hook_entered = entered.clone();
        let hook_release = release.clone();
        state.set_admission_hook(Arc::new(move || {
            hook_entered.wait();
            hook_release.wait();
        }));
        let (sender, receiver) = oneshot::channel();
        let request_state = state.clone();
        let request = std::thread::spawn(move || request_state.register_request(7, sender));
        entered.wait();
        let close_state = state.clone();
        let close_started = Arc::new(Barrier::new(2));
        let close_thread_started = close_started.clone();
        let close = std::thread::spawn(move || {
            close_thread_started.wait();
            close_state.begin_close();
        });
        close_started.wait();
        release.wait();
        request.join().unwrap().unwrap();
        close.join().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), receiver)
            .await
            .expect("request receiver stranded")
            .expect("sender dropped without settlement")
            .unwrap_err();
        assert_eq!(result.code(), CodexAppServerErrorCode::Closed);
    }

    #[tokio::test]
    async fn terminal_transition_cannot_lose_event_wakeup() {
        let (state, receiver) = protocol_state();
        let events = Arc::new(AsyncMutex::new(receiver));
        let held = events.lock().await;
        let first = tokio::spawn(receive_event(
            events.clone(),
            state.clone(),
            CancellationToken::new(),
        ));
        let second = tokio::spawn(receive_event(
            events.clone(),
            state.clone(),
            CancellationToken::new(),
        ));
        tokio::task::yield_now().await;
        state.fail(CodexAppServerErrorCode::Protocol);
        for waiter in [first, second] {
            let error = tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("event waiter stranded")
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code(), CodexAppServerErrorCode::Protocol);
        }
        drop(held);
    }

    #[test]
    fn inbound_events_obey_aggregate_budget_and_release_on_drop() {
        let (state, mut receiver) = protocol_state();
        let body = "x".repeat(900 * 1024);
        let frame = serde_json::to_vec(&serde_json::json!({
            "method": "fixture/event",
            "params": {"body": body}
        }))
        .unwrap();
        let mut retained_events = 0_usize;
        loop {
            let (message, weight) = parse_server_frame(&frame).unwrap();
            match state.inbound_budget.acquire(weight) {
                Ok(permit) => {
                    route_message(message.with_permit(permit), &state).unwrap();
                    retained_events += 1;
                }
                Err(error) => {
                    assert_eq!(error.code(), CodexAppServerErrorCode::Overloaded);
                    break;
                }
            }
        }
        assert!(retained_events < INBOUND_CAPACITY);
        assert!(state.inbound_budget.retained() <= MAX_INBOUND_RETAINED_BYTES);
        drain_event_receiver(&mut receiver);
        assert_eq!(state.inbound_budget.retained(), 0);
    }
}
