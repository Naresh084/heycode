//! Replaceable subprocess service and managed-handle contract.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    ExactExecutable, ProcessAuthority, ProcessError, ProcessExit, ProcessId, ProcessOutput,
    ProcessSpec, SubprocessContainment, TerminalSize,
};

/// Maximum bytes returned by one raw interactive stdout read.
pub const MAX_PROCESS_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;

/// Stream identity for byte-exact live process observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// PTYs merge both streams.
    Terminal,
}

/// Synchronous bounded output consumer. Implementations must not block or panic.
pub trait ProcessOutputSink: Send + Sync {
    /// Observe bytes before process settlement; stream order is preserved.
    fn append(&self, stream: OutputStream, bytes: &[u8]);
    /// Observe a proven process exit after its streams have settled.
    fn finished(&self, _exit: &ProcessExit) {}
}

/// Provider implementation behind [`SubprocessService`].
#[async_trait]
pub trait SubprocessBackend: Send + Sync {
    /// Resolve a configured program name/path to one exact absolute executable.
    ///
    /// # Errors
    /// Missing, non-executable, or invalid programs fail safely.
    fn resolve_program(
        &self,
        program: &std::ffi::OsStr,
    ) -> Result<std::path::PathBuf, ProcessError>;

    /// Spawn-free containment facts for this provider.
    fn containment(&self) -> SubprocessContainment;

