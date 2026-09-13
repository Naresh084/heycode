//! Owner-scoped persistent terminal sessions over the subprocess seam.
//!
//! A terminal outlives the call that opened it, so the registry — not the
//! caller — owns every session: its process tree, its lifecycle token, and the
//! one task draining its output. Sessions are addressed by an opaque id scoped
//! to the owner that opened them; a foreign owner cannot observe, drive, or
//! even distinguish the existence of a session it does not own.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    ManagedProcess, ProcessError, ProcessErrorCode, ProcessExit, ProcessInput, ProcessOutputChunk,
    ProcessOutputReader, ProcessSpec, SERVICE_SUBPROCESS, SERVICE_TERMINAL, SubprocessContainment,
    SubprocessService,
};

tokio::task_local! { static CALLER_TERMINAL_OWNER: TerminalOwner; }

/// Bind host-owned terminal authority for one tool dispatch and nested awaits.
pub async fn with_terminal_owner<F: std::future::Future>(
    owner: TerminalOwner,
    future: F,
) -> F::Output {
    CALLER_TERMINAL_OWNER.scope(owner, future).await
}

/// Current host-bound terminal authority, if the caller installed one.
#[must_use]
pub fn current_terminal_owner() -> Option<TerminalOwner> {
    CALLER_TERMINAL_OWNER.try_with(Clone::clone).ok()
}

/// Maximum bytes one [`TerminalService::read`] can return.
pub const MAX_TERMINAL_READ_BYTES: usize = crate::MAX_PROCESS_OUTPUT_CHUNK_BYTES;

/// Retained bytes of terminal output when a spec does not choose.
pub const DEFAULT_TERMINAL_RETAINED_BYTES: usize = 256 * 1024;

/// Largest retention a single terminal session may request.
pub const MAX_TERMINAL_RETAINED_BYTES: usize = 1024 * 1024;

/// Largest number of concurrently open sessions one owner may hold.
pub const MAX_TERMINAL_SESSIONS_PER_OWNER: usize = 8;

/// Largest number of concurrently open sessions the registry holds in total.
pub const MAX_TERMINAL_SESSIONS: usize = 64;

const MAX_OWNER_BYTES: usize = 128;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opaque identity of one persistent terminal session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalId(String);

impl TerminalId {
    fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Validate opaque text produced by a previous [`Self::as_str`].
    ///
    /// # Errors
    /// Text that is not a terminal identity is rejected.
    pub fn parse(value: &str) -> Result<Self, ProcessError> {
        uuid::Uuid::parse_str(value)
            .map(|parsed| Self(parsed.to_string()))
            .map_err(|_| ProcessError::new(ProcessErrorCode::InvalidSpec))
    }

    /// Stable opaque text for registries and diagnostics.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TerminalId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated authority scope a terminal session belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TerminalOwner(String);

impl TerminalOwner {
    /// Validate one owner scope.
    ///
    /// # Errors
    /// Empty, over-128-byte, and non-`[A-Za-z0-9._:-]` scopes are rejected, so
    /// an owner can never carry control characters or embedded structure.
    pub fn new(value: impl Into<String>) -> Result<Self, ProcessError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_OWNER_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(ProcessError::new(ProcessErrorCode::InvalidSpec))
        }
    }

    /// Stable owner text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TerminalOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated live terminal geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    /// Validate one non-degenerate geometry.
    ///
    /// # Errors
    /// A zero column or row count is rejected; it is never clamped.
    pub const fn new(cols: u16, rows: u16) -> Result<Self, ProcessError> {
        if cols == 0 || rows == 0 {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        Ok(Self { cols, rows })
    }

    /// Column count.
    #[must_use]
    pub const fn cols(self) -> u16 {
        self.cols
    }

    /// Row count.
    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }
}

impl Default for TerminalSize {
    /// The conventional 80x24 terminal.
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

/// Fully resolved persistent terminal launch.
///
/// `Debug` carries only counts and bounds; the underlying [`ProcessSpec`]
/// already redacts argv, environment values and paths.
#[derive(Clone)]
pub struct TerminalSpec {
    process: ProcessSpec,
    size: TerminalSize,
    retained_bytes: usize,
    observer: Option<Arc<dyn crate::ProcessOutputSink>>,
}

impl TerminalSpec {
    /// Bind an exact interactive process spec to a default 80x24 terminal.
    ///
    /// # Errors
    /// A terminal is interactive by construction, so a spec without
    /// [`ProcessSpec::with_interactive_stdio`] is rejected.
    pub fn new(process: ProcessSpec) -> Result<Self, ProcessError> {
        if !process.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        Ok(Self {
            process,
            size: TerminalSize::default(),
            retained_bytes: DEFAULT_TERMINAL_RETAINED_BYTES,
            observer: None,
        })
    }

