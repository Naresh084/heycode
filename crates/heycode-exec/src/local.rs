//! Local processkit-backed subprocess provider.

use std::io::{Error as IoError, ErrorKind};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use processkit::{OutputBufferPolicy, OverflowMode};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::PollSender;

use crate::{
    ExactExecutable, InteractiveProcess, MAX_PROCESS_OUTPUT_CHUNK_BYTES, ManagedProcess,
    ManagedProcessHandle, OutputOverflowPolicy, ProcessAuthority, ProcessError, ProcessErrorCode,
    ProcessExit, ProcessId, ProcessInput, ProcessInputHandle, ProcessLineHandle, ProcessLines,
    ProcessOutput, ProcessOutputChunk, ProcessOutputHandle, ProcessOutputReader, ProcessSpec,
    RawInteractiveProcess, SandboxService, SubprocessBackend, SubprocessContainment, TerminalSize,
};

const RAW_OUTPUT_CHANNEL_CAPACITY: usize = 8;

pub(crate) struct LocalSubprocessBackend {
    shutdown: CancellationToken,
    // Scoped workspaces own returned handles; ordinary services transfer ownership.
    cancel_on_drop: bool,
    containment: SubprocessContainment,
    sandbox: SandboxService,
}

impl Drop for LocalSubprocessBackend {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            self.shutdown.cancel();
        }
    }
}

impl LocalSubprocessBackend {
    pub(crate) fn new(sandbox: SandboxService) -> Self {
        Self {
            shutdown: CancellationToken::new(),
            cancel_on_drop: false,
            containment: SubprocessContainment::from_processkit(),
            sandbox,
        }
    }

    /// An explicit workspace owner also bounds every returned process handle.
    pub(crate) fn scoped(sandbox: SandboxService) -> Self {
        let mut backend = Self::new(sandbox);
        backend.cancel_on_drop = true;
        backend
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    fn operation_token(&self) -> Result<CancellationToken, ProcessError> {
        if self.shutdown.is_cancelled() {
            Err(ProcessError::new(ProcessErrorCode::ServiceStopped))
        } else {
            Ok(self.shutdown.child_token())
        }
    }

    fn command(spec: &ProcessSpec, cancellation: CancellationToken) -> processkit::Command {
        let overflow = match spec.output_overflow_policy() {
            OutputOverflowPolicy::Error => OverflowMode::Error,
            OutputOverflowPolicy::Tail => OverflowMode::DropOldest,
        };
        let buffer = OutputBufferPolicy::bounded(usize::MAX)
            .with_max_bytes(spec.output_limit_bytes())
            .with_overflow(overflow);
        let mut command = processkit::Command::new(spec.program())
            .args(spec.args())
            .current_dir(spec.cwd())
            .env_clear()
            .envs(spec.environment().iter().map(|(key, value)| (key, value)))
            .output_buffer(buffer)
            .cancel_on(cancellation);
        if let Some(timeout) = spec.timeout() {
            command = command.timeout(timeout);
        }
        if spec.interactive_stdio() {
            command = command.keep_stdin_open();
        }
        command
    }

    fn raw_command(
        spec: &ProcessSpec,
        cancellation: CancellationToken,
        writer: RawTeeWriter,
    ) -> processkit::Command {
        let decoded_drain = OutputBufferPolicy::bounded(1)
            .with_max_bytes(MAX_PROCESS_OUTPUT_CHUNK_BYTES)
            .with_overflow(OverflowMode::DropOldest);
        Self::command(spec, cancellation)
            .output_buffer(decoded_drain)
            .stdout_raw_tee(writer)
    }

    /// Start a raw-tee'd process tree, optionally over a pseudo-terminal.
    ///
    /// The terminal and pipe paths differ only in the launch primitive; every
    /// containment, cancellation and raw-output ownership fact below is shared.
    async fn start_raw(
        &self,
        spec: ProcessSpec,
        size: Option<TerminalSize>,
        authority: Option<ProcessAuthority>,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        if !spec.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let operation = self.operation_token()?;
        let spec = match authority {
            Some(authority) => self.sandbox.confine_exact(spec, authority)?,
            None => self.sandbox.confine(spec)?,
        };
        let (sender, receiver) = mpsc::channel(RAW_OUTPUT_CHANNEL_CAPACITY);
        let raw_output = Arc::new(RawOutputState::new(receiver));
        let mut command = Self::raw_command(&spec, operation.clone(), RawTeeWriter::new(sender));
        if let Some(size) = size {
            command = command.use_pty().pty_size(size.cols(), size.rows());
        }
        let mut running = command.start().await.map_err(ProcessError::from)?;
        if !running.kills_tree_on_drop() {
            return Err(ProcessError::new(ProcessErrorCode::Unsupported));
        }
        let pid = running.pid();
        let stdin = running
            .take_stdin()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Io))?;
        let mut lines = running.stdout_lines().map_err(ProcessError::from)?;
        let output_drain = tokio::spawn(async move { while lines.next().await.is_some() {} });
        let relay_operation = operation.clone();
        let relay_raw_output = raw_output.clone();
        let relay = tokio::spawn(async move {
            tokio::select! {
                () = cancellation.cancelled() => {
                    relay_raw_output.close().await;
                    relay_operation.cancel();
                }
                () = relay_operation.cancelled() => {
                    relay_raw_output.close().await;
                }
            }
        });
        let process = ManagedProcess::new(Box::new(LocalManagedProcess {
            id: ProcessId::generate(),
            pid,
            containment: self.containment.clone(),
            operation,
            relay: Some(relay),
            output_drain: Some(output_drain),
            raw_output: Some(raw_output.clone()),
            process: Some(running),
        }));
        Ok(RawInteractiveProcess::new(
            process,
            ProcessInput::new(Box::new(LocalProcessInput(stdin))),
            ProcessOutputReader::new(Box::new(LocalProcessOutput::new(raw_output))),
        ))
    }
}

