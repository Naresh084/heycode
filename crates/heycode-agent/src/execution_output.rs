//! Bounded non-consuming output cursors shared by jobs, tools and UI.
use heycode_exec::{OutputStream, ProcessOutputSink};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Maximum retained bytes per stream. Older bytes remain counted by offsets.
pub const EXECUTION_OUTPUT_CAPACITY: usize = 256 * 1024;
/// Maximum bytes one output page returns.
pub const EXECUTION_OUTPUT_PAGE: usize = 64 * 1024;

#[derive(Default, Clone, Serialize, Deserialize)]
struct Stream {
    bytes: VecDeque<u8>,
    total: u64,
}
impl Stream {
    fn append(&mut self, bytes: &[u8], capacity: usize) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        self.bytes.extend(bytes);
        let excess = self.bytes.len().saturating_sub(capacity);
        self.bytes.drain(..excess);
    }
    fn page(&self, offset: u64, limit: usize) -> OutputPage {
        let start = self.total.saturating_sub(self.bytes.len() as u64);
        let actual = offset.max(start).min(self.total);
        let bytes: Vec<_> = self
            .bytes
            .iter()
            .skip((actual - start) as usize)
            .take(limit.min(EXECUTION_OUTPUT_PAGE))
            .copied()
            .collect();
        OutputPage {
            offset: actual,
            next_offset: actual + bytes.len() as u64,
            retained_from: start,
            total_bytes: self.total,
            lost_bytes: start.saturating_sub(offset),
            text: super::execution_jobs::sanitize_output(&String::from_utf8_lossy(&bytes)),
        }
    }
}
/// Byte offsets are independent per stream; text uses lossy UTF-8 decoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputPage {
    /// Actual first byte returned.
    pub offset: u64,
    /// Cursor for the next page.
    pub next_offset: u64,
    /// Oldest retained byte.
    pub retained_from: u64,
    /// Total observed bytes including evicted prefixes.
    pub total_bytes: u64,
    /// Bytes lost since the requested cursor.
    pub lost_bytes: u64,
    /// Terminal-safe rendered text.
    pub text: String,
}
/// Execution facts retained with the owner-scoped output history.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    /// Exact shell program.
    pub command: String,
    /// Resolved working directory.
    pub cwd: String,
    /// Effective deadline.
    pub timeout_ms: Option<u64>,
    /// Unix start timestamp in milliseconds.
    pub started_ms: Option<u64>,
    /// Unix settlement timestamp in milliseconds.
    pub finished_ms: Option<u64>,
    /// Monotonic elapsed milliseconds, frozen at settlement.
    pub elapsed_ms: u64,
    /// Execution-owner settlement description.
    pub reason: Option<String>,
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Default, Clone, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    metadata: Option<ExecutionMetadata>,
    #[serde(skip)]
    clock: Option<std::time::Instant>,
    stdout: Stream,
    stderr: Stream,
    terminal: Stream,
    ended: bool,
    capacity: usize,
    terminal_id: Option<String>,
    interrupted: bool,
    #[serde(default)]
    exit_success: Option<bool>,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    persistence_error: bool,
}
/// Shared bounded output independent of destructive terminal reads.
#[derive(Clone)]
pub struct ExecutionOutput(
    Arc<Mutex<State>>,
    Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
);
impl Default for ExecutionOutput {
    fn default() -> Self {
        Self::new(EXECUTION_OUTPUT_CAPACITY)
    }
}
impl ExecutionOutput {
    pub(crate) fn new(capacity: usize) -> Self {
        Self(
            Arc::new(Mutex::new(State {
                capacity,
                ..State::default()
            })),
            None,
        )
    }
    pub(crate) fn with_permit(mut self, permit: tokio::sync::OwnedSemaphorePermit) -> Self {
        self.1 = Some(Arc::new(permit));
        self
    }