    /// Attach a non-consuming bounded observer for the full terminal lifetime.
    #[must_use]
    pub fn with_output_sink(mut self, sink: Arc<dyn crate::ProcessOutputSink>) -> Self {
        self.observer = Some(sink);
        self
    }

    /// Set the initial terminal geometry.
    #[must_use]
    pub const fn with_size(mut self, size: TerminalSize) -> Self {
        self.size = size;
        self
    }

    /// Set how many bytes of the most recent output this session retains.
    ///
    /// # Errors
    /// The bound must be between one byte and
    /// [`MAX_TERMINAL_RETAINED_BYTES`].
    pub fn with_retained_bytes(mut self, bytes: usize) -> Result<Self, ProcessError> {
        if bytes == 0 || bytes > MAX_TERMINAL_RETAINED_BYTES {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        self.retained_bytes = bytes;
        Ok(self)
    }

    /// Exact process launch.
    #[must_use]
    pub const fn process(&self) -> &ProcessSpec {
        &self.process
    }

    /// Initial terminal geometry.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    /// Retained-output bound in bytes.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

impl std::fmt::Debug for TerminalSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSpec")
            .field("process", &self.process)
            .field("size", &self.size)
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

/// One bounded drain of a terminal's retained output.
pub struct TerminalRead {
    bytes: Vec<u8>,
    dropped_bytes: u64,
    ended: bool,
}

impl TerminalRead {
    /// Byte-exact terminal output in child order, escapes and CR framing intact.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Take the byte-exact output.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Cumulative bytes this session discarded at its retention bound.
    ///
    /// A non-zero value means older output was lost, never newer.
    #[must_use]
    pub const fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }

    /// Whether the terminal's output stream has ended.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }
}

impl std::fmt::Debug for TerminalRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalRead")
            .field("bytes", &self.bytes.len())
            .field("dropped_bytes", &self.dropped_bytes)
            .field("ended", &self.ended)
            .finish()
    }
}

/// Body-free status of one live registry session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStatus {
    id: TerminalId,
    size: TerminalSize,
    pending_bytes: usize,
    dropped_bytes: u64,
    output_ended: bool,
}

impl TerminalStatus {
    /// Opaque session identity.
    #[must_use]
    pub const fn id(&self) -> &TerminalId {
        &self.id
    }

    /// Last geometry applied to the live terminal.
    #[must_use]
    pub const fn size(&self) -> TerminalSize {
        self.size
    }

    /// Retained bytes not yet read.
    #[must_use]
    pub const fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Cumulative bytes discarded at the retention bound.
    #[must_use]
    pub const fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }

    /// Whether the terminal's output stream has ended.
    #[must_use]
    pub const fn output_ended(&self) -> bool {
        self.output_ended
    }
}

/// Bounded drop-oldest retention for one session's terminal output.
struct Backlog {
    state: Mutex<BacklogState>,
}

struct BacklogState {
    bytes: VecDeque<u8>,
    capacity: usize,
    dropped: u64,
    ended: bool,
}

impl Backlog {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(BacklogState {
                bytes: VecDeque::new(),
                capacity,
                dropped: 0,
                ended: false,
            }),
        }
    }

    /// Append `chunk`, discarding the oldest bytes past the bound.
    ///
    /// Push and trim share one critical section, so no observer can ever see
    /// more than `capacity` retained bytes.
    fn push(&self, chunk: &[u8]) {
        let mut state = lock(&self.state);
        state.bytes.extend(chunk);
        let excess = state.bytes.len().saturating_sub(state.capacity);
        if excess > 0 {
            state.bytes.drain(..excess);
            state.dropped = state.dropped.saturating_add(excess as u64);
        }
    }

    fn end(&self) {
        lock(&self.state).ended = true;
    }

    fn read(&self, max_bytes: usize) -> TerminalRead {
        let mut state = lock(&self.state);
        let take = max_bytes.min(state.bytes.len());
        let bytes = state.bytes.drain(..take).collect();
        TerminalRead {
            bytes,
            dropped_bytes: state.dropped,
            ended: state.ended,
        }
    }

    fn ended(&self) -> bool {
        lock(&self.state).ended
    }

    fn counts(&self) -> (usize, u64, bool) {
        let state = lock(&self.state);
        (state.bytes.len(), state.dropped, state.ended)
    }
}