#[async_trait]
impl SubprocessBackend for LocalSubprocessBackend {
    fn resolve_program(
        &self,
        program: &std::ffi::OsStr,
    ) -> Result<std::path::PathBuf, ProcessError> {
        processkit::which(program).map_err(ProcessError::from)
    }

    fn containment(&self) -> SubprocessContainment {
        self.containment.clone()
    }

    async fn output(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        if spec.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let operation = self.operation_token()?;
        let spec = self.sandbox.confine(spec)?;
        let command = Self::command(&spec, operation.clone());
        let future = command.output_bytes();
        tokio::pin!(future);
        let result = tokio::select! {
            result = &mut future => result,
            () = cancellation.cancelled() => {
                operation.cancel();
                future.await
            }
        }
        .map_err(ProcessError::from)?;
        let exit = crate::model::map_outcome(result.outcome())?;
        let stderr = result.stderr().to_owned();
        let duration = result.duration();
        let truncated = result.truncated();
        let stdout = result.into_stdout();
        Ok(ProcessOutput::new(
            exit, stdout, stderr, duration, truncated,
        ))
    }

    async fn output_streaming(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
        sink: Arc<dyn crate::ProcessOutputSink>,
    ) -> Result<ProcessOutput, ProcessError> {
        if spec.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let operation = self.operation_token()?;
        let spec = self.sandbox.confine(spec)?;
        let capture = Arc::new(StreamingCapture {
            sink,
            capacity: spec.output_limit_bytes(),
            streams: std::sync::Mutex::new(Default::default()),
        });
        let command = Self::command(&spec, operation.clone())
            .stdout_raw_tee(ObservedWriter(capture.clone(), crate::OutputStream::Stdout))
            .stderr_raw_tee(ObservedWriter(capture.clone(), crate::OutputStream::Stderr));
        let future = command.output_string();
        tokio::pin!(future);
        let result = tokio::select! {
            result = &mut future => result,
            () = cancellation.cancelled() => {
                operation.cancel();
                future.await
            }
        }
        .map_err(ProcessError::from)?;
        let exit = crate::model::map_outcome(result.outcome())?;
        capture.sink.finished(&exit);
        let duration = result.duration();
        let streams = capture.streams.lock().unwrap_or_else(|e| e.into_inner());
        let stdout = streams.0.iter().copied().collect();
        let stderr =
            String::from_utf8_lossy(&streams.1.iter().copied().collect::<Vec<_>>()).into_owned();
        let truncated = streams.2 || result.truncated();
        Ok(ProcessOutput::new(
            exit, stdout, stderr, duration, truncated,
        ))
    }

