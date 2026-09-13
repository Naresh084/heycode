//! PL09 adapter from exact code-plugin frames to the common process owner.

use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle, ThreadId};

use heycode_exec::{
    ExactExecutable, ManagedProcess, ProcessAuthority, ProcessError, ProcessErrorCode,
    ProcessInput, ProcessOutputChunk, ProcessOutputReader, ProcessSpec, SubprocessService,
};
use heycode_extensions::{
    CODE_PLUGIN_MAX_FRAME_BYTES, CodePluginCancellationToken, CodePluginExit, CodePluginLaunchSpec,
    CodePluginLauncher, CodePluginProcess, CodePluginTransportFault, PluginPermission,
};

type ExitListener = Arc<dyn Fn(CodePluginExit) + Send + Sync>;

/// Concrete PL09 launcher over [`SubprocessService`].
///
/// The executable path is used only for its verified package-relative working
/// directory. The child image itself comes from the frozen bytes carried by
/// [`CodePluginLaunchSpec`] and is rebound by `heycode-exec` immediately before
/// the common sandbox launch. The child environment is explicitly empty.
#[derive(Clone)]
pub struct HeycodeExecCodePluginLauncher {
    subprocess: SubprocessService,
}

impl HeycodeExecCodePluginLauncher {
    /// Bind the composed process/sandbox owner.
    #[must_use]
    pub fn new(subprocess: SubprocessService) -> Self {
        Self { subprocess }
    }
}

impl CodePluginLauncher for HeycodeExecCodePluginLauncher {
    fn launch(
        &self,
        spec: &CodePluginLaunchSpec,
        cancellation: &CodePluginCancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        let executable = ExactExecutable::new(
            spec.executable_bytes().to_vec(),
            spec.executable_digest().as_str(),
        )
        .map_err(map_spawn_error)?;
        let cwd = spec
            .entrypoint()
            .parent()
            .ok_or(CodePluginTransportFault::Protocol)?;
        let process = ProcessSpec::new(spec.entrypoint(), cwd)
            .map(ProcessSpec::with_interactive_stdio)
            .map_err(map_spawn_error)?;
        let authority = process_authority(spec.granted_capabilities());
        ManagedCodePluginProcess::spawn(
            self.subprocess.clone(),
            executable,
            process,
            authority,
            cancellation.clone(),
        )
    }
}

fn process_authority(grants: &[PluginPermission]) -> ProcessAuthority {
    ProcessAuthority::new(
        grants.contains(&PluginPermission::FilesystemRead),
        grants.contains(&PluginPermission::FilesystemWrite),
        grants.contains(&PluginPermission::NetworkAccess),
        grants.contains(&PluginPermission::ProcessSpawn),
    )
}

struct DriverCommand {
    request: Vec<u8>,
    cancellation: CodePluginCancellationToken,
    reply: mpsc::SyncSender<Result<Vec<u8>, CodePluginTransportFault>>,
}

struct NotificationState {
    exit: Option<CodePluginExit>,
    listener: Option<ExitListener>,
    listener_installed: bool,
}

struct ProcessShared {
    lifecycle: CodePluginCancellationToken,
    notification: Mutex<NotificationState>,
    driver: Mutex<Option<JoinHandle<()>>>,
    driver_id: Mutex<Option<ThreadId>>,
    shutdown_gate: Mutex<()>,
}

impl ProcessShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            lifecycle: CodePluginCancellationToken::new(),
            notification: Mutex::new(NotificationState {
                exit: None,
                listener: None,
                listener_installed: false,
            }),
            driver: Mutex::new(None),
            driver_id: Mutex::new(None),
            shutdown_gate: Mutex::new(()),
        })
    }

    fn publish_exit(&self, exit: CodePluginExit) {
        let listener = {
            let mut state = lock(&self.notification);
            if state.exit.is_some() {
                return;
            }
            state.exit = Some(exit);
            state.listener.take()
        };
        if let Some(listener) = listener {
            listener(exit);
        }
    }

    fn install_listener(&self, listener: ExitListener) -> Result<(), CodePluginTransportFault> {
        let exit = {
            let mut state = lock(&self.notification);
            if state.listener_installed {
                return Err(CodePluginTransportFault::Protocol);
            }
            state.listener_installed = true;
            match state.exit {
                Some(exit) => Some(exit),
                None => {
                    state.listener = Some(Arc::clone(&listener));
                    None
                }
            }
        };
        if let Some(exit) = exit {
            listener(exit);
        }
        Ok(())
    }

    fn stop_and_join(&self) {
        let _gate = lock(&self.shutdown_gate);
        self.lifecycle.cancel();
        let driver = lock(&self.driver).take();
        if let Some(driver) = driver {
            let current = thread::current().id();
            let is_driver = lock(&self.driver_id).as_ref() == Some(&current);
            if !is_driver {
                let _settled = driver.join();
            }
        }
    }
}