/// The registry-owned live terminal. Every field is owned here, never by a
/// caller, so a dropped consumer handle can never strand the process tree.
struct Session {
    size: Mutex<TerminalSize>,
    backlog: Arc<Backlog>,
    control: tokio::sync::Mutex<SessionControl>,
    cancellation: CancellationToken,
    drain: Mutex<Option<JoinHandle<()>>>,
    wait_result: Mutex<Option<Result<ProcessExit, ProcessErrorCode>>>,
    wait_done: tokio::sync::Notify,
}

struct SessionControl {
    process: Option<ManagedProcess>,
    input: Option<ProcessInput>,
}

impl Session {
    fn status(&self, id: TerminalId) -> TerminalStatus {
        let (pending_bytes, dropped_bytes, output_ended) = self.backlog.counts();
        TerminalStatus {
            id,
            size: *lock(&self.size),
            pending_bytes,
            dropped_bytes,
            output_ended,
        }
    }

    /// Stop the output drain, settling it when an async owner is present.
    ///
    /// The synchronous teardown path can only abort — exactly as
    /// `LocalManagedProcess::drop` does — because a disposer may not await.
    fn take_drain(&self) -> Option<JoinHandle<()>> {
        lock(&self.drain).take()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(drain) = self.take_drain() {
            drain.abort();
        }
    }
}

/// Read the byte-exact terminal stream into the bounded backlog forever.
///
/// This task holds no session reference, so it can never keep a session — and
/// therefore a process tree — alive past the registry.
async fn drain_output(
    mut reader: ProcessOutputReader,
    backlog: Arc<Backlog>,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn crate::ProcessOutputSink>>,
) {
    loop {
        match reader.read_chunk(cancellation.clone()).await {
            Ok(ProcessOutputChunk::Data(bytes)) => {
                if let Some(sink) = &observer {
                    sink.append(crate::OutputStream::Terminal, &bytes);
                }
                backlog.push(&bytes);
            }
            Ok(ProcessOutputChunk::Eof) | Err(_) => {
                backlog.end();
                return;
            }
        }
    }
}

/// One registry slot: the owner's claim on a session, held from admission.
///
/// A slot is reserved before its child starts and counts against the bounds
/// immediately, so admission and reservation share one critical section and the
/// session bounds cannot be exceeded by concurrent openers. A slot becomes
/// addressable only once its live session is installed.
struct Slot {
    owner: TerminalOwner,
    session: Option<Arc<Session>>,
}

struct RegistryState {
    open: bool,
    workspace_paused: bool,
    retiring: usize,
    slots: BTreeMap<TerminalId, Slot>,
}

impl RegistryState {
    fn live(&self, id: &TerminalId, owner: &TerminalOwner) -> Option<&Arc<Session>> {
        self.slots
            .get(id)
            .filter(|slot| &slot.owner == owner)
            .and_then(|slot| slot.session.as_ref())
    }
}

struct Registry {
    subprocess: SubprocessService,
    shutdown: CancellationToken,
    state: Mutex<RegistryState>,
}

impl Registry {
    /// Refuse further work and cancel every live session.
    ///
    /// Runtime-free by construction: it only flips a flag and cancels the one
    /// token every session token descends from, so it is safe as a `Context`
    /// disposer. Cancelling that token closes each session's raw output before
    /// the provider waits on its tree, then cancels the provider's operation
    /// token, whose armed watchdog kills the contained tree. Sessions stay in
    /// the map so a later call reports a stopped service rather than an unknown
    /// terminal; dropping the registry itself reaps them through the provider's
    /// whole-tree kill-on-drop.
    fn close(&self) {
        self.shutdown.cancel();
        lock(&self.state).open = false;
    }
}

