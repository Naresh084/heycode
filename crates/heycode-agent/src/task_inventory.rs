//! Bounded child projections owned by the subagent registry, independent of handles.
use crate::Agent;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

/// Authoritative lifecycle of a delegated task. Interrupted means the process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Admitted and awaiting execution capacity.
    Queued,
    /// Provider initialization or first inference is active.
    Running,
    /// Cancellation requested; execution has not yet settled.
    Cancelling,
    /// Retained child is ready for another message.
    Idle,
    /// One-shot completed.
    Completed,
    /// Execution failed.
    Failed,
    /// Execution settled after cancellation.
    Cancelled,
    /// Archived conversation, restorable in this process.
    Closed,
    /// Execution/handle did not survive a process restart.
    Interrupted,
}
impl TaskState {
    pub(crate) fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}

/// Persisted diagnostic supplied by the execution owner, independently of streamed output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDiagnostic {
    /// Stable diagnostic occurrence; assigned at settlement when absent.
    pub id: String,
    /// Safe bounded diagnostic message.
    pub message: String,
    /// Owner-provided category/code; unknown stays absent.
    pub code: Option<String>,
    /// Owner-provided execution stage; never inferred from message text.
    pub stage: Option<String>,
    /// Exact run identity when exposed by the runtime.
    pub run_id: Option<String>,
    /// Known partial result from this same run.
    pub partial_result: Option<String>,
    /// Retained diagnostic or session log location.
    pub log_location: Option<String>,
    /// Human review/dismissal clears attention without removing evidence.
    #[serde(default)]
    pub acknowledged: bool,
}

/// Safe task projection usable without retaining or opening a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    /// Stable ID minted at admission.
    pub id: String,
    /// Owner permitted to inspect/control this task.
    pub owner: String,
    /// Human label.
    pub label: String,
    /// Selected provider.
    pub provider: String,
    /// Durable session identity, populated before native inference.
    pub session_id: Option<String>,
    /// Actual workspace, retained independently of truncated textual output.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Background job correlation.
    pub job_id: Option<String>,
    /// Parent tool occurrence that admitted this child, when available.
    #[serde(default)]
    pub spawn_call_id: Option<String>,
    /// Exact committed provider accounting; absent historical accounting defaults unknown.
    #[serde(default)]
    pub usage: heycode_session::WorkflowUsage,
    /// Actual lifecycle.
    pub state: TaskState,
    /// Monotonic change cursor.
    pub revision: u64,
    /// Admission wall-clock time in milliseconds.
    pub created_at_ms: u64,
    /// Last state/output change.
    pub updated_at_ms: u64,
    /// Bounded last result/error, with explicit truncation.
    pub output: String,
    /// Whether output exceeded retention.
    pub output_truncated: bool,
    /// Current terminal error, separate from recoverable tool failures.
    #[serde(default)]
    pub terminal_diagnostic: Option<TaskDiagnostic>,
    /// Prior failure evidence retained when a later run succeeds.
    #[serde(default)]
    pub diagnostics: Vec<TaskDiagnostic>,
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

type TaskObserveFn = dyn Fn(&TaskSnapshot) -> std::io::Result<()> + Send + Sync;

