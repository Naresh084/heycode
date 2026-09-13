//! A native, session-only directory grant prompt. Selection is read-only;
//! confirmation goes through the workspace owner's ordinary admission fence.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Result, anyhow, ensure};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::FutureExt;
use heycode_agent::workspace_transition::{
    DirectoryGrantCandidate, WorkspaceSnapshot, WorkspaceTransitionService,
};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;

/// The command-to-frontend service for `/add-dir` without a path.
pub const SERVICE_ADD_DIRECTORY_PROMPT: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("add-directory-prompt");

const MAX_PATH_BYTES: usize = 4096;

#[derive(Default)]
struct BridgeState {
    attached: Option<Arc<CancellationToken>>,
    active: Weak<RequestLease>,
    pending: Option<AddDirectoryDialog>,
}

struct RequestLease {
    cancellation: CancellationToken,
}

/// A bounded bridge: exactly one attached frontend and one pending/live dialog.
#[derive(Clone, Default)]
pub struct AddDirectoryBridge(Arc<Mutex<BridgeState>>);

impl AddDirectoryBridge {
    /// Keep this guard for the entire frontend run, including suspended modals.
    pub fn attach(&self) -> Result<AddDirectoryAttachment> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow!("Directory prompt unavailable"))?;
        ensure!(
            state.attached.is_none(),
            "Directory prompt already attached"
        );
        let cancellation = Arc::new(CancellationToken::new());
        state.attached = Some(cancellation.clone());
        Ok(AddDirectoryAttachment {
            bridge: self.clone(),
            cancellation,
        })
    }

    /// Enqueue a path prompt without parking the command dispatcher.
    pub fn request(&self, service: Arc<WorkspaceTransitionService>) -> Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow!("Directory prompt unavailable"))?;
        let attached = state.attached.as_ref().ok_or_else(|| {
            anyhow!("An interactive terminal is required; use /add-dir <path> here")
        })?;
        ensure!(
            state.active.upgrade().is_none(),
            "A directory prompt is already open or settling"
        );
        let lease = Arc::new(RequestLease {
            cancellation: attached.child_token(),
        });
        state.active = Arc::downgrade(&lease);
        state.pending = Some(AddDirectoryDialog::new(service, lease));
        Ok(())
    }

    /// Take the pending request when the frontend can display it.
    pub fn take(&self) -> Option<AddDirectoryDialog> {
        self.0.lock().ok()?.pending.take()
    }
}

/// Detaching cancels pending selections and any commit still waiting to begin.
pub struct AddDirectoryAttachment {
    bridge: AddDirectoryBridge,
    cancellation: Arc<CancellationToken>,
}

impl Drop for AddDirectoryAttachment {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let pending = self.bridge.0.lock().ok().and_then(|mut state| {
            if state
                .attached
                .as_ref()
                .is_some_and(|token| Arc::ptr_eq(token, &self.cancellation))
            {
                state.attached = None;
                state.pending.take()
            } else {
                None
            }
        });
        drop(pending);
    }
}

enum Phase {
    Path,
    Confirm(DirectoryGrantCandidate),
    Commit {
        path: PathBuf,
        task: JoinHandle<Result<WorkspaceSnapshot>>,
    },
    Settled,
}

/// One final result. A successful atomic commit always wins a cancellation race.
pub enum AddDirectoryOutcome {
    /// The request settled before granting access.
    Cancelled,
    /// Actual session authority has been committed by the workspace owner.
    Granted {
        /// Canonical path the human confirmed.
        path: PathBuf,
        /// Authoritative committed state.
        snapshot: Box<WorkspaceSnapshot>,
    },
}

#[derive(Clone, Copy, Default)]
struct Buttons {
    grant: Option<Rect>,
    cancel: Option<Rect>,
}

/// Input state lives outside the main composer, preserving its draft and cursor.
pub struct AddDirectoryDialog {
    service: Arc<WorkspaceTransitionService>,
    lease: Arc<RequestLease>,
    editor: TextArea<'static>,
    phase: Phase,
    error: Option<String>,
    grant_selected: bool,
    path_scroll: u16,
    buttons: Cell<Buttons>,
}