struct OpeningReservation {
    registry: Arc<Registry>,
    id: TerminalId,
    committed: bool,
}
impl Drop for OpeningReservation {
    fn drop(&mut self) {
        if !self.committed {
            lock(&self.registry.state).slots.remove(&self.id);
        }
    }
}

/// Owner-scoped registry of persistent terminal sessions.
///
/// # Output retention
///
/// A terminal produces unbounded output, so the registry never buffers without
/// a limit. Each session drains its provider stream continuously — an unread
/// terminal therefore never blocks its child — into a per-session ring of at
/// most [`TerminalSpec::retained_bytes`] bytes
/// ([`DEFAULT_TERMINAL_RETAINED_BYTES`], at most
/// [`MAX_TERMINAL_RETAINED_BYTES`]). At the bound the **oldest** bytes are
/// discarded and counted, so a live terminal always shows its newest output and
/// a reader can always see how much it lost. [`Self::read`] drains what it
/// returns and returns at most [`MAX_TERMINAL_READ_BYTES`] per call, so one
/// consumer result is bounded independently of the retention bound.
#[derive(Clone)]
pub struct TerminalService {
    registry: Arc<Registry>,
}

/// Owned admission pause used while the host swaps workspace authority.
pub struct TerminalWorkspacePause(Arc<Registry>);
impl Drop for TerminalWorkspacePause {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.workspace_paused = false;
        }
    }
}
struct TerminalRetirement(Arc<Registry>);
impl Drop for TerminalRetirement {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.retiring = state.retiring.saturating_sub(1);
        }
    }
}

impl TerminalService {
    /// Atomically refuse new PTYs only when no open, opening or retiring process
    /// owns this registry. Dropping the permit resumes admission.
    pub fn pause_workspace(&self) -> Result<TerminalWorkspacePause, ProcessError> {
        let mut state = self
            .registry
            .state
            .lock()
            .map_err(|_| ProcessError::new(ProcessErrorCode::ServiceStopped))?;
        if !state.open || state.workspace_paused || !state.slots.is_empty() || state.retiring != 0 {
            return Err(ProcessError::new(ProcessErrorCode::TerminalCapacity));
        }
        state.workspace_paused = true;
        Ok(TerminalWorkspacePause(self.registry.clone()))
    }
}

impl TerminalService {
    /// Bind a registry to one resolved subprocess service.
    #[must_use]
    pub fn new(subprocess: SubprocessService) -> Self {
        Self {
            registry: Arc::new(Registry {
                subprocess,
                shutdown: CancellationToken::new(),
                state: Mutex::new(RegistryState {
                    open: true,
                    workspace_paused: false,
                    retiring: 0,
                    slots: BTreeMap::new(),
                }),
            }),
        }
    }

    /// Spawn-free containment facts for the sessions this registry opens.
    #[must_use]
    pub fn containment(&self) -> SubprocessContainment {
        self.registry.subprocess.containment()
    }

    /// Open one persistent terminal owned by `owner`.
    ///
    /// The registry owns the resulting process tree, its lifecycle token and
    /// its output drain; the caller receives only an opaque id.
    ///
    /// # Errors
    /// A stopped registry answers [`ProcessErrorCode::ServiceStopped`], an
    /// owner or registry at its session bound answers
    /// [`ProcessErrorCode::TerminalCapacity`], and launch failures return the
    /// provider's secret-safe [`ProcessError`].
    pub async fn open(
        &self,
        owner: &TerminalOwner,
        spec: TerminalSpec,
    ) -> Result<TerminalId, ProcessError> {
        self.open_cancellable(owner, spec, CancellationToken::new())
            .await
    }

    /// Open a PTY with cancellation during launch; the registry owns it after admission.
    ///
    /// # Errors
    /// Ordinary open errors or caller cancellation before publication.
    pub async fn open_cancellable(
        &self,
        owner: &TerminalOwner,
        spec: TerminalSpec,
        opening: CancellationToken,
    ) -> Result<TerminalId, ProcessError> {
        self.open_with_subprocess(owner, spec, opening, &self.registry.subprocess)
            .await
    }