    /// Execute to completion with bounded capture.
    ///
    /// # Errors
    /// Invalid provider state, launch, cancellation, capture, or teardown
    /// failures return a secret-safe [`ProcessError`].
    async fn output(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError>;

    /// Execute with live output. Unsupported backends fail before launch.
    async fn output_streaming(
        &self,
        _spec: ProcessSpec,
        _cancellation: CancellationToken,
        _sink: Arc<dyn ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        Err(ProcessError::new(crate::ProcessErrorCode::Unsupported))
    }

    /// Start one owned process tree.
    ///
    /// # Errors
    /// Invalid provider state, launch, or pre-spawn cancellation returns a
    /// secret-safe [`ProcessError`].
    async fn spawn(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn ManagedProcessHandle>, ProcessError>;

    /// Start one owned process tree with writable stdin and line stdout.
    ///
    /// # Errors
    /// Invalid stdio mode, launch, or pre-spawn cancellation fails safely.
    async fn spawn_interactive(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<InteractiveProcess, ProcessError>;

    /// Start one owned process tree with writable stdin and byte-exact stdout.
    ///
    /// # Errors
    /// Invalid stdio mode, launch, or pre-spawn cancellation fails safely.
    async fn spawn_interactive_raw(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError>;

    /// Start exact admitted executable bytes with explicit environment and
    /// operating-system authority ceilings.
    ///
    /// Providers that cannot bind the admitted bytes or prove that every
    /// exposed authority is within `authority` fail closed.
    ///
    /// # Errors
    /// Unsupported exact binding, authority overexposure, invalid stdio,
    /// launch, or pre-spawn cancellation fails safely.
    async fn spawn_exact_interactive_raw(
        &self,
        executable: ExactExecutable,
        spec: ProcessSpec,
        authority: ProcessAuthority,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        let _ = (executable, spec, authority, environment, cancellation);
        Err(ProcessError::new(crate::ProcessErrorCode::Unsupported))
    }

    /// Start one owned process tree attached to a pseudo-terminal of `size`.
    ///
    /// The returned handle carries the same byte-exact stdout plane as
    /// [`Self::spawn_interactive_raw`], plus a resizable live terminal. A
    /// pseudo-terminal merges the child's stdout and stderr onto one master, and
    /// the provider additionally supplies the child's terminal identity
    /// (`TERM`/`COLUMNS`/`LINES`) on top of the spec's explicit environment.
    ///
    /// # Errors
    /// Backends without a pseudo-terminal primitive answer
    /// [`ProcessErrorCode::Unsupported`]; invalid stdio mode, launch, or
    /// pre-spawn cancellation fails safely.
    async fn spawn_terminal(
        &self,
        spec: ProcessSpec,
        size: TerminalSize,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        let _ = (spec, size, cancellation);
        Err(ProcessError::new(crate::ProcessErrorCode::Unsupported))
    }
}

/// Provider-owned writable process stdin.
#[async_trait]
pub trait ProcessInputHandle: Send {
    /// Write exact bytes.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ProcessError>;

    /// Write UTF-8 text plus the provider's line terminator and flush.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    async fn write_line(&mut self, line: &str) -> Result<(), ProcessError>;

    /// Close stdin and deliver EOF.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    async fn finish(self: Box<Self>) -> Result<(), ProcessError>;
}

/// Provider-owned line-oriented process stdout.
#[async_trait]
pub trait ProcessLineHandle: Send {
    /// Read the next line, or `None` after EOF.
    ///
    /// # Errors
    /// Provider stream failures are secret-safe.
    async fn next_line(&mut self) -> Result<Option<String>, ProcessError>;
}

/// Provider-owned byte-exact process stdout.
#[async_trait]
pub trait ProcessOutputHandle: Send {
    /// Read one bounded raw chunk or explicit EOF.
    ///
    /// Cancellation affects this read only and must not consume bytes or cancel
    /// the process.
    ///
    /// # Errors
    /// Cancellation and provider read failures are secret-safe.
    async fn read_chunk(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutputChunk, ProcessError>;
}

/// Provider-owned live process tree.
///
/// Terminal operations consume the handle, so two wait/kill paths cannot race
/// by construction.
#[async_trait]
pub trait ManagedProcessHandle: Send {
    /// Opaque process identity.
    fn id(&self) -> &ProcessId;

    /// Direct-child operating-system pid when available.
    fn os_pid(&self) -> Option<u32>;

    /// Containment facts attached at spawn.
    fn containment(&self) -> &SubprocessContainment;

    /// Wait for natural, timeout, or signal settlement.
    ///
    /// # Errors
    /// Caller/provider cancellation, I/O, or unconfirmed teardown fails.
    async fn wait(self: Box<Self>) -> Result<ProcessExit, ProcessError>;

    /// Cancel and wait until the process tree is quiescent.
    ///
    /// # Errors
    /// I/O or unconfirmed teardown fails.
    async fn cancel(self: Box<Self>) -> Result<(), ProcessError>;

    /// Gracefully stop the process tree, escalating after `grace`.
    ///
    /// # Errors
    /// Unsupported control, cancellation, I/O, or unconfirmed teardown fails.
    async fn terminate(self: Box<Self>, grace: Duration) -> Result<ProcessExit, ProcessError>;

    /// Hard-kill the process tree and wait for quiescence.
    ///
    /// # Errors
    /// Cancellation, I/O, or unconfirmed teardown fails.
    async fn kill(self: Box<Self>) -> Result<ProcessExit, ProcessError>;

    /// Resize the live pseudo-terminal this handle owns.
    ///
    /// # Errors
    /// Handles without a pseudo-terminal, and terminals whose child already
    /// exited, answer [`ProcessErrorCode::Unsupported`].
    fn resize_terminal(&mut self, size: TerminalSize) -> Result<(), ProcessError> {
        let _ = size;
        Err(ProcessError::new(crate::ProcessErrorCode::Unsupported))
    }
}

/// Typed wrapper around a replaceable subprocess backend.
#[derive(Clone)]
pub struct SubprocessService {
    backend: Arc<dyn SubprocessBackend>,
}

impl SubprocessService {
    /// Bind a backend implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn SubprocessBackend>) -> Self {
        Self { backend }
    }

    /// Construct a standalone local service for embedding and tests.
    /// Returned process handles retain ownership if this service value is dropped.
    /// Shipped worlds use [`crate::local_subprocess_plugin`] so context shutdown
    /// owns cancellation.
    #[must_use]
    pub fn local() -> Self {
        Self::new(Arc::new(crate::local::LocalSubprocessBackend::new(
            crate::SandboxService::standalone_off(),
        )))
    }

    /// Bind a local process backend to an explicitly host-resolved sandbox scope.
    /// The backend cancels its operations when its last service owner is dropped.
    #[must_use]
    pub fn local_with_sandbox(sandbox: crate::SandboxService) -> Self {
        Self::new(Arc::new(crate::local::LocalSubprocessBackend::scoped(
            sandbox,
        )))
    }

    /// Spawn-free containment facts for the active provider.
    #[must_use]
    pub fn containment(&self) -> SubprocessContainment {
        self.backend.containment()
    }

    /// Resolve a configured program name/path to one exact absolute executable.
    ///
    /// # Errors
    /// Missing, non-executable, or invalid programs fail safely.
    pub fn resolve_program(
        &self,
        program: &std::ffi::OsStr,
    ) -> Result<std::path::PathBuf, ProcessError> {
        self.backend.resolve_program(program)
    }

    /// Execute to completion with bounded capture.
    ///
    /// # Errors
    /// Invalid provider state, launch, cancellation, capture, or teardown
    /// failures return a secret-safe [`ProcessError`].
    pub async fn output(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.backend.output(spec, cancellation).await
    }

    /// Execute through the provider with bounded live byte observations.
    ///
    /// # Errors
    /// Unsupported streaming or ordinary execution failure.
    pub async fn output_streaming(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        self.backend
            .output_streaming(spec, cancellation, sink)
            .await
    }

    /// Start one owned process tree.
    ///
    /// # Errors
    /// Invalid provider state, launch, or pre-spawn cancellation returns a
    /// secret-safe [`ProcessError`].
    pub async fn spawn(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ManagedProcess, ProcessError> {
        self.backend
            .spawn(spec, cancellation)
            .await
            .map(ManagedProcess::new)
    }

    /// Start one owned process tree with writable stdin and line stdout.
    ///
    /// # Errors
    /// Invalid stdio mode, launch, or pre-spawn cancellation fails safely.
    pub async fn spawn_interactive(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<InteractiveProcess, ProcessError> {
        self.backend.spawn_interactive(spec, cancellation).await
    }

    /// Start one owned process tree with writable stdin and byte-exact stdout.
    ///
    /// # Errors
    /// Invalid stdio mode, launch, or pre-spawn cancellation fails safely.
    pub async fn spawn_interactive_raw(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        self.backend.spawn_interactive_raw(spec, cancellation).await
    }

    /// Start exact admitted executable bytes through the common sandbox and
    /// process-tree owner.
    ///
    /// The environment argument is the complete child environment. Any
    /// environment already present on `spec` is replaced, so a caller cannot
    /// accidentally inherit an earlier resolution layer's values.
    ///
    /// # Errors
    /// Unsupported exact binding, authority overexposure, invalid stdio,
    /// launch, or pre-spawn cancellation fails safely.
    pub async fn spawn_exact_interactive_raw(
        &self,
        executable: ExactExecutable,
        spec: ProcessSpec,
        authority: ProcessAuthority,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        self.backend
            .spawn_exact_interactive_raw(executable, spec, authority, environment, cancellation)
            .await
    }

    /// Start one owned process tree attached to a pseudo-terminal of `size`.
    ///
    /// # Errors
    /// Backends without a pseudo-terminal primitive answer
    /// [`ProcessErrorCode::Unsupported`]; invalid stdio mode, launch, or
    /// pre-spawn cancellation fails safely.
    pub async fn spawn_terminal(
        &self,
        spec: ProcessSpec,
        size: TerminalSize,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        self.backend.spawn_terminal(spec, size, cancellation).await
    }
}

/// Consumer-facing writable process input.
pub struct ProcessInput {
    inner: Box<dyn ProcessInputHandle>,
}

impl ProcessInput {
    pub(crate) fn new(inner: Box<dyn ProcessInputHandle>) -> Self {
        Self { inner }
    }

    /// Write exact bytes.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        self.inner.write(bytes).await
    }

    /// Write UTF-8 text plus the provider's line terminator and flush.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    pub async fn write_line(&mut self, line: &str) -> Result<(), ProcessError> {
        self.inner.write_line(line).await
    }

    /// Close stdin and deliver EOF.
    ///
    /// # Errors
    /// Closed/broken process input fails safely.
    pub async fn finish(self) -> Result<(), ProcessError> {
        self.inner.finish().await
    }
}

impl std::fmt::Debug for ProcessInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessInput")
            .finish_non_exhaustive()
    }
}

/// Consumer-facing line-oriented process stdout.
pub struct ProcessLines {
    inner: Box<dyn ProcessLineHandle>,
}

impl ProcessLines {
    pub(crate) fn new(inner: Box<dyn ProcessLineHandle>) -> Self {
        Self { inner }
    }

    /// Read the next line, or `None` after EOF.
    ///
    /// # Errors
    /// Provider stream failures are secret-safe.
    pub async fn next_line(&mut self) -> Result<Option<String>, ProcessError> {
        self.inner.next_line().await
    }
}

impl std::fmt::Debug for ProcessLines {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessLines")
            .finish_non_exhaustive()
    }
}

/// One bounded byte-exact stdout observation.
#[derive(Clone, PartialEq, Eq)]
pub enum ProcessOutputChunk {
    /// Non-empty bytes in exact process order.
    Data(Vec<u8>),
    /// The stdout byte stream ended cleanly.
    Eof,
}

impl std::fmt::Debug for ProcessOutputChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data(bytes) => formatter
                .debug_struct("Data")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Eof => formatter.write_str("Eof"),
        }
    }
}