    async fn spawn(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn ManagedProcessHandle>, ProcessError> {
        if spec.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let operation = self.operation_token()?;
        let spec = self.sandbox.confine(spec)?;
        let command = Self::command(&spec, operation.clone());
        let process = command.start().await.map_err(ProcessError::from)?;
        if !process.kills_tree_on_drop() {
            return Err(ProcessError::new(ProcessErrorCode::Unsupported));
        }
        let pid = process.pid();
        let relay_operation = operation.clone();
        let relay = tokio::spawn(async move {
            cancellation.cancelled().await;
            relay_operation.cancel();
        });
        Ok(Box::new(LocalManagedProcess {
            id: ProcessId::generate(),
            pid,
            containment: self.containment.clone(),
            operation,
            relay: Some(relay),
            output_drain: None,
            raw_output: None,
            process: Some(process),
        }))
    }

    async fn spawn_interactive(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<InteractiveProcess, ProcessError> {
        if !spec.interactive_stdio() {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::new(ProcessErrorCode::Cancelled));
        }
        let operation = self.operation_token()?;
        let spec = self.sandbox.confine(spec)?;
        let command = Self::command(&spec, operation.clone());
        let mut running = command.start().await.map_err(ProcessError::from)?;
        if !running.kills_tree_on_drop() {
            return Err(ProcessError::new(ProcessErrorCode::Unsupported));
        }
        let pid = running.pid();
        let stdin = running
            .take_stdin()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Io))?;
        let lines = running.stdout_lines().map_err(ProcessError::from)?;
        let relay_operation = operation.clone();
        let relay = tokio::spawn(async move {
            cancellation.cancelled().await;
            relay_operation.cancel();
        });
        let process = ManagedProcess::new(Box::new(LocalManagedProcess {
            id: ProcessId::generate(),
            pid,
            containment: self.containment.clone(),
            operation,
            relay: Some(relay),
            output_drain: None,
            raw_output: None,
            process: Some(running),
        }));
        Ok(InteractiveProcess::new(
            process,
            ProcessInput::new(Box::new(LocalProcessInput(stdin))),
            ProcessLines::new(Box::new(LocalProcessLines(lines))),
        ))
    }

    async fn spawn_interactive_raw(
        &self,
        spec: ProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        self.start_raw(spec, None, None, cancellation).await
    }

    async fn spawn_exact_interactive_raw(
        &self,
        executable: ExactExecutable,
        spec: ProcessSpec,
        authority: ProcessAuthority,
        environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        #[cfg(unix)]
        {
            if cancellation.is_cancelled() {
                return Err(ProcessError::new(ProcessErrorCode::Cancelled));
            }
            let staged = StagedExecutable::new(&executable)?;
            let mut argv = spec.launch_argv();
            let Some(program) = argv.first_mut() else {
                return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
            };
            *program = staged.path().as_os_str().to_os_string();
            let spec = spec.with_launch_argv(argv)?.with_environment(environment)?;
            let mut result = self
                .start_raw(spec, None, Some(authority), cancellation)
                .await?;
            result.hold(staged);
            Ok(result)
        }
        #[cfg(not(unix))]
        {
            let _ = (executable, spec, authority, environment, cancellation);
            Err(ProcessError::new(ProcessErrorCode::Unsupported))
        }
    }

    async fn spawn_terminal(
        &self,
        spec: ProcessSpec,
        size: TerminalSize,
        cancellation: CancellationToken,
    ) -> Result<RawInteractiveProcess, ProcessError> {
        self.start_raw(spec, Some(size), None, cancellation).await
    }
}

