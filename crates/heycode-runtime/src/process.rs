//! `heycode-exec` adapter for the transport-neutral ACP process contract.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{AcpProcess, AcpProcessFactory, AcpProcessSpec, RuntimeError};
use async_trait::async_trait;
use heycode_exec::{
    ManagedProcess, ProcessError, ProcessErrorCode, ProcessInput, ProcessOutputChunk,
    ProcessOutputReader, ProcessSpec, RawInteractiveProcess, SubprocessService,
};
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const VERSION_OUTPUT_LIMIT: usize = 4096;
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

/// Version-pinned, executable-bound ACP transport over the composed subprocess service.
pub struct ManagedAcpProcessFactory {
    subprocess: SubprocessService,
    program: PathBuf,
    executable_digest: [u8; 32],
    version: String,
    arguments: Vec<OsString>,
}

impl ManagedAcpProcessFactory {
    /// Bind a provider-owned executable, exact version output and ACP arguments.
    ///
    /// # Errors
    /// Unreadable/oversized executables, invalid version text or argv fail.
    pub fn new(
        subprocess: SubprocessService,
        program: PathBuf,
        version: impl Into<String>,
        arguments: Vec<OsString>,
    ) -> Result<Self, RuntimeError> {
        let version = version.into();
        if version.is_empty()
            || version.len() > 128
            || version.trim() != version
            || version.chars().any(char::is_control)
            || arguments.len() > 128
            || arguments
                .iter()
                .any(|arg| arg.to_string_lossy().contains('\0'))
        {
            return Err(RuntimeError::invalid_request());
        }
        let executable_digest = hash_executable(&program)?;
        Ok(Self {
            subprocess,
            program,
            executable_digest,
            version,
            arguments,
        })
    }

    async fn verify_version(
        &self,
        spec: &AcpProcessSpec,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let version = ProcessSpec::new(&self.program, spec.cwd())
            .and_then(|spec| spec.with_args([OsStr::new("--version")]))
            .and_then(|process| process.with_environment(spec.environment().iter().cloned()))
            .and_then(|spec| spec.with_timeout(Some(VERSION_TIMEOUT)))
            .and_then(|spec| spec.with_output_limit_bytes(VERSION_OUTPUT_LIMIT));
        let version = match version {
            Ok(version) => version,
            Err(error) => return Err(map_process_error(error)),
        };
        let output = self
            .subprocess
            .output(version, cancellation)
            .await
            .map_err(map_process_error)?;
        let expected_lf = format!("{}\n", self.version);
        let expected_crlf = format!("{}\r\n", self.version);
        let exact = output.stdout() == self.version.as_bytes()
            || output.stdout() == expected_lf.as_bytes()
            || output.stdout() == expected_crlf.as_bytes();
        if !output.exit().is_success()
            || output.truncated()
            || !output.stderr().is_empty()
            || !exact
        {
            return Err(RuntimeError::protocol());
        }
        Ok(())
    }
}

#[async_trait]
impl AcpProcessFactory for ManagedAcpProcessFactory {
    async fn spawn(
        &self,
        spec: AcpProcessSpec,
        lifecycle: CancellationToken,
        cancellation: CancellationToken,
    ) -> Result<Arc<dyn AcpProcess>, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        if lifecycle.is_cancelled() {
            return Err(RuntimeError::closed());
        }
        if spec.program() != self.program || spec.args() != self.arguments {
            return Err(RuntimeError::invalid_request());
        }
        self.verify_executable()?;
        let probe_cancellation = CancellationToken::new();
        let verify = self.verify_version(&spec, probe_cancellation.clone());
        tokio::pin!(verify);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                probe_cancellation.cancel();
                let _settled = verify.await;
                return Err(RuntimeError::cancelled());
            }
            () = lifecycle.cancelled() => {
                probe_cancellation.cancel();
                let _settled = verify.await;
                return Err(RuntimeError::closed());
            }
            result = &mut verify => result?,
        }
        self.verify_executable()?;
        let launch = ProcessSpec::new(spec.program(), spec.cwd())
            .and_then(|launch| launch.with_args(spec.args().iter().cloned()))
            .and_then(|launch| launch.with_environment(spec.environment().iter().cloned()))
            .map(ProcessSpec::with_interactive_stdio)
            .map_err(map_process_error)?;
        let spawn = self
            .subprocess
            .spawn_interactive_raw(launch, lifecycle.clone());
        tokio::pin!(spawn);
        let raw = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                lifecycle.cancel();
                match spawn.await {
                    Ok(raw) => {
                        ManagedAcpProcess::new(raw, lifecycle).close(CancellationToken::new()).await?;
                    }
                    Err(_error) => {}
                }
                return Err(RuntimeError::cancelled());
            }
            () = lifecycle.cancelled() => {
                let _settled = spawn.await;
                return Err(RuntimeError::closed());
            }
            result = &mut spawn => result.map_err(map_process_error)?,
        };
        Ok(ManagedAcpProcess::new(raw, lifecycle))
    }
}

impl ManagedAcpProcessFactory {
    fn verify_executable(&self) -> Result<(), RuntimeError> {
        if hash_executable(&self.program)? == self.executable_digest {
            Ok(())
        } else {
            Err(RuntimeError::protocol())
        }
    }
}

struct ManagedAcpProcess {
    process: Mutex<Option<ManagedProcess>>,
    input: Mutex<Option<ProcessInput>>,
    output: Mutex<Option<ProcessOutputReader>>,
    close_gate: Mutex<()>,
    lifecycle: CancellationToken,
    closed: AtomicBool,
}

impl ManagedAcpProcess {
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
}

#[async_trait]
impl AcpProcess for ManagedAcpProcess {
    async fn write(
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

    async fn read(&self, cancellation: CancellationToken) -> Result<Option<Vec<u8>>, RuntimeError> {
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

    async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
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

fn hash_executable(path: &std::path::Path) -> Result<[u8; 32], RuntimeError> {
    let metadata = std::fs::metadata(path).map_err(|_| RuntimeError::unavailable())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_EXECUTABLE_BYTES {
        return Err(RuntimeError::unavailable());
    }
    let mut file = File::open(path).map_err(|_| RuntimeError::unavailable())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| RuntimeError::unavailable())?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(RuntimeError::unavailable)?;
        if total > MAX_EXECUTABLE_BYTES {
            return Err(RuntimeError::unavailable());
        }
        hasher.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(RuntimeError::protocol());
    }
    Ok(hasher.finalize().into())
}