/// Consumer-facing byte-exact process stdout reader.
pub struct ProcessOutputReader {
    inner: Box<dyn ProcessOutputHandle>,
    eof: bool,
}

impl ProcessOutputReader {
    pub(crate) fn new(inner: Box<dyn ProcessOutputHandle>) -> Self {
        Self { inner, eof: false }
    }

    /// Read one byte-exact chunk of at most
    /// [`MAX_PROCESS_OUTPUT_CHUNK_BYTES`] or explicit EOF.
    ///
    /// Cancellation affects only this read. A later call can still observe the
    /// same pending bytes.
    ///
    /// # Errors
    /// Cancellation, provider failure, empty chunks, or oversized provider
    /// output fails safely.
    pub async fn read_chunk(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutputChunk, ProcessError> {
        if self.eof {
            return Ok(ProcessOutputChunk::Eof);
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(crate::ProcessErrorCode::Cancelled));
        }
        let chunk = self.inner.read_chunk(cancellation).await?;
        match &chunk {
            ProcessOutputChunk::Data(bytes)
                if bytes.is_empty() || bytes.len() > MAX_PROCESS_OUTPUT_CHUNK_BYTES =>
            {
                Err(ProcessError::new(crate::ProcessErrorCode::OutputLimit))
            }
            ProcessOutputChunk::Eof => {
                self.eof = true;
                Ok(chunk)
            }
            ProcessOutputChunk::Data(_) => Ok(chunk),
        }
    }
}