    /// Launch through a host-rebound subprocess authority, retaining this registry's ownership.
    ///
    /// # Errors
    /// Ordinary terminal launch/admission/cancellation errors.
    pub async fn open_with_subprocess(
        &self,
        owner: &TerminalOwner,
        spec: TerminalSpec,
        opening: CancellationToken,
        subprocess: &SubprocessService,
    ) -> Result<TerminalId, ProcessError> {
        if opening.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let id = self.reserve(owner)?;
        let mut reservation = OpeningReservation {
            registry: self.registry.clone(),
            id: id.clone(),
            committed: false,
        };
        let cancellation = self.registry.shutdown.child_token();
        let launch = subprocess.spawn_terminal(spec.process, spec.size, cancellation.clone());
        tokio::pin!(launch);
        let started = tokio::select! {
            biased;
            ()=opening.cancelled()=>{cancellation.cancel();return Err(ProcessError::new(ProcessErrorCode::Cancelled));}
            result=&mut launch=>result,
        };
        let terminal = match started {
            Ok(terminal) => terminal,
            Err(error) => {
                // A launch that never became a session must return its slot, or
                // failures would permanently consume the owner's budget.
                lock(&self.registry.state).slots.remove(&id);
                return Err(error);
            }
        };
        let (process, input, reader) = terminal.into_raw_parts();
        let backlog = Arc::new(Backlog::new(spec.retained_bytes));
        let drain = tokio::spawn(drain_output(
            reader,
            backlog.clone(),
            cancellation.clone(),
            spec.observer,
        ));
        let session = Arc::new(Session {
            size: Mutex::new(spec.size),
            backlog,
            control: tokio::sync::Mutex::new(SessionControl {
                process: Some(process),
                input: Some(input),
            }),
            cancellation,
            drain: Mutex::new(Some(drain)),
            wait_result: Mutex::new(None),
            wait_done: tokio::sync::Notify::new(),
        });

        let mut uninstalled = Some(session);
        let installed = {
            let mut state = lock(&self.registry.state);
            let open =
                state.open && !self.registry.shutdown.is_cancelled() && !opening.is_cancelled();
            match state.slots.get_mut(&id) {
                Some(slot) if open => {
                    slot.session = uninstalled.take();
                    true
                }
                _ => {
                    state.slots.remove(&id);
                    false
                }
            }
        };
        drop(uninstalled);
        if installed {
            reservation.committed = true;
            Ok(id)
        } else {
            // The registry closed while the child was starting. The uninstalled
            // session drops here, outside the lock: its drain aborts and the
            // provider's kill-on-drop reaps its tree.
            Err(ProcessError::new(ProcessErrorCode::ServiceStopped))
        }
    }