impl AddDirectoryDialog {
    fn new(service: Arc<WorkspaceTransitionService>, lease: Arc<RequestLease>) -> Self {
        let mut editor = TextArea::default();
        editor.set_cursor_line_style(Style::default());
        Self {
            service,
            lease,
            editor,
            phase: Phase::Path,
            error: None,
            grant_selected: true,
            path_scroll: 0,
            buttons: Cell::default(),
        }
    }

    /// Consume dialog keys. Enter only submits on a fresh key press, so repeats
    /// from path entry cannot implicitly confirm the following grant.
    pub fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            self.lease.cancellation.cancel();
            return;
        }
        if self.lease.cancellation.is_cancelled() {
            return;
        }
        match &self.phase {
            Phase::Path => {
                if key.code == KeyCode::Enter && key.kind == KeyEventKind::Press {
                    self.preview();
                } else if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('u')
                {
                    self.editor.delete_line_by_head();
                } else if matches!(
                    key.code,
                    KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Home
                        | KeyCode::End
                ) || (key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(
                        key.code,
                        KeyCode::Char('a' | 'e' | 'b' | 'f' | 'h' | 'd' | 'k' | 'w')
                    ))
                {
                    self.editor.input(key);
                } else if let KeyCode::Char(ch) = key.code
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    self.paste(&ch.to_string());
                }
            }
            Phase::Confirm(_) => match key.code {
                KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down => {
                    self.grant_selected = !self.grant_selected;
                }
                KeyCode::PageDown => self.path_scroll = self.path_scroll.saturating_add(3),
                KeyCode::PageUp => self.path_scroll = self.path_scroll.saturating_sub(3),
                KeyCode::Enter if key.kind == KeyEventKind::Press => {
                    if self.grant_selected {
                        self.confirm();
                    } else {
                        self.lease.cancellation.cancel();
                    }
                }
                _ => {}
            },
            Phase::Commit { .. } | Phase::Settled => {}
        }
    }

    /// Consume a bounded, single-line paste as path data. Paste never submits.
    pub fn paste(&mut self, text: &str) {
        if !matches!(self.phase, Phase::Path) || self.lease.cancellation.is_cancelled() {
            return;
        }
        if text.chars().any(char::is_control) {
            self.error =
                Some("Enter one directory path without newlines or control characters.".into());
        } else if self.editor.lines()[0].len().saturating_add(text.len()) > MAX_PATH_BYTES {
            self.error = Some(format!(
                "Directory paths are limited to {MAX_PATH_BYTES} bytes."
            ));
        } else {
            self.editor.insert_str(text);
            self.error = None;
        }
    }

    /// Activate only buttons hit-tested against the most recent visible draw.
    pub fn mouse(&mut self, event: MouseEvent) {
        if event.kind != MouseEventKind::Down(MouseButton::Left)
            || self.lease.cancellation.is_cancelled()
        {
            return;
        }
        let point = Position::new(event.column, event.row);
        let buttons = self.buttons.get();
        if buttons.cancel.is_some_and(|rect| rect.contains(point)) {
            self.lease.cancellation.cancel();
        } else if buttons.grant.is_some_and(|rect| rect.contains(point)) {
            match self.phase {
                Phase::Path => self.preview(),
                Phase::Confirm(_) => self.confirm(),
                Phase::Commit { .. } | Phase::Settled => {}
            }
        }
    }

    /// Poll without blocking the terminal. Errors remain visible for correction.
    pub fn poll(&mut self) -> Option<AddDirectoryOutcome> {
        let finished = matches!(&self.phase, Phase::Commit { task, .. } if task.is_finished());
        if finished {
            let phase = std::mem::replace(&mut self.phase, Phase::Settled);
            if let Phase::Commit { path, mut task } = phase {
                match (&mut task).now_or_never() {
                    Some(Ok(Ok(snapshot))) => {
                        return Some(AddDirectoryOutcome::Granted {
                            path,
                            snapshot: Box::new(snapshot),
                        });
                    }
                    Some(Ok(Err(error))) => {
                        if self.lease.cancellation.is_cancelled() {
                            return Some(AddDirectoryOutcome::Cancelled);
                        }
                        self.error = Some(
                            crate::markdown::terminal_safe_span(&error.to_string()).into_owned(),
                        );
                        self.phase = Phase::Path;
                    }
                    Some(Err(error)) => {
                        // A runtime panic is not a confirmed cancellation. Keep
                        // the error visible rather than claim an unobserved result.
                        self.error = Some(format!(
                            "Directory grant task failed: {error}. Check /worktree status before retrying."
                        ));
                        self.phase = Phase::Path;
                    }
                    None => self.phase = Phase::Commit { path, task },
                }
            }
        }
        if self.lease.cancellation.is_cancelled()
            && matches!(self.phase, Phase::Path | Phase::Confirm(_))
        {
            self.phase = Phase::Settled;
            return Some(AddDirectoryOutcome::Cancelled);
        }
        None
    }

    /// Keep polling a pending commit even while a higher-priority modal hides it.
    #[must_use]
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Commit { .. })
    }

    /// Preferred terminal height; rendering also supports smaller available areas.
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        14
    }

    /// Full, untruncated text for flat output and screen-reader announcements.
    #[must_use]
    pub fn plain_lines(&self) -> Vec<String> {
        let mut lines = vec!["Add a directory".into()];
        match &self.phase {
            Phase::Path => {
                lines.push("Enter an existing directory path (relative to the current workspace or absolute).".into());
                lines.push(format!("Directory: {}", self.editor.lines()[0]));
                lines.push("Enter: review directory. Escape: cancel.".into());
            }
            Phase::Confirm(candidate) => {
                lines.push("Allow file access to this directory for this session?".into());
                lines.push(candidate.path().display().to_string());
                lines.push(
                    "This does not change the working directory or grant project trust.".into(),
                );
                lines.push(format!(
                    "{} Grant for this session    {} Cancel",
                    if self.grant_selected { ">" } else { " " },
                    if self.grant_selected { " " } else { ">" }
                ));
                lines.push("Tab/arrows: choose. Enter: confirm. Escape: cancel. PageUp/PageDown: scroll path.".into());
            }
            Phase::Commit { path, .. } => {
                lines.push(path.display().to_string());
                lines.push(
                    if self.lease.cancellation.is_cancelled() {
                        "Settling cancellation…"
                    } else {
                        "Granting directory access…"
                    }
                    .into(),
                );
            }
            Phase::Settled => lines.push("Directory prompt closed.".into()),
        }
        if let Some(error) = &self.error {
            lines.push(error.clone());
        }
        lines
    }

    fn preview(&mut self) {
        self.buttons.set(Buttons::default());
        let raw = self.editor.lines()[0].trim();
        if raw.is_empty() {
            self.error = Some("An existing directory path is required.".into());
            return;
        }
        let path = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        if path.is_empty() {
            self.error = Some("An existing directory path is required.".into());
            return;
        }
        match self.service.prepare_directory_grant(Path::new(path)) {
            Ok(candidate) => {
                if candidate
                    .path()
                    .to_str()
                    .is_none_or(|text| text.chars().any(char::is_control))
                {
                    self.error = Some("This directory contains characters that cannot be displayed safely; choose another path.".into());
                    return;
                }
                self.phase = Phase::Confirm(candidate);
                self.error = None;
                self.grant_selected = true;
                self.path_scroll = 0;
            }
            Err(error) => {
                self.error =
                    Some(crate::markdown::terminal_safe_span(&error.to_string()).into_owned())
            }
        }
    }

    fn confirm(&mut self) {
        self.buttons.set(Buttons::default());
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.error = Some("Directory grant runtime unavailable.".into());
            return;
        };
        let phase = std::mem::replace(&mut self.phase, Phase::Settled);
        if let Phase::Confirm(candidate) = phase {
            let path = candidate.path().to_path_buf();
            let service = self.service.clone();
            let lease = self.lease.clone();
            let task = runtime.spawn(async move {
                service
                    .confirm_directory_grant(candidate, lease.cancellation.clone())
                    .await
                    .map_err(Into::into)
            });
            self.phase = Phase::Commit { path, task };
        } else {
            self.phase = phase;
        }
    }
}