impl std::fmt::Debug for ProcessOutputReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessOutputReader")
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

/// One live interactive process split into lifecycle, input and output owners.
pub struct InteractiveProcess {
    process: ManagedProcess,
    input: ProcessInput,
    lines: ProcessLines,
}

impl InteractiveProcess {
    pub(crate) fn new(process: ManagedProcess, input: ProcessInput, lines: ProcessLines) -> Self {
        Self {
            process,
            input,
            lines,
        }
    }

    /// Split the independent lifecycle/input/output handles.
    #[must_use]
    pub fn into_parts(self) -> (ManagedProcess, ProcessInput, ProcessLines) {
        (self.process, self.input, self.lines)
    }
}

impl std::fmt::Debug for InteractiveProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InteractiveProcess")
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

/// One live interactive process split into lifecycle, input and raw-output
/// owners.
pub struct RawInteractiveProcess {
    process: ManagedProcess,
    input: ProcessInput,
    output: ProcessOutputReader,
}

impl RawInteractiveProcess {
    pub(crate) fn new(
        process: ManagedProcess,
        input: ProcessInput,
        output: ProcessOutputReader,
    ) -> Self {
        Self {
            process,
            input,
            output,
        }
    }

    /// Split the independent lifecycle/input/raw-output handles.
    #[must_use]
    pub fn into_raw_parts(self) -> (ManagedProcess, ProcessInput, ProcessOutputReader) {
        (self.process, self.input, self.output)
    }

    #[cfg(unix)]
    pub(crate) fn hold<T>(&mut self, value: T)
    where
        T: Send + Sync + 'static,
    {
        self.process.hold(value);
    }
}

impl std::fmt::Debug for RawInteractiveProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RawInteractiveProcess")
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

/// Consumer-facing live process handle.
pub struct ManagedProcess {
    inner: Box<dyn ManagedProcessHandle>,
    lifetimes: Vec<Box<dyn Send + Sync>>,
}