    /// Write exact bytes to the terminal's input.
    ///
    /// # Errors
    /// [`ProcessErrorCode::UnknownTerminal`] for an id this owner does not own,
    /// [`ProcessErrorCode::TerminalExited`] once the terminal ended, and the
    /// provider's failure otherwise.
    pub async fn write(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
        bytes: &[u8],
    ) -> Result<(), ProcessError> {
        let session = self.session(owner, id)?;
        if session.backlog.ended() {
            return Err(ProcessError::new(ProcessErrorCode::TerminalExited));
        }
        let mut control = session.control.lock().await;
        control
            .input
            .as_mut()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::TerminalExited))?
            .write(bytes)
            .await
    }

    /// Drain up to `max_bytes` of retained terminal output.
    ///
    /// Returned bytes are removed from the session's retention ring. The call
    /// never waits on the child: an idle terminal returns an empty read.
    ///
    /// # Errors
    /// [`ProcessErrorCode::UnknownTerminal`] for an id this owner does not own,
    /// and [`ProcessErrorCode::InvalidSpec`] for a zero-byte request.
    pub async fn read(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
        max_bytes: usize,
    ) -> Result<TerminalRead, ProcessError> {
        if max_bytes == 0 {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        let session = self.session(owner, id)?;
        Ok(session.backlog.read(max_bytes.min(MAX_TERMINAL_READ_BYTES)))
    }

    /// Resize the live terminal, delivering the host's window change to the
    /// child.
    ///
    /// # Errors
    /// [`ProcessErrorCode::UnknownTerminal`] for an id this owner does not own,
    /// [`ProcessErrorCode::TerminalExited`] once the terminal ended, and the
    /// provider's failure otherwise. The geometry recorded in
    /// [`Self::list`] changes only after the live resize succeeded.
    pub async fn resize(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
        size: TerminalSize,
    ) -> Result<(), ProcessError> {
        let session = self.session(owner, id)?;
        if session.backlog.ended() {
            return Err(ProcessError::new(ProcessErrorCode::TerminalExited));
        }
        let mut control = session.control.lock().await;
        control
            .process
            .as_mut()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::TerminalExited))?
            .resize_terminal(size)?;
        *lock(&session.size) = size;
        Ok(())
    }

    /// Retire one session: hard-kill its process tree and settle its drain.
    ///
    /// The id is removed before teardown starts, so a second `kill` cannot race
    /// the first and the session is unreachable the moment it is retired.
    ///
    /// # Errors
    /// [`ProcessErrorCode::UnknownTerminal`] for an id this owner does not own,
    /// and the provider's teardown failure otherwise.
    pub async fn kill(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
    ) -> Result<ProcessExit, ProcessError> {
        let (session, _retirement) = self.retire(owner, id)?;
        let mut control = session.control.lock().await;
        // Input stays open across the kill: closing it first would deliver an
        // implicit stdin EOF, letting a well-behaved child exit cleanly and
        // turning a documented hard kill into a hidden graceful shutdown. The
        // writer is released when the retired session drops.
        let process = match control.process.take() {
            Some(process) => process,
            None => {
                drop(control);
                session.cancellation.cancel();
                return wait_for_terminal_waiter(&session).await;
            }
        };
        // Terminal teardown closes the raw receiver before waiting on the tree,
        // which releases a drain blocked on the provider's bounded tee.
        let exit = process.kill().await;
        session.cancellation.cancel();
        if let Some(drain) = session.take_drain() {
            drain.abort();
            let _settled = drain.await;
        }
        exit
    }

    /// Retire one session and wait for its process tree to settle naturally.
    ///
    /// The supplied token controls this wait and the exact terminal operation:
    /// cancellation is forwarded into the process Provider and this method
    /// returns only after that Provider has settled the contained tree. The
    /// registry retains the status row while the wait is active and retires it
    /// only after settlement; the process handle itself is taken exactly once,
    /// so a competing waiter/control path fails rather than racing ownership.
    ///
    /// # Errors
    /// [`ProcessErrorCode::UnknownTerminal`] for an id this owner does not own,
    /// [`ProcessErrorCode::Cancelled`] after caller cancellation has settled
    /// the tree, and the Provider's secret-safe wait failure otherwise.
    pub async fn wait(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
        cancellation: CancellationToken,
    ) -> Result<ProcessExit, ProcessError> {
        let session = self.session(owner, id)?;
        let mut control = session.control.lock().await;
        let process = control
            .process
            .take()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::TerminalExited))?;
        drop(control);
        let wait = process.wait();
        tokio::pin!(wait);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                session.cancellation.cancel();
                wait.await
            }
            result = &mut wait => result,
        };
        session.cancellation.cancel();
        if let Some(drain) = session.take_drain() {
            let _settled = drain.await;
        }
        *lock(&session.wait_result) = Some(
            result
                .as_ref()
                .map(Clone::clone)
                .map_err(ProcessError::code),
        );
        session.wait_done.notify_waiters();
        let mut state = lock(&self.registry.state);
        let same_session = state
            .live(id, owner)
            .is_some_and(|registered| Arc::ptr_eq(registered, &session));
        if same_session {
            state.slots.remove(id);
        }
        result
    }

    /// Body-free status of every session this owner holds, by id.
    ///
    /// A stopped registry, and an owner holding nothing, both list nothing.
    pub async fn list(&self, owner: &TerminalOwner) -> Vec<TerminalStatus> {
        let state = lock(&self.registry.state);
        if !state.open {
            return Vec::new();
        }
        state
            .slots
            .iter()
            .filter(|(_, slot)| &slot.owner == owner)
            .filter_map(|(id, slot)| slot.session.as_ref().map(|s| s.status(id.clone())))
            .collect()
    }

    /// Refuse further work and cancel every live session.
    ///
    /// Idempotent and runtime-free, so it is the exact disposer
    /// [`terminal_registry_plugin`] registers.
    pub fn close(&self) {
        self.registry.close();
    }

    /// Admit one opener and claim its slot in the same critical section.
    fn reserve(&self, owner: &TerminalOwner) -> Result<TerminalId, ProcessError> {
        let mut state = lock(&self.registry.state);
        if !state.open || state.workspace_paused || self.registry.shutdown.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::ServiceStopped));
        }
        if state.slots.len() >= MAX_TERMINAL_SESSIONS {
            return Err(ProcessError::new(ProcessErrorCode::TerminalCapacity));
        }
        let owned = state
            .slots
            .values()
            .filter(|slot| &slot.owner == owner)
            .count();
        if owned >= MAX_TERMINAL_SESSIONS_PER_OWNER {
            return Err(ProcessError::new(ProcessErrorCode::TerminalCapacity));
        }
        let id = TerminalId::generate();
        state.slots.insert(
            id.clone(),
            Slot {
                owner: owner.clone(),
                session: None,
            },
        );
        Ok(id)
    }

    fn session(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
    ) -> Result<Arc<Session>, ProcessError> {
        let state = lock(&self.registry.state);
        if !state.open || self.registry.shutdown.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::ServiceStopped));
        }
        state
            .live(id, owner)
            .cloned()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::UnknownTerminal))
    }

    fn retire(
        &self,
        owner: &TerminalOwner,
        id: &TerminalId,
    ) -> Result<(Arc<Session>, TerminalRetirement), ProcessError> {
        let mut state = lock(&self.registry.state);
        if !state.open || self.registry.shutdown.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::ServiceStopped));
        }
        if state.live(id, owner).is_none() {
            return Err(ProcessError::new(ProcessErrorCode::UnknownTerminal));
        }
        let session = state
            .slots
            .remove(id)
            .and_then(|slot| slot.session)
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::UnknownTerminal))?;
        state.retiring = state.retiring.saturating_add(1);
        Ok((session, TerminalRetirement(self.registry.clone())))
    }
}