    pub(crate) fn begin(&self, command: String, cwd: String, timeout_ms: Option<u64>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.metadata = Some(ExecutionMetadata {
            command,
            cwd,
            timeout_ms,
            ..ExecutionMetadata::default()
        });
    }
    pub(crate) fn mark_started(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(metadata) = state.metadata.as_mut() {
            metadata.started_ms = Some(now_ms());
        }
        state.clock = Some(std::time::Instant::now());
        persist(&mut state);
    }
    /// Current execution facts, independent of output paging.
    #[must_use]
    pub fn metadata(&self) -> Option<ExecutionMetadata> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut metadata = state.metadata.clone()?;
        if !state.ended {
            metadata.elapsed_ms = state
                .clock
                .map_or(metadata.elapsed_ms, |t| t.elapsed().as_millis() as u64);
        }
        Some(metadata)
    }
    pub(crate) fn finish_reason(&self, reason: String) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(metadata) = state.metadata.as_mut() {
            metadata.reason = Some(reason);
        }
    }
    /// True when recovery found metadata for a process that did not settle.
    #[must_use]
    pub fn interrupted(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).interrupted
    }
    /// Proven process success, or None when exit metadata was not observed.
    #[must_use]
    pub fn exit_success(&self) -> Option<bool> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .exit_success
    }
    /// Whether the latest durable output checkpoint failed.
    #[must_use]
    pub fn persistence_error(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .persistence_error
    }
    pub(crate) fn terminal_id(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .terminal_id
            .clone()
    }
    pub(crate) fn set_terminal(&self, id: String) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.terminal_id = Some(id);
        persist(&mut state);
    }
    pub(crate) fn attach(&self, path: PathBuf) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.path = Some(path);
        persist(&mut state);
    }
    pub(crate) fn load(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 16 * 1024 * 1024 {
            return None;
        }
        let mut state: State = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        if !(1024..=1024 * 1024).contains(&state.capacity)
            || [&state.stdout, &state.stderr, &state.terminal]
                .iter()
                .any(|s| s.bytes.len() > state.capacity || s.total < s.bytes.len() as u64)
        {
            return None;
        }
        state.interrupted |= !state.ended;
        state.ended = true;
        state.path = Some(path.to_path_buf());
        Some(Self(Arc::new(Mutex::new(state)), None))
    }
    pub(crate) fn remove_file(&self) {
        if let Some(path) = &self.0.lock().unwrap_or_else(|e| e.into_inner()).path {
            let _removed = std::fs::remove_file(path);
        }
    }

    /// Read without consuming another reader's cursor.
    #[must_use]
    pub fn read(&self, stream: OutputStream, offset: u64, limit: usize) -> OutputPage {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match stream {
            OutputStream::Stdout => &state.stdout,
            OutputStream::Stderr => &state.stderr,
            OutputStream::Terminal => &state.terminal,
        }
        .page(offset, limit)
    }
    /// Return the newest bounded bytes without changing another reader's cursor.
    #[must_use]
    pub fn tail(&self, stream: OutputStream, limit: usize) -> OutputPage {
        let total = self.read(stream, 0, 0).total_bytes;
        self.read(stream, total.saturating_sub(limit as u64), limit)
    }

    /// Whether the owning worker has ended its output.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).ended
    }
    pub(crate) fn interrupt(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.ended {
            state.interrupted = true;
            let elapsed = state.clock.map(|t| t.elapsed().as_millis() as u64);
            if let Some(metadata) = state.metadata.as_mut() {
                metadata.finished_ms = Some(now_ms());
                if let Some(elapsed) = elapsed {
                    metadata.elapsed_ms = elapsed;
                }
            }
            state.ended = true;
            persist(&mut state);
        }
    }
    pub(crate) fn end(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.ended {
            let elapsed = state.clock.map(|t| t.elapsed().as_millis() as u64);
            if let Some(metadata) = state.metadata.as_mut() {
                metadata.finished_ms = Some(now_ms());
                if let Some(elapsed) = elapsed {
                    metadata.elapsed_ms = elapsed;
                }
            }
            state.ended = true;
            persist(&mut state);
        }
    }
}
impl ProcessOutputSink for ExecutionOutput {
    fn finished(&self, exit: &heycode_exec::ProcessExit) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .exit_success = Some(exit.is_success());
    }
    fn append(&self, stream: OutputStream, bytes: &[u8]) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.ended {
            return;
        }
        let capacity = state.capacity;
        match stream {
            OutputStream::Stdout => &mut state.stdout,
            OutputStream::Stderr => &mut state.stderr,
            OutputStream::Terminal => &mut state.terminal,
        }
        .append(bytes, capacity);
    }
}

fn persist(state: &mut State) {
    let Some(path) = &state.path else {
        return;
    };
    let result = (|| -> std::io::Result<()> {
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && (!metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(std::io::Error::other("unsafe output path"));
        }
        let mut options = atomic_write_file::AtomicWriteFile::options();
        #[cfg(unix)]
        {
            use atomic_write_file::unix::OpenOptionsExt as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.preserve_mode(false).mode(0o600);
        }
        let mut file = options.open(path)?;
        let bytes = serde_json::to_vec(state)?;
        file.write_all(&bytes)?;
        file.commit()
    })();
    state.persistence_error = result.is_err();
}

/// Ensures panicking or withdrawn workers close their retained output owner.
pub(crate) struct OutputCompletion(pub(crate) ExecutionOutput);
impl Drop for OutputCompletion {
    fn drop(&mut self) {
        self.0.interrupt();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn independent_byte_cursors_report_overflow_and_survive_settlement() {
        let output = ExecutionOutput::new(1024);
        output.append(OutputStream::Stdout, &vec![b'x'; 1500]);
        output.append(OutputStream::Stderr, b"separate");
        let page = output.read(OutputStream::Stdout, 0, 100);
        assert_eq!(
            (
                page.offset,
                page.next_offset,
                page.total_bytes,
                page.lost_bytes
            ),
            (476, 576, 1500, 476)
        );
        assert_eq!(output.read(OutputStream::Stdout, 576, 2048).text.len(), 924);
        assert_eq!(output.read(OutputStream::Stdout, 0, 100).text, page.text);
        output.end();
        assert_eq!(output.read(OutputStream::Stderr, 0, 100).text, "separate");
    }
    #[test]
    fn persisted_output_recovers_unsettled_metadata_as_interrupted() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output.json");
        let live = ExecutionOutput::new(1024);
        live.attach(path.clone());
        live.set_terminal("terminal-identity".to_owned());
        let recovered = ExecutionOutput::load(&path).unwrap();
        assert!(recovered.ended());
        assert!(recovered.interrupted());
        assert_eq!(
            recovered.terminal_id().as_deref(),
            Some("terminal-identity")
        );
        live.append(OutputStream::Stdout, b"finished bytes");
        live.end();
        assert!(!live.persistence_error());
        let recovered = ExecutionOutput::load(&path).unwrap();
        assert!(!recovered.interrupted());
        assert_eq!(
            recovered.read(OutputStream::Stdout, 0, 100).text,
            "finished bytes"
        );
    }
    #[cfg(unix)]
    #[test]
    fn output_file_is_private_and_symlink_replacement_fails_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output.json");
        let output = ExecutionOutput::new(1024);
        output.attach(path.clone());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(&path).unwrap();
        let victim = root.path().join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        symlink(&victim, &path).unwrap();
        output.end();
        assert!(output.persistence_error());
        assert_eq!(std::fs::read_to_string(victim).unwrap(), "untouched");
        assert!(ExecutionOutput::load(&path).is_none());
    }
}