impl Drop for AddDirectoryDialog {
    fn drop(&mut self) {
        self.lease.cancellation.cancel();
        // Never abort a commit and discard its result. The task owns the lease
        // until it settles; authority is durable if cancellation arrived too late.
        // Dropping a Tokio JoinHandle detaches, it does not cancel its future.
    }
}

/// Draw the modal in the caller's allocated area. Default terminal colours also
/// work in monochrome and light themes; selection uses reverse/bold modifiers.
pub fn draw(frame: &mut Frame<'_>, area: Rect, dialog: &AddDirectoryDialog) {
    dialog.buttons.set(Buttons::default());
    if area.width < 8 || area.height < 5 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Add a directory ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let footer_y = inner.bottom().saturating_sub(1);
    let buttons_y = footer_y.saturating_sub(1);
    let text = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        buttons_y.saturating_sub(inner.y),
    );
    match &dialog.phase {
        Phase::Path => {
            frame.render_widget(
                Paragraph::new("Enter an existing directory path:").wrap(Wrap { trim: false }),
                Rect::new(text.x, text.y, text.width, 1),
            );
            if text.height > 1 {
                frame.render_widget(&dialog.editor, Rect::new(text.x, text.y + 1, text.width, 1));
            }
            if text.height > 3 {
                let helper = dialog.error.as_deref().unwrap_or(
                    "Relative paths use the current workspace. Access applies to this session.",
                );
                frame.render_widget(
                    Paragraph::new(helper).wrap(Wrap { trim: false }),
                    Rect::new(text.x, text.y + 3, text.width, text.height - 3),
                );
            }
        }
        Phase::Confirm(candidate) => {
            frame.render_widget(
                Paragraph::new("Allow file access for this session?").wrap(Wrap { trim: false }),
                Rect::new(text.x, text.y, text.width, 1),
            );
            let path_area = Rect::new(
                text.x,
                text.y + 1,
                text.width,
                text.height.saturating_sub(3),
            );
            frame.render_widget(
                Paragraph::new(candidate.path().display().to_string())
                    .wrap(Wrap { trim: false })
                    .scroll((dialog.path_scroll, 0)),
                path_area,
            );
            if text.height >= 2 {
                frame.render_widget(
                    Paragraph::new("Working directory and project trust stay unchanged.")
                        .wrap(Wrap { trim: false }),
                    Rect::new(text.x, text.bottom() - 2, text.width, 2),
                );
            }
        }
        Phase::Commit { .. } | Phase::Settled => {
            frame.render_widget(
                Paragraph::new(
                    dialog
                        .plain_lines()
                        .into_iter()
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .wrap(Wrap { trim: false }),
                text,
            );
        }
    }
    if matches!(dialog.phase, Phase::Path | Phase::Confirm(_)) {
        let label = if matches!(dialog.phase, Phase::Path) {
            "[ Review ]"
        } else {
            "[ Grant for this session ]"
        };
        let grant_width = (label.len() as u16).min(inner.width.saturating_sub(11));
        let grant = Rect::new(inner.x, buttons_y, grant_width, 1);
        let cancel = Rect::new(
            grant.right().saturating_add(2),
            buttons_y,
            10.min(inner.width.saturating_sub(grant_width + 2)),
            1,
        );
        let selected = Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(label).style(if dialog.grant_selected {
                selected
            } else {
                Style::default()
            }),
            grant,
        );
        frame.render_widget(
            Paragraph::new("[ Cancel ]").style(if dialog.grant_selected {
                Style::default()
            } else {
                selected
            }),
            cancel,
        );
        dialog.buttons.set(Buttons {
            grant: Some(grant),
            cancel: Some(cancel),
        });
    }
    frame.render_widget(
        Paragraph::new(if matches!(dialog.phase, Phase::Confirm(_)) {
            "Tab: choose · Enter: confirm · Esc: cancel · PgUp/PgDn: path"
        } else {
            "Enter: review · Esc: cancel"
        }),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_agent::workspace_transition::{
        WorkspaceTransitionError, WorkspaceTransitionGuard, WorkspaceTransitionOrigin,
        WorkspaceTransitionPermit,
    };
    use heycode_exec::{
        LocalShellConfig, SandboxMode, SandboxService, ShellService, SubprocessService,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    struct Admission {
        wait: AtomicBool,
        entered: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl WorkspaceTransitionGuard for Admission {
        async fn acquire(
            &self,
            _: WorkspaceTransitionOrigin,
        ) -> std::result::Result<Box<dyn WorkspaceTransitionPermit>, WorkspaceTransitionError>
        {
            self.entered.notify_one();
            if self.wait.load(Ordering::SeqCst) {
                futures::future::pending::<()>().await;
            }
            Ok(Box::new(()))
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        extra: PathBuf,
        journal: PathBuf,
        service: Arc<WorkspaceTransitionService>,
        admission: Arc<Admission>,
        bridge: AddDirectoryBridge,
        attachment: Option<AddDirectoryAttachment>,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(temp.path()).unwrap();
            let root = base.join("root");
            let extra = base.join("extra dir");
            let session = base.join("session");
            for path in [&root, &extra, &session] {
                std::fs::create_dir(path).unwrap();
            }
            let journal = session.join("workspace.json");
            let service = WorkspaceTransitionService::open(
                journal.clone(),
                root.clone(),
                SandboxService::new(SandboxMode::Off, root.clone(), None).unwrap(),
                ShellService::local(
                    LocalShellConfig::platform(root.clone(), Duration::from_secs(5)).unwrap(),
                ),
                SubprocessService::local(),
                base.join("worktrees"),
            )
            .unwrap();
            let admission = Arc::new(Admission {
                wait: AtomicBool::new(false),
                entered: tokio::sync::Notify::new(),
            });
            service.install_guard(admission.clone()).unwrap();
            let bridge = AddDirectoryBridge::default();
            let attachment = Some(bridge.attach().unwrap());
            Self {
                _temp: temp,
                root,
                extra,
                journal,
                service,
                admission,
                bridge,
                attachment,
            }
        }
        fn dialog(&self) -> AddDirectoryDialog {
            self.bridge.request(self.service.clone()).unwrap();
            self.bridge.take().unwrap()
        }
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    async fn settle(dialog: &mut AddDirectoryDialog) -> Option<AddDirectoryOutcome> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(outcome) = dialog.poll() {
                    return Some(outcome);
                }
                if !dialog.running() {
                    return None;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn path_preview_requires_fresh_confirmation_and_preserves_cwd_and_trust() {
        let fixture = Fixture::new();
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        assert!(!fixture.journal.exists());
        dialog.key(key(KeyCode::Enter));
        assert!(matches!(dialog.phase, Phase::Confirm(_)));
        assert!(!fixture.journal.exists());
        dialog.key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert!(!dialog.running());
        dialog.paste("\n");
        assert!(!dialog.running());
        dialog.key(key(KeyCode::Enter));
        let AddDirectoryOutcome::Granted { path, snapshot } = settle(&mut dialog).await.unwrap()
        else {
            panic!("grant should succeed")
        };
        assert_eq!(path, fixture.extra);
        assert_eq!(snapshot.cwd, fixture.root);
        assert!(snapshot.roots.iter().any(|root| root.path == fixture.extra));
        assert!(fixture.journal.exists());
        assert!(dialog.poll().is_none());
        fixture
            .service
            .change_directory(
                &fixture.extra,
                WorkspaceTransitionOrigin::HumanCommand,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            fixture
                .service
                .instruction_sources(None, true)
                .unwrap()
                .workspace
                .is_none()
        );
    }

    #[tokio::test]
    async fn bounded_bridge_detach_cancels_pending_and_live_requests() {
        let mut fixture = Fixture::new();
        assert!(fixture.bridge.attach().is_err());
        fixture.bridge.request(fixture.service.clone()).unwrap();
        assert!(fixture.bridge.request(fixture.service.clone()).is_err());
        fixture.attachment.take();
        assert!(fixture.bridge.take().is_none());
        assert!(
            fixture
                .bridge
                .request(fixture.service.clone())
                .unwrap_err()
                .to_string()
                .contains("interactive terminal")
        );
        fixture.attachment = Some(fixture.bridge.attach().unwrap());
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        fixture.attachment.take();
        assert!(matches!(
            dialog.poll(),
            Some(AddDirectoryOutcome::Cancelled)
        ));
        assert!(!fixture.journal.exists());
        drop(dialog);
        fixture.attachment = Some(fixture.bridge.attach().unwrap());
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        dialog.key(key(KeyCode::Tab));
        dialog.key(key(KeyCode::Enter));
        assert!(matches!(
            dialog.poll(),
            Some(AddDirectoryOutcome::Cancelled)
        ));
        assert!(!fixture.journal.exists());
    }

    #[tokio::test]
    async fn cancellation_and_drop_settle_waiting_admission_without_authority() {
        let fixture = Fixture::new();
        fixture.admission.wait.store(true, Ordering::SeqCst);
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        dialog.key(key(KeyCode::Enter));
        fixture.admission.entered.notified().await;
        assert!(dialog.running());
        dialog.key(key(KeyCode::Esc));
        assert!(matches!(
            settle(&mut dialog).await,
            Some(AddDirectoryOutcome::Cancelled)
        ));
        assert!(!fixture.journal.exists());
        drop(dialog);
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        dialog.key(key(KeyCode::Enter));
        fixture.admission.entered.notified().await;
        drop(dialog);
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.bridge.0.lock().unwrap().active.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!fixture.journal.exists());
        assert_eq!(fixture.service.snapshot().unwrap().revision, 0);
    }

    #[tokio::test]
    async fn committed_grant_wins_late_escape_and_detach() {
        let mut fixture = Fixture::new();
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        dialog.key(key(KeyCode::Enter));
        tokio::time::timeout(Duration::from_secs(3), async {
            while fixture.service.snapshot().unwrap().revision == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        fixture.attachment.take();
        dialog.key(key(KeyCode::Esc));
        assert!(matches!(
            settle(&mut dialog).await,
            Some(AddDirectoryOutcome::Granted { .. })
        ));
        assert_eq!(fixture.service.snapshot().unwrap().revision, 1);
    }

    #[tokio::test]
    async fn stale_selection_errors_inline_and_allows_a_fresh_preview() {
        let fixture = Fixture::new();
        let mut dialog = fixture.dialog();
        dialog.paste(&fixture.extra.display().to_string());
        dialog.key(key(KeyCode::Enter));
        std::fs::rename(&fixture.extra, fixture.extra.with_file_name("old")).unwrap();
        std::fs::create_dir(&fixture.extra).unwrap();
        dialog.key(key(KeyCode::Enter));
        assert!(settle(&mut dialog).await.is_none());
        assert!(
            dialog
                .plain_lines()
                .join("\n")
                .contains("Directory changed")
        );
        assert!(!fixture.journal.exists());
        dialog.key(key(KeyCode::Enter));
        dialog.key(key(KeyCode::Enter));
        assert!(matches!(
            settle(&mut dialog).await,
            Some(AddDirectoryOutcome::Granted { .. })
        ));
    }

    #[tokio::test]
    async fn paste_controls_bounds_and_mouse_confirmation_use_owned_state() {
        let fixture = Fixture::new();
        let mut dialog = fixture.dialog();
        dialog.paste("bad\npath");
        assert!(dialog.editor.lines()[0].is_empty());
        dialog.paste(&"x".repeat(MAX_PATH_BYTES + 1));
        assert!(dialog.editor.lines()[0].is_empty());
        dialog.paste("old-prefix-suffix");
        for _ in 0..6 {
            dialog.key(key(KeyCode::Left));
        }
        dialog.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            dialog.editor.lines()[0],
            "suffix",
            "Ctrl+U deletes only the prefix before the cursor"
        );
        dialog.key(key(KeyCode::End));
        dialog.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(dialog.editor.lines()[0].is_empty());
        dialog.paste(&fixture.extra.display().to_string());
        let mut terminal = Terminal::new(TestBackend::new(90, 18)).unwrap();
        terminal
            .draw(|frame| draw(frame, Rect::new(1, 1, 85, 14), &dialog))
            .unwrap();
        let review = dialog.buttons.get().grant.unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: review.x,
            row: review.y,
            modifiers: KeyModifiers::NONE,
        };
        dialog.mouse(click);
        assert!(matches!(dialog.phase, Phase::Confirm(_)));
        dialog.mouse(click); // stale review coordinates cannot immediately grant.
        assert!(!dialog.running());
        terminal
            .draw(|frame| draw(frame, Rect::new(1, 1, 85, 14), &dialog))
            .unwrap();
        let grant = dialog.buttons.get().grant.unwrap();
        dialog.mouse(MouseEvent {
            column: grant.x,
            row: grant.y,
            ..click
        });
        assert!(matches!(
            settle(&mut dialog).await,
            Some(AddDirectoryOutcome::Granted { .. })
        ));
    }
}