#[cfg(unix)]
struct StagedExecutable {
    directory: PathBuf,
    path: PathBuf,
}

#[cfg(unix)]
impl StagedExecutable {
    fn new(executable: &ExactExecutable) -> Result<Self, ProcessError> {
        use sha2::Digest as _;
        use std::fs::{DirBuilder, File, OpenOptions, Permissions};
        use std::io::{Read as _, Write as _};
        use std::os::unix::fs::{
            DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
        };

        if !is_native_executable(executable.bytes()) {
            return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
        }
        let parent = std::fs::canonicalize(std::env::temp_dir())
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
        let directory = (0..8)
            .find_map(|_| {
                let candidate = parent.join(format!("heycode-exact-{}", uuid::Uuid::new_v4()));
                match DirBuilder::new().mode(0o700).create(&candidate) {
                    Ok(()) => Some(Ok(candidate)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(_) => Some(Err(ProcessError::new(ProcessErrorCode::Io))),
                }
            })
            .transpose()?
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Io))?;
        let staged = Self {
            path: directory.join("image"),
            directory,
        };
        std::fs::set_permissions(&staged.directory, Permissions::from_mode(0o700))
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o700)
            .open(&staged.path)
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
        let publication = (|| {
            file.write_all(executable.bytes())
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            file.sync_all()
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            file.set_permissions(Permissions::from_mode(0o700))
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            drop(file);
            File::open(&staged.directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            let metadata = std::fs::symlink_metadata(&staged.path)
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != executable.bytes().len() as u64
                || metadata.nlink() != 1
                || metadata.mode() & 0o777 != 0o700
            {
                return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
            }
            let mut verifier =
                File::open(&staged.path).map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            let mut bytes = Vec::with_capacity(executable.bytes().len());
            verifier
                .read_to_end(&mut bytes)
                .map_err(|_| ProcessError::new(ProcessErrorCode::Io))?;
            let digest: [u8; 32] = sha2::Sha256::digest(&bytes).into();
            if bytes.as_slice() != executable.bytes() || &digest != executable.digest() {
                return Err(ProcessError::new(ProcessErrorCode::InvalidSpec));
            }
            Ok(())
        })();
        publication?;
        Ok(staged)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for StagedExecutable {
    fn drop(&mut self) {
        let _removed_file = std::fs::remove_file(&self.path);
        let _removed_directory = std::fs::remove_dir(&self.directory);
    }
}

#[cfg(unix)]
fn is_native_executable(bytes: &[u8]) -> bool {
    let Some(magic) = bytes.get(..4) else {
        return false;
    };
    magic == b"\x7fELF"
        || matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
}

struct LocalProcessInput(processkit::ProcessStdin);

#[async_trait]
impl ProcessInputHandle for LocalProcessInput {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        self.0
            .write(bytes)
            .await
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))
    }

    async fn write_line(&mut self, line: &str) -> Result<(), ProcessError> {
        self.0
            .write_line(line)
            .await
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))
    }

    async fn finish(self: Box<Self>) -> Result<(), ProcessError> {
        self.0
            .finish()
            .await
            .map_err(|_| ProcessError::new(ProcessErrorCode::Io))
    }
}

struct LocalProcessLines(processkit::StdoutLines);

#[async_trait]
impl ProcessLineHandle for LocalProcessLines {
    async fn next_line(&mut self) -> Result<Option<String>, ProcessError> {
        Ok(self.0.next().await)
    }
}

enum RawOutputMessage {
    Data(Vec<u8>),
    Eof,
}

struct RawTeeWriter {
    sender: PollSender<RawOutputMessage>,
    eof_sent: bool,
}