struct ManagedCodePluginProcess {
    sender: tokio::sync::mpsc::UnboundedSender<DriverCommand>,
    shared: Arc<ProcessShared>,
    exchange_gate: Mutex<()>,
}

impl ManagedCodePluginProcess {
    fn spawn(
        subprocess: SubprocessService,
        executable: ExactExecutable,
        spec: ProcessSpec,
        authority: ProcessAuthority,
        cancellation: CodePluginCancellationToken,
    ) -> Result<Arc<dyn CodePluginProcess>, CodePluginTransportFault> {
        let shared = ProcessShared::new();
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let thread_shared = Arc::clone(&shared);
        let driver = thread::Builder::new()
            .name("heycode-code-plugin".to_owned())
            .spawn(move || {
                *lock(&thread_shared.driver_id) = Some(thread::current().id());
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _sent = started_tx.send(Err(CodePluginTransportFault::Spawn));
                        return;
                    }
                };
                let lifecycle = thread_shared.lifecycle.clone();
                let started = runtime.block_on(subprocess.spawn_exact_interactive_raw(
                    executable,
                    spec,
                    authority,
                    Vec::<(OsString, OsString)>::new(),
                    lifecycle.clone(),
                ));
                let raw = match started {
                    Ok(raw) => raw,
                    Err(error) => {
                        let _sent = started_tx.send(Err(map_spawn_error(error)));
                        return;
                    }
                };
                if cancellation.is_cancelled() {
                    lifecycle.cancel();
                }
                if started_tx.send(Ok(())).is_err() {
                    lifecycle.cancel();
                }
                let (process, input, output) = raw.into_raw_parts();
                let exit =
                    runtime.block_on(run_driver(receiver, process, input, output, lifecycle));
                thread_shared.publish_exit(exit);
            })
            .map_err(|_| CodePluginTransportFault::Spawn)?;
        *lock(&shared.driver) = Some(driver);
        match started_rx.recv() {
            Ok(Ok(())) => Ok(Arc::new(Self {
                sender: commands,
                shared,
                exchange_gate: Mutex::new(()),
            })),
            Ok(Err(fault)) => {
                shared.stop_and_join();
                Err(fault)
            }
            Err(_) => {
                shared.stop_and_join();
                Err(CodePluginTransportFault::Spawn)
            }
        }
    }
}

