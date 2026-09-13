//! Owned raw SDK subprocess connection over `heycode-exec`.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use heycode_exec::{
    ManagedProcess, ProcessError, ProcessErrorCode, ProcessInput, ProcessOutputChunk,
    ProcessOutputReader, ProcessSpec, RawInteractiveProcess, SubprocessService,
};
use heycode_runtime::RuntimeError;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(crate) async fn spawn_sdk_process(
    subprocess: &SubprocessService,
    program: &Path,
    args: &[OsString],
    environment: &[(OsString, OsString)],
    cwd: &Path,
    lifecycle: CancellationToken,
    cancellation: CancellationToken,
) -> Result<Arc<SdkProcess>, RuntimeError> {
    if cancellation.is_cancelled() {
        return Err(RuntimeError::cancelled());
    }
    if lifecycle.is_cancelled() {
        return Err(RuntimeError::closed());
    }
    let spec = ProcessSpec::new(program, cwd)
        .and_then(|spec| spec.with_args(args.iter().cloned()))
        .and_then(|spec| spec.with_environment(environment.iter().cloned()))
        .map(ProcessSpec::with_interactive_stdio)
        .map_err(map_process_error)?;
    let spawn = subprocess.spawn_interactive_raw(spec, lifecycle.clone());
    tokio::pin!(spawn);
    let raw = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            lifecycle.cancel();
            if let Ok(raw) = spawn.await {
                SdkProcess::new(raw, lifecycle).close(CancellationToken::new()).await?;
            }
            return Err(RuntimeError::cancelled());
        }
        () = lifecycle.cancelled() => {
            let _settled = spawn.await;
            return Err(RuntimeError::closed());
        }
        result = &mut spawn => result.map_err(map_process_error)?,
    };
    Ok(SdkProcess::new(raw, lifecycle))
}

pub(crate) struct SdkProcess {
    process: Mutex<Option<ManagedProcess>>,
    input: Mutex<Option<ProcessInput>>,
    output: Mutex<Option<ProcessOutputReader>>,
    close_gate: Mutex<()>,
    lifecycle: CancellationToken,
    closed: AtomicBool,
}

impl SdkProcess {
    fn new(raw: RawInteractiveProcess, lifecycle: CancellationToken) -> Arc<Self> {
        let (process, input, output) = raw.into_raw_parts();
        Arc::new(Self {
            process: Mutex::new(Some(process)),
            input: Mutex::new(Some(input)),
            output: Mutex::new(Some(output)),
            close_gate: Mutex::new(()),
            lifecycle,
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) async fn write(
        &self,
        frame: &[u8],
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if self.closed.load(Ordering::SeqCst) || self.lifecycle.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let mut input = self.input.lock().await;
        let input = input.as_mut().ok_or_else(RuntimeError::closed)?;
        let write = input.write(frame);
        tokio::pin!(write);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(RuntimeError::cancelled()),
            () = self.lifecycle.cancelled() => Err(RuntimeError::closed()),
            result = &mut write => result.map_err(map_process_error),
        }
    }

    pub(crate) async fn read(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        let mut output = self.output.lock().await;
        let output = output.as_mut().ok_or_else(RuntimeError::closed)?;
        let read_cancellation = self.lifecycle.child_token();
        let read = output.read_chunk(read_cancellation.clone());
        tokio::pin!(read);
        let chunk = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                read_cancellation.cancel();
                let _settled = read.await;
                return Err(RuntimeError::cancelled());
            }
            result = &mut read => result.map_err(map_process_error)?,
        };
        match chunk {
            ProcessOutputChunk::Data(bytes) => Ok(Some(bytes)),
            ProcessOutputChunk::Eof => Ok(None),
        }
    }

    pub(crate) async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        let _gate = self.close_gate.lock().await;
        if self.process.lock().await.is_none() {
            return Ok(());
        }
        self.closed.store(true, Ordering::SeqCst);
        self.lifecycle.cancel();
        drop(self.output.lock().await.take());
        let process = self.process.lock().await.take();
        let result = match process {
            Some(process) => process.cancel().await.map_err(map_process_error),
            None => Ok(()),
        };
        drop(self.input.lock().await.take());
        if result.is_ok() && cancellation.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else {
            result
        }
    }
}

fn map_process_error(error: ProcessError) -> RuntimeError {
    match error.code() {
        ProcessErrorCode::Cancelled => RuntimeError::cancelled(),
        ProcessErrorCode::InvalidSpec => RuntimeError::invalid_request(),
        ProcessErrorCode::Unsupported => RuntimeError::unsupported(),
        _ => RuntimeError::unavailable(),
    }
}