impl RawTeeWriter {
    fn new(sender: mpsc::Sender<RawOutputMessage>) -> Self {
        Self {
            sender: PollSender::new(sender),
            eof_sent: false,
        }
    }

    fn send_reserved(&mut self, message: RawOutputMessage) -> std::io::Result<()> {
        self.sender
            .send_item(message)
            .map_err(|_| IoError::new(ErrorKind::BrokenPipe, "raw process output consumer closed"))
    }
}

impl AsyncWrite for RawTeeWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.eof_sent {
            return Poll::Ready(Err(IoError::new(
                ErrorKind::BrokenPipe,
                "raw process output already ended",
            )));
        }
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        ready!(self.sender.poll_reserve(context)).map_err(|_| {
            IoError::new(ErrorKind::BrokenPipe, "raw process output consumer closed")
        })?;
        let length = bytes.len().min(MAX_PROCESS_OUTPUT_CHUNK_BYTES);
        self.send_reserved(RawOutputMessage::Data(bytes[..length].to_vec()))?;
        Poll::Ready(Ok(length))
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.eof_sent {
            return Poll::Ready(Ok(()));
        }
        ready!(self.sender.poll_reserve(context)).map_err(|_| {
            IoError::new(ErrorKind::BrokenPipe, "raw process output consumer closed")
        })?;
        self.send_reserved(RawOutputMessage::Eof)?;
        self.eof_sent = true;
        self.sender.close();
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(context)
    }
}

struct RawOutputState {
    receiver: tokio::sync::Mutex<mpsc::Receiver<RawOutputMessage>>,
    shutdown: CancellationToken,
}

impl RawOutputState {
    fn new(receiver: mpsc::Receiver<RawOutputMessage>) -> Self {
        Self {
            receiver: tokio::sync::Mutex::new(receiver),
            shutdown: CancellationToken::new(),
        }
    }

    async fn close(&self) {
        self.shutdown.cancel();
        self.receiver.lock().await.close();
    }
}

struct LocalProcessOutput {
    state: Arc<RawOutputState>,
    eof: bool,
}

impl LocalProcessOutput {
    fn new(state: Arc<RawOutputState>) -> Self {
        Self { state, eof: false }
    }
}

impl Drop for LocalProcessOutput {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
        if let Ok(mut receiver) = self.state.receiver.try_lock() {
            receiver.close();
        }
    }
}

#[async_trait]
impl ProcessOutputHandle for LocalProcessOutput {
    async fn read_chunk(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<ProcessOutputChunk, ProcessError> {
        if self.eof {
            return Ok(ProcessOutputChunk::Eof);
        }
        let mut receiver = self.state.receiver.lock().await;
        let message = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(ProcessError::new(ProcessErrorCode::Cancelled));
            }
            () = self.state.shutdown.cancelled() => {
                return Err(ProcessError::new(ProcessErrorCode::Cancelled));
            }
            message = receiver.recv() => message,
        };
        match message {
            Some(RawOutputMessage::Data(bytes)) => Ok(ProcessOutputChunk::Data(bytes)),
            Some(RawOutputMessage::Eof) => {
                self.eof = true;
                Ok(ProcessOutputChunk::Eof)
            }
            None => Err(ProcessError::new(ProcessErrorCode::Io)),
        }
    }
}

struct LocalManagedProcess {
    id: ProcessId,
    pid: Option<u32>,
    containment: SubprocessContainment,
    operation: CancellationToken,
    relay: Option<JoinHandle<()>>,
    output_drain: Option<JoinHandle<()>>,
    raw_output: Option<Arc<RawOutputState>>,
    process: Option<processkit::RunningProcess>,
}