impl CodePluginProcess for ManagedCodePluginProcess {
    fn exchange(
        &self,
        request: &[u8],
        cancellation: &CodePluginCancellationToken,
    ) -> Result<Vec<u8>, CodePluginTransportFault> {
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if request.is_empty() || request.len() > CODE_PLUGIN_MAX_FRAME_BYTES {
            return Err(CodePluginTransportFault::Protocol);
        }
        let _gate = lock(&self.exchange_gate);
        if cancellation.is_cancelled() {
            return Err(CodePluginTransportFault::Cancelled);
        }
        if lock(&self.shared.notification).exit.is_some() {
            return Err(CodePluginTransportFault::Crashed);
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .send(DriverCommand {
                request: request.to_vec(),
                cancellation: cancellation.clone(),
                reply,
            })
            .map_err(|_| CodePluginTransportFault::Crashed)?;
        response
            .recv()
            .unwrap_or(Err(CodePluginTransportFault::Crashed))
    }

    fn set_exit_listener(&self, listener: ExitListener) -> Result<(), CodePluginTransportFault> {
        self.shared.install_listener(listener)
    }

    fn shutdown(&self) {
        self.shared.stop_and_join();
    }
}

impl Drop for ManagedCodePluginProcess {
    fn drop(&mut self) {
        self.shared.stop_and_join();
    }
}

async fn run_driver(
    mut commands: tokio::sync::mpsc::UnboundedReceiver<DriverCommand>,
    process: ManagedProcess,
    mut input: ProcessInput,
    mut output: ProcessOutputReader,
    lifecycle: CodePluginCancellationToken,
) -> CodePluginExit {
    let exit = loop {
        let read_token = lifecycle.child_token();
        let command = {
            let read = output.read_chunk(read_token.clone());
            tokio::pin!(read);
            let command = tokio::select! {
                biased;
                () = lifecycle.cancelled() => break CodePluginExit::Cancelled,
                command = commands.recv() => command,
                chunk = &mut read => {
                    break match chunk {
                        Ok(ProcessOutputChunk::Eof) => CodePluginExit::Exited,
                        Ok(ProcessOutputChunk::Data(_)) | Err(_) => CodePluginExit::Crashed,
                    };
                }
            };
            read_token.cancel();
            let _settled = read.await;
            command
        };
        let Some(command) = command else {
            break CodePluginExit::Cancelled;
        };
        match exchange_frame(&mut input, &mut output, &lifecycle, &command).await {
            Ok(response) => {
                let _sent = command.reply.send(Ok(response));
            }
            Err(fault) => {
                let exit = if fault == CodePluginTransportFault::Cancelled {
                    CodePluginExit::Cancelled
                } else {
                    CodePluginExit::Crashed
                };
                let _sent = command.reply.send(Err(fault));
                break exit;
            }
        }
    };
    lifecycle.cancel();
    drop(output);
    let _settled = process.cancel().await;
    drop(input);
    exit
}

async fn exchange_frame(
    input: &mut ProcessInput,
    output: &mut ProcessOutputReader,
    lifecycle: &CodePluginCancellationToken,
    command: &DriverCommand,
) -> Result<Vec<u8>, CodePluginTransportFault> {
    if command.cancellation.is_cancelled() {
        return Err(CodePluginTransportFault::Cancelled);
    }
    let length =
        u32::try_from(command.request.len()).map_err(|_| CodePluginTransportFault::Protocol)?;
    let mut frame = Vec::with_capacity(4 + command.request.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&command.request);
    let write = input.write(&frame);
    tokio::pin!(write);
    tokio::select! {
        biased;
        () = command.cancellation.cancelled() => {
            lifecycle.cancel();
            let _settled = write.await;
            return Err(CodePluginTransportFault::Cancelled);
        }
        () = lifecycle.cancelled() => {
            let _settled = write.await;
            return Err(CodePluginTransportFault::Cancelled);
        }
        result = &mut write => result.map_err(map_exchange_error)?,
    }
    read_frame(output, lifecycle, &command.cancellation).await
}

async fn read_frame(
    output: &mut ProcessOutputReader,
    lifecycle: &CodePluginCancellationToken,
    cancellation: &CodePluginCancellationToken,
) -> Result<Vec<u8>, CodePluginTransportFault> {
    let mut buffer = Vec::new();
    let mut expected = None;
    loop {
        let read_token = lifecycle.child_token();
        let read = output.read_chunk(read_token.clone());
        tokio::pin!(read);
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                read_token.cancel();
                let _settled = read.await;
                return Err(CodePluginTransportFault::Cancelled);
            }
            () = lifecycle.cancelled() => {
                read_token.cancel();
                let _settled = read.await;
                return Err(CodePluginTransportFault::Cancelled);
            }
            result = &mut read => result.map_err(map_exchange_error)?,
        };
        match chunk {
            ProcessOutputChunk::Eof => return Err(CodePluginTransportFault::Crashed),
            ProcessOutputChunk::Data(bytes) => buffer.extend_from_slice(&bytes),
        }
        if expected.is_none() && buffer.len() >= 4 {
            let prefix = [buffer[0], buffer[1], buffer[2], buffer[3]];
            let length = u32::from_be_bytes(prefix) as usize;
            if length == 0 || length > CODE_PLUGIN_MAX_FRAME_BYTES {
                return Err(CodePluginTransportFault::Protocol);
            }
            expected = Some(length);
        }
        if buffer.len() > CODE_PLUGIN_MAX_FRAME_BYTES + 4 {
            return Err(CodePluginTransportFault::Protocol);
        }
        if let Some(length) = expected
            && buffer.len() >= length + 4
        {
            if buffer.len() != length + 4 {
                return Err(CodePluginTransportFault::Protocol);
            }
            return Ok(buffer.split_off(4));
        }
    }
}

const fn map_spawn_error(error: ProcessError) -> CodePluginTransportFault {
    match error.code() {
        ProcessErrorCode::Cancelled => CodePluginTransportFault::Cancelled,
        ProcessErrorCode::Spawn
        | ProcessErrorCode::NotFound
        | ProcessErrorCode::PermissionDenied => CodePluginTransportFault::Spawn,
        ProcessErrorCode::InvalidSpec | ProcessErrorCode::OutputLimit => {
            CodePluginTransportFault::Protocol
        }
        ProcessErrorCode::ServiceStopped
        | ProcessErrorCode::Unsupported
        | ProcessErrorCode::Sandbox
        | ProcessErrorCode::Teardown
        | ProcessErrorCode::UnknownTerminal
        | ProcessErrorCode::TerminalExited
        | ProcessErrorCode::TerminalCapacity
        | ProcessErrorCode::Io => CodePluginTransportFault::Unavailable,
        _ => CodePluginTransportFault::Unavailable,
    }
}

const fn map_exchange_error(error: ProcessError) -> CodePluginTransportFault {
    match error.code() {
        ProcessErrorCode::Cancelled => CodePluginTransportFault::Cancelled,
        ProcessErrorCode::OutputLimit | ProcessErrorCode::InvalidSpec => {
            CodePluginTransportFault::Protocol
        }
        _ => CodePluginTransportFault::Crashed,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