/// Host-only observer attached at admission, never accepted from model arguments.
pub(crate) struct TaskObserver(Box<TaskObserveFn>);
impl TaskObserver {
    pub(crate) fn new(
        observe: impl Fn(&TaskSnapshot) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self(Box::new(observe))
    }
}
impl std::fmt::Debug for TaskObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskObserver")
    }
}
impl PartialEq for TaskObserver {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for TaskObserver {}

pub(crate) struct TaskRecord {
    observer: Option<Arc<TaskObserver>>,
    pub snapshot: Mutex<TaskSnapshot>,
    pub cancellation: Mutex<CancellationToken>,
    pub native: Mutex<Weak<Agent>>,
    pub parent: Mutex<Option<Arc<Agent>>>,
    pub changed: tokio::sync::Notify,
    pub turns: tokio::sync::Mutex<()>,
    pub closing: std::sync::atomic::AtomicBool,
    path: Option<std::path::PathBuf>,
}
impl std::fmt::Debug for TaskRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRecord").finish_non_exhaustive()
    }
}
impl PartialEq for TaskRecord {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}
impl Eq for TaskRecord {}
impl TaskRecord {
    pub fn new(
        snapshot: TaskSnapshot,
        cancellation: CancellationToken,
        path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            observer: None,
            snapshot: Mutex::new(snapshot),
            cancellation: Mutex::new(cancellation),
            native: Mutex::new(Weak::new()),
            parent: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
            turns: tokio::sync::Mutex::new(()),
            closing: std::sync::atomic::AtomicBool::new(false),
            path,
        }
    }
    pub(crate) fn with_observer(mut self, observer: Option<Arc<TaskObserver>>) -> Self {
        self.observer = observer;
        self
    }
    pub(crate) fn observe_usage(
        &self,
        usage: Option<heycode_core::TokenUsage>,
    ) -> std::io::Result<()> {
        self.update(|row| match usage {
            Some(usage) => {
                row.usage.reported_requests = row.usage.reported_requests.saturating_add(1);
                row.usage.prompt_tokens =
                    row.usage.prompt_tokens.saturating_add(usage.prompt_tokens);
                row.usage.completion_tokens = row
                    .usage
                    .completion_tokens
                    .saturating_add(usage.completion_tokens);
            }
            None => row.usage.unreported_requests = row.usage.unreported_requests.saturating_add(1),
        })
    }
    pub fn read(&self) -> TaskSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn update(&self, edit: impl FnOnce(&mut TaskSnapshot)) -> std::io::Result<()> {
        let mut row = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = row.clone();
        edit(&mut next);
        if next.state != TaskState::Failed {
            next.terminal_diagnostic = None;
        }
        next.revision = next.revision.saturating_add(1);
        next.updated_at_ms = now_ms().max(row.updated_at_ms);
        if let Some(path) = &self.path {
            if let Some(diagnostic) = next.terminal_diagnostic.as_mut()
                && row
                    .terminal_diagnostic
                    .as_ref()
                    .is_none_or(|prior| prior.id != diagnostic.id)
            {
                // Immutable evidence outlives the bounded index and later successful retries.
                let directory = path
                    .parent()
                    .ok_or_else(|| std::io::Error::other("task diagnostic directory unavailable"))?
                    .join("diagnostics");
                std::fs::create_dir_all(&directory)?;
                let evidence = directory.join(format!("{}.json", heycode_core::CallId::generate()));
                diagnostic.log_location = Some(evidence.display().to_string());
                if let Some(retained) = next
                    .diagnostics
                    .iter_mut()
                    .find(|retained| retained.id == diagnostic.id)
                {
                    retained.log_location = diagnostic.log_location.clone();
                }
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&evidence)?;
                file.write_all(&serde_json::to_vec(diagnostic)?)?;
                file.sync_all()?;
                // The task index stays under its 64 KiB recovery bound; exact
                // bounded diagnostics are retained separately above.
                for text in std::iter::once(&mut diagnostic.message)
                    .chain(diagnostic.partial_result.iter_mut())
                {
                    if text.len() > 4096 {
                        let mut end = 4096;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                        text.push_str("… (see retained log)");
                    }
                }
                if let Some(retained) = next
                    .diagnostics
                    .iter_mut()
                    .find(|retained| retained.id == diagnostic.id)
                {
                    *retained = diagnostic.clone();
                }
                while next.diagnostics.len() > 1
                    && serde_json::to_vec(&next.diagnostics)?.len() > 16 * 1024
                {
                    next.diagnostics.remove(0);
                }
            }
            let tmp = path.with_extension("tmp");
            let bytes = serde_json::to_vec(&next)?;
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(tmp, path)?;
        }
        *row = next;
        // Serialize observer publication with task revisions. The host observer
        // reads the current snapshot and must not reenter this task record.
        if let Some(observer) = &self.observer {
            (observer.0)(&row)?;
        }
        drop(row);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn finish(&self, state: TaskState, output: &str) -> std::io::Result<()> {
        self.finish_with_diagnostic(state, output, None)
    }
    pub fn finish_with_diagnostic(
        &self,
        state: TaskState,
        output: &str,
        diagnostic: Option<TaskDiagnostic>,
    ) -> std::io::Result<()> {
        self.update(|row| {
            row.state = state;
            let mut end = output.len().min(32 * 1024);
            while !output.is_char_boundary(end) {
                end -= 1;
            }
            row.output = output[..end].to_owned();
            row.output_truncated = end < output.len();
            row.terminal_diagnostic = if state == TaskState::Failed {
                let mut diagnostic = diagnostic.unwrap_or_else(|| TaskDiagnostic {
                    message: row.output.clone(),
                    ..TaskDiagnostic::default()
                });
                if diagnostic.id.is_empty() {
                    diagnostic.id =
                        format!("{}:diagnostic:{}", row.id, row.revision.saturating_add(1));
                }
                if diagnostic.message.is_empty() {
                    diagnostic.message =
                        "The run ended without a retained diagnostic message.".into();
                }
                if diagnostic.log_location.is_none() {
                    diagnostic.log_location =
                        self.path.as_ref().map(|path| path.display().to_string());
                }
                for text in std::iter::once(&mut diagnostic.message)
                    .chain(diagnostic.partial_result.iter_mut())
                {
                    if text.len() > 32 * 1024 {
                        let mut end = 32 * 1024;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                    }
                }
                if !row
                    .diagnostics
                    .iter()
                    .any(|prior| prior.id == diagnostic.id)
                {
                    row.diagnostics.push(diagnostic.clone());
                }
                // Each record owns bounded diagnostics; the session journal remains independent.
                if row.diagnostics.len() > 16 {
                    row.diagnostics.remove(0);
                }
                Some(diagnostic)
            } else {
                None
            };
        })
    }
    pub fn publish_native(&self, session_id: String, agent: &Arc<Agent>) -> std::io::Result<()> {
        self.update(|row| {
            row.session_id = Some(session_id);
            row.workspace = Some(agent.cwd().display().to_string());
        })?;
        *self.native.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(agent);
        Ok(())
    }
    pub fn remove_file(&self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod diagnostic_tests {
    use super::*;
    fn snapshot() -> TaskSnapshot {
        TaskSnapshot {
            id: "child-1".into(),
            owner: "parent".into(),
            label: "Tools and execution".into(),
            provider: "native".into(),
            session_id: None,
            workspace: None,
            job_id: None,
            spawn_call_id: None,
            usage: Default::default(),
            state: TaskState::Running,
            revision: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            output: String::new(),
            output_truncated: false,
            terminal_diagnostic: None,
            diagnostics: Vec::new(),
        }
    }
    #[test]
    fn failure_diagnostic_is_durable_and_success_preserves_previous_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("child.json");
        let record = TaskRecord::new(snapshot(), CancellationToken::new(), Some(path.clone()));
        record
            .finish_with_diagnostic(
                TaskState::Failed,
                "Quota exceeded",
                Some(TaskDiagnostic {
                    message: "Quota exceeded".into(),
                    code: Some("rate_limit".into()),
                    stage: Some("provider_request".into()),
                    run_id: Some("run-42".into()),
                    partial_result: Some("Partial notes".into()),
                    ..Default::default()
                }),
            )
            .unwrap();
        let failed: TaskSnapshot = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let diagnostic = failed.terminal_diagnostic.unwrap();
        let evidence = diagnostic.log_location.unwrap();
        let original = std::fs::read(&evidence).unwrap();
        assert!(String::from_utf8_lossy(&original).contains("Partial notes"));
        record
            .finish(TaskState::Idle, "Recovered in a new run")
            .unwrap();
        let succeeded: TaskSnapshot =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(succeeded.terminal_diagnostic.is_none());
        assert_eq!(succeeded.diagnostics.len(), 1);
        assert_eq!(std::fs::read(evidence).unwrap(), original);
    }
    #[test]
    fn repeated_large_diagnostics_keep_history_index_readable_and_immutable_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("child.json");
        let record = TaskRecord::new(snapshot(), CancellationToken::new(), Some(path.clone()));
        for _ in 0..20 {
            record
                .finish(TaskState::Failed, &"q".repeat(32 * 1024))
                .unwrap();
        }
        assert!(std::fs::metadata(path).unwrap().len() < 64 * 1024);
        assert_eq!(
            std::fs::read_dir(root.path().join("diagnostics"))
                .unwrap()
                .count(),
            20
        );
    }
    #[test]
    fn legacy_snapshot_without_diagnostics_still_replays_and_cancellation_is_not_failure() {
        let mut value = serde_json::to_value(snapshot()).unwrap();
        value.as_object_mut().unwrap().remove("terminal_diagnostic");
        value.as_object_mut().unwrap().remove("diagnostics");
        let legacy: TaskSnapshot = serde_json::from_value(value).unwrap();
        assert!(legacy.diagnostics.is_empty());
        let record = TaskRecord::new(legacy, CancellationToken::new(), None);
        record
            .finish(TaskState::Cancelled, "User cancelled")
            .unwrap();
        assert!(record.read().terminal_diagnostic.is_none());
    }
}