impl LocalManagedProcess {
    fn take_process(&mut self) -> Result<processkit::RunningProcess, ProcessError> {
        self.process
            .take()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Io))
    }

    fn abort_relay(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.abort();
        }
    }

    async fn settle_relay(&mut self) {
        if let Some(relay) = self.relay.take() {
            relay.abort();
            let _settled = relay.await;
        }
    }

    fn abort_output_drain(&mut self) {
        if let Some(output_drain) = self.output_drain.take() {
            output_drain.abort();
        }
    }

    async fn settle_output_drain(&mut self) {
        if let Some(output_drain) = self.output_drain.take() {
            let _settled = output_drain.await;
        }
    }

    async fn close_raw_output(&mut self) {
        if let Some(raw_output) = self.raw_output.take() {
            raw_output.close().await;
        }
    }
}

impl Drop for LocalManagedProcess {
    fn drop(&mut self) {
        self.operation.cancel();
        self.abort_relay();
        self.abort_output_drain();
        if let Some(raw_output) = self.raw_output.take() {
            raw_output.shutdown.cancel();
        }
    }
}

#[async_trait]
impl ManagedProcessHandle for LocalManagedProcess {
    fn id(&self) -> &ProcessId {
        &self.id
    }

    fn os_pid(&self) -> Option<u32> {
        self.pid
    }

    fn containment(&self) -> &SubprocessContainment {
        &self.containment
    }

    async fn wait(mut self: Box<Self>) -> Result<ProcessExit, ProcessError> {
        let process = self.take_process()?;
        let outcome = process.wait().await.map_err(ProcessError::from);
        self.settle_relay().await;
        self.settle_output_drain().await;
        crate::model::map_outcome(outcome?)
    }

    async fn cancel(mut self: Box<Self>) -> Result<(), ProcessError> {
        let process = self.take_process()?;
        self.close_raw_output().await;
        self.operation.cancel();
        let result = process.wait().await;
        self.settle_relay().await;
        self.settle_output_drain().await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(ProcessError::from(error)),
        }
    }

    async fn terminate(mut self: Box<Self>, grace: Duration) -> Result<ProcessExit, ProcessError> {
        let process = self.take_process()?;
        self.close_raw_output().await;
        self.settle_relay().await;
        let outcome = process.shutdown(grace).await.map_err(ProcessError::from);
        self.settle_output_drain().await;
        crate::model::map_outcome(outcome?)
    }

    fn resize_terminal(&mut self, size: TerminalSize) -> Result<(), ProcessError> {
        self.process
            .as_mut()
            .ok_or_else(|| ProcessError::new(ProcessErrorCode::Unsupported))?
            .resize_pty(size.cols(), size.rows())
            .map_err(ProcessError::from)
    }

    async fn kill(mut self: Box<Self>) -> Result<ProcessExit, ProcessError> {
        let process = self.take_process()?;
        self.close_raw_output().await;
        self.settle_relay().await;
        let outcome = process
            .shutdown(Duration::ZERO)
            .await
            .map_err(ProcessError::from);
        self.settle_output_drain().await;
        crate::model::map_outcome(outcome?)
    }
}

pub(crate) fn backend(sandbox: SandboxService) -> Arc<LocalSubprocessBackend> {
    Arc::new(LocalSubprocessBackend::new(sandbox))
}

struct ObservedWriter(Arc<dyn crate::ProcessOutputSink>, crate::OutputStream);
impl AsyncWrite for ObservedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let count = bytes.len().min(MAX_PROCESS_OUTPUT_CHUNK_BYTES);
        self.0.append(self.1, &bytes[..count]);
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct StreamingCapture {
    sink: Arc<dyn crate::ProcessOutputSink>,
    capacity: usize,
    streams: std::sync::Mutex<(
        std::collections::VecDeque<u8>,
        std::collections::VecDeque<u8>,
        bool,
    )>,
}
impl crate::ProcessOutputSink for StreamingCapture {
    fn append(&self, stream: crate::OutputStream, bytes: &[u8]) {
        self.sink.append(stream, bytes);
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        let target = if stream == crate::OutputStream::Stderr {
            &mut streams.1
        } else {
            &mut streams.0
        };
        target.extend(bytes);
        let excess = target.len().saturating_sub(self.capacity);
        target.drain(..excess);
        streams.2 |= excess > 0;
    }
}