impl ManagedProcess {
    pub(crate) fn new(inner: Box<dyn ManagedProcessHandle>) -> Self {
        Self {
            inner,
            lifetimes: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn hold<T>(&mut self, value: T)
    where
        T: Send + Sync + 'static,
    {
        self.lifetimes.push(Box::new(value));
    }

    /// Opaque process identity.
    #[must_use]
    pub fn id(&self) -> &ProcessId {
        self.inner.id()
    }

    /// Direct-child operating-system pid when available.
    #[must_use]
    pub fn os_pid(&self) -> Option<u32> {
        self.inner.os_pid()
    }

    /// Containment facts attached at spawn.
    #[must_use]
    pub fn containment(&self) -> &SubprocessContainment {
        self.inner.containment()
    }

    /// Wait for natural, timeout, or signal settlement.
    ///
    /// # Errors
    /// Caller/provider cancellation, I/O, or unconfirmed teardown fails.
    pub async fn wait(self) -> Result<ProcessExit, ProcessError> {
        let Self { inner, lifetimes } = self;
        let result = inner.wait().await;
        drop(lifetimes);
        result
    }

    /// Cancel and wait until the process tree is quiescent.
    ///
    /// # Errors
    /// I/O or unconfirmed teardown fails.
    pub async fn cancel(self) -> Result<(), ProcessError> {
        let Self { inner, lifetimes } = self;
        let result = inner.cancel().await;
        drop(lifetimes);
        result
    }

    /// Gracefully stop the process tree, escalating after `grace`.
    ///
    /// # Errors
    /// Unsupported control, cancellation, I/O, or unconfirmed teardown fails.
    pub async fn terminate(self, grace: Duration) -> Result<ProcessExit, ProcessError> {
        let Self { inner, lifetimes } = self;
        let result = inner.terminate(grace).await;
        drop(lifetimes);
        result
    }

    /// Hard-kill the process tree and wait for quiescence.
    ///
    /// # Errors
    /// Cancellation, I/O, or unconfirmed teardown fails.
    pub async fn kill(self) -> Result<ProcessExit, ProcessError> {
        let Self { inner, lifetimes } = self;
        let result = inner.kill().await;
        drop(lifetimes);
        result
    }

    /// Resize the live pseudo-terminal this handle owns.
    ///
    /// # Errors
    /// Handles without a pseudo-terminal, and terminals whose child already
    /// exited, answer [`ProcessErrorCode::Unsupported`].
    pub fn resize_terminal(&mut self, size: TerminalSize) -> Result<(), ProcessError> {
        self.inner.resize_terminal(size)
    }
}

impl std::fmt::Debug for ManagedProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedProcess")
            .field("id", self.id())
            .field("os_pid", &self.os_pid())
            .field("containment", self.containment())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::VecDeque;

    use super::*;

    enum FakeStep {
        Chunk(ProcessOutputChunk),
        Error(crate::ProcessErrorCode),
    }

    struct FakeOutputHandle(VecDeque<FakeStep>);

    #[async_trait]
    impl ProcessOutputHandle for FakeOutputHandle {
        async fn read_chunk(
            &mut self,
            _cancellation: CancellationToken,
        ) -> Result<ProcessOutputChunk, ProcessError> {
            match self.0.pop_front() {
                Some(FakeStep::Chunk(chunk)) => Ok(chunk),
                Some(FakeStep::Error(code)) => Err(ProcessError::new(code)),
                None => Ok(ProcessOutputChunk::Eof),
            }
        }
    }

    #[tokio::test]
    async fn raw_reader_rejects_provider_overflow_and_empty_chunks() {
        let debug = format!(
            "{:?}",
            ProcessOutputChunk::Data(b"private-raw-output-canary".to_vec())
        );
        assert!(!debug.contains("private-raw-output-canary"));

        let mut oversized = ProcessOutputReader::new(Box::new(FakeOutputHandle(
            [FakeStep::Chunk(ProcessOutputChunk::Data(vec![
                b'x';
                MAX_PROCESS_OUTPUT_CHUNK_BYTES
                    + 1
            ]))]
            .into(),
        )));
        assert_eq!(
            oversized
                .read_chunk(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            crate::ProcessErrorCode::OutputLimit
        );

        let mut empty = ProcessOutputReader::new(Box::new(FakeOutputHandle(
            [FakeStep::Chunk(ProcessOutputChunk::Data(Vec::new()))].into(),
        )));
        assert_eq!(
            empty
                .read_chunk(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            crate::ProcessErrorCode::OutputLimit
        );
    }

    #[tokio::test]
    async fn raw_reader_has_explicit_idempotent_eof_and_retryable_errors() {
        let mut reader = ProcessOutputReader::new(Box::new(FakeOutputHandle(
            [
                FakeStep::Error(crate::ProcessErrorCode::Io),
                FakeStep::Chunk(ProcessOutputChunk::Data(b"ok".to_vec())),
                FakeStep::Chunk(ProcessOutputChunk::Eof),
                FakeStep::Chunk(ProcessOutputChunk::Data(b"not-observable".to_vec())),
            ]
            .into(),
        )));
        assert_eq!(
            reader
                .read_chunk(CancellationToken::new())
                .await
                .unwrap_err()
                .code(),
            crate::ProcessErrorCode::Io
        );
        assert_eq!(
            reader.read_chunk(CancellationToken::new()).await.unwrap(),
            ProcessOutputChunk::Data(b"ok".to_vec())
        );
        assert_eq!(
            reader.read_chunk(CancellationToken::new()).await.unwrap(),
            ProcessOutputChunk::Eof
        );
        assert_eq!(
            reader.read_chunk(CancellationToken::new()).await.unwrap(),
            ProcessOutputChunk::Eof
        );
    }
}