impl std::fmt::Debug for TerminalService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.registry.state);
        formatter
            .debug_struct("TerminalService")
            .field("open", &state.open)
            .field("sessions", &state.slots.len())
            .finish_non_exhaustive()
    }
}

async fn wait_for_terminal_waiter(session: &Session) -> Result<ProcessExit, ProcessError> {
    loop {
        let notified = session.wait_done.notified();
        if let Some(result) = lock(&session.wait_result).clone() {
            return result.map_err(ProcessError::new);
        }
        notified.await;
    }
}

/// Publish the owner-scoped persistent terminal registry.
#[must_use]
pub fn terminal_registry_plugin() -> Box<dyn Plugin> {
    struct TerminalRegistryPlugin;

    impl Plugin for TerminalRegistryPlugin {
        fn name(&self) -> &'static str {
            "terminal-registry"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_TERMINAL]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SUBPROCESS]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let subprocess = context
                .get::<SubprocessService>(SERVICE_SUBPROCESS)
                .ok_or_else(|| CoreError::other("subprocess service type mismatch"))?;
            let service = TerminalService::new((*subprocess).clone());
            let disposer = service.clone();
            context.effect(move || disposer.close());
            context.provide(SERVICE_TERMINAL, self.name(), service)
        }
    }

    Box::new(TerminalRegistryPlugin)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn backlog_retains_the_newest_bytes_and_counts_every_discarded_byte() {
        let backlog = Backlog::new(8);
        backlog.push(b"0123456789abcdef");
        let (pending, dropped, ended) = backlog.counts();
        assert_eq!(pending, 8);
        assert_eq!(dropped, 8);
        assert!(!ended);

        let read = backlog.read(64);
        assert_eq!(read.bytes(), b"89abcdef");
        assert_eq!(read.dropped_bytes(), 8);
        assert!(!read.ended());

        backlog.push(b"tail");
        backlog.end();
        let read = backlog.read(2);
        assert_eq!(read.bytes(), b"ta");
        assert!(read.ended());
        assert_eq!(backlog.read(64).bytes(), b"il");
    }

    #[test]
    fn terminal_values_are_validated_newtypes() {
        assert!(TerminalOwner::new("session:01.a-b_c").is_ok());
        assert!(TerminalOwner::new("owner/scope").is_err());
        assert!(TerminalOwner::new("a".repeat(MAX_OWNER_BYTES + 1)).is_err());
        assert!(TerminalId::parse(TerminalId::generate().as_str()).is_ok());
        assert!(TerminalSize::new(1, 1).is_ok());
        assert_eq!(TerminalSize::default().cols(), 80);
    }
}
