//! Standalone contract checks for the source-grounded conversation exporter.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../src/export_panel.rs"]
mod export_panel;

use std::sync::Arc;

use async_trait::async_trait;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use export_panel::{
    ConversationExportError, ConversationExportPanel, ExportAction, ExportPanelStyles,
    ExportProgressAction, PlainTextExportProgress, PlainTextExportRequest,
    commit_plain_text_export,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use tokio_util::sync::CancellationToken;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

struct DelayedFileSystem {
    inner: heycode_exec::FileSystemService,
    started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl heycode_exec::FileSystemBackend for DelayedFileSystem {
    async fn write_checked(
        &self,
        _spec: heycode_exec::CheckedWriteSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::CheckedWriteOutput, heycode_exec::FileSystemError> {
        self.started.notify_one();
        cancellation.cancelled().await;
        Err(heycode_exec::FileSystemError::new(
            heycode_exec::FileSystemErrorCode::Cancelled,
        ))
    }

    fn resolve(
        &self,
        request: heycode_exec::PathRequest,
    ) -> Result<heycode_exec::ResolvedPath, heycode_exec::FileSystemError> {
        self.inner.resolve(request)
    }

    fn observations(&self) -> heycode_exec::ObservationLog {
        self.inner.observations()
    }

    fn policy(&self) -> heycode_exec::FileSystemPolicy {
        self.inner.policy()
    }

    async fn metadata(
        &self,
        path: heycode_exec::ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::FileMetadata, heycode_exec::FileSystemError> {
        self.inner.metadata(path, cancellation).await
    }

    async fn create_dir_all(
        &self,
        path: heycode_exec::ResolvedPath,
        cancellation: CancellationToken,
    ) -> Result<(), heycode_exec::FileSystemError> {
        self.inner.create_dir_all(path, cancellation).await
    }

    async fn read(
        &self,
        spec: heycode_exec::ReadFileSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::ReadFileOutput, heycode_exec::FileSystemError> {
        self.inner.read(spec, cancellation).await
    }

    async fn write(
        &self,
        spec: heycode_exec::WriteFileSpec,
        cancellation: CancellationToken,
    ) -> Result<(), heycode_exec::FileSystemError> {
        self.inner.write(spec, cancellation).await
    }

    async fn edit(
        &self,
        spec: heycode_exec::EditFileSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::EditFileOutput, heycode_exec::FileSystemError> {
        self.inner.edit(spec, cancellation).await
    }

    async fn glob(
        &self,
        spec: heycode_exec::GlobSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::GlobOutput, heycode_exec::FileSystemError> {
        self.inner.glob(spec, cancellation).await
    }

    async fn grep(
        &self,
        spec: heycode_exec::GrepSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::GrepOutput, heycode_exec::FileSystemError> {
        self.inner.grep(spec, cancellation).await
    }
}

#[test]
fn chooser_defaults_to_clipboard_and_escape_cancels() {
    let mut panel = ConversationExportPanel::new(Arc::from("exact\n"), "conversation.txt")
        .expect("valid panel");
    assert!(panel.plain_lines().join("\n").contains("selected: 1. Copy"));
    match panel.handle_event(&key(KeyCode::Enter)) {
        ExportAction::Copy { transcript } => assert_eq!(&*transcript, "exact\n"),
        _ => panic!("default Enter must copy"),
    }
    assert!(matches!(
        panel.handle_event(&key(KeyCode::Esc)),
        ExportAction::Cancel
    ));
}

#[test]
fn save_escape_returns_to_selected_method_then_cancels() {
    let mut panel =
        ConversationExportPanel::new(Arc::from("exact"), "conversation.txt").expect("valid panel");
    assert!(matches!(
        panel.handle_event(&key(KeyCode::Down)),
        ExportAction::None
    ));
    let _ = panel.handle_event(&key(KeyCode::Enter));
    assert!(panel.plain_lines().contains(&"Enter filename:".to_owned()));
    assert!(matches!(
        panel.handle_event(&key(KeyCode::Esc)),
        ExportAction::None
    ));
    assert!(panel.plain_lines().join("\n").contains("selected: 2. Save"));
    assert!(matches!(
        panel.handle_event(&key(KeyCode::Esc)),
        ExportAction::Cancel
    ));
}

#[tokio::test]
async fn filename_form_appends_txt_and_rejects_multiline_paste() {
    let mut panel =
        ConversationExportPanel::new(Arc::from("exact"), "conversation").expect("valid panel");
    let _ = panel.handle_event(&key(KeyCode::Down));
    let _ = panel.handle_event(&key(KeyCode::Enter));
    let _ = panel.handle_event(&Event::Paste("\nbad".to_owned()));
    assert!(panel.plain_lines().join("\n").contains("without control"));
    let ExportAction::Save(request) = panel.handle_event(&key(KeyCode::Enter)) else {
        panic!("filename Enter must plan a save");
    };
    assert_eq!(request.destination().to_string_lossy(), "conversation.txt");
    assert_eq!(request.transcript(), "exact");
    let root = tempfile::tempdir().expect("tempdir");
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root.path(),
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("root")])
    .expect("policy");
    let filesystem = heycode_exec::FileSystemService::local(policy).expect("filesystem");
    let receipt =
        commit_plain_text_export(&filesystem, root.path(), request, CancellationToken::new())
            .await
            .expect("commit");
    assert!(receipt.path().ends_with("conversation.txt"));
    assert_eq!(std::fs::read(receipt.path()).expect("read"), b"exact");
}

#[tokio::test]
async fn quoted_nested_destination_is_normalized_without_touching_bytes() {
    let request = PlainTextExportRequest::new("\"reports/my conversation\"", Arc::from("a\nβ\n"))
        .expect("valid request");
    assert_eq!(
        request.destination().to_string_lossy(),
        "reports/my conversation.txt"
    );
    assert_eq!(request.transcript().as_bytes(), "a\nβ\n".as_bytes());
    let root = tempfile::tempdir().expect("tempdir");
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root.path(),
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("root")])
    .expect("policy");
    let filesystem = heycode_exec::FileSystemService::local(policy).expect("filesystem");
    let receipt =
        commit_plain_text_export(&filesystem, root.path(), request, CancellationToken::new())
            .await
            .expect("commit");
    assert!(receipt.path().ends_with("reports/my conversation.txt"));
    assert_eq!(
        std::fs::read(receipt.path()).expect("read"),
        "a\nβ\n".as_bytes()
    );
}

#[tokio::test]
async fn commit_creates_parents_and_refuses_existing_target() {
    let root = tempfile::tempdir().expect("tempdir");
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root.path(),
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("root")])
    .expect("policy");
    let filesystem = heycode_exec::FileSystemService::local(policy).expect("filesystem");
    let request =
        PlainTextExportRequest::new("nested/export", Arc::from("exact\n")).expect("request");
    let receipt = commit_plain_text_export(
        &filesystem,
        root.path(),
        request.clone(),
        CancellationToken::new(),
    )
    .await
    .expect("first create");
    assert_eq!(receipt.byte_len(), 6);
    assert_eq!(std::fs::read(receipt.path()).expect("read"), b"exact\n");

    let error =
        commit_plain_text_export(&filesystem, root.path(), request, CancellationToken::new())
            .await
            .expect_err("must not overwrite");
    assert!(matches!(
        error,
        ConversationExportError::Filesystem {
            code: heycode_exec::FileSystemErrorCode::ChangedAtCommit,
            ..
        }
    ));
    assert_eq!(std::fs::read(receipt.path()).expect("read"), b"exact\n");
}

#[tokio::test]
async fn commit_refuses_directory_target_and_outside_root() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("occupied.txt")).expect("directory");
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root.path(),
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("root")])
    .expect("policy");
    let filesystem = heycode_exec::FileSystemService::local(policy).expect("filesystem");

    let directory =
        PlainTextExportRequest::new("occupied.txt", Arc::from("exact")).expect("request");
    let error = commit_plain_text_export(
        &filesystem,
        root.path(),
        directory,
        CancellationToken::new(),
    )
    .await
    .expect_err("directory is not replaced");
    assert!(matches!(
        error,
        ConversationExportError::Filesystem {
            code: heycode_exec::FileSystemErrorCode::ChangedAtCommit,
            ..
        }
    ));

    let outside_root = tempfile::tempdir().expect("outside");
    let outside = PlainTextExportRequest::new(
        outside_root
            .path()
            .join("export.txt")
            .to_string_lossy()
            .as_ref(),
        Arc::from("exact"),
    )
    .expect("request");
    let error =
        commit_plain_text_export(&filesystem, root.path(), outside, CancellationToken::new())
            .await
            .expect_err("outside root");
    assert!(matches!(
        error,
        ConversationExportError::Filesystem {
            code: heycode_exec::FileSystemErrorCode::OutsideAllowedRoots,
            ..
        }
    ));
}

#[test]
fn method_and_filename_states_render_source_copy() {
    let mut panel =
        ConversationExportPanel::new(Arc::from("exact"), "conversation.txt").expect("valid panel");
    assert_eq!(panel.desired_height(), 9);
    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let styles = ExportPanelStyles::new(Color::White, Color::DarkGray, Color::LightBlue);
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), styles))
        .expect("draw methods");
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Export conversation"));
    assert!(rendered.contains("Copy to clipboard"));
    assert!(rendered.contains("Save to file"));

    let _ = panel.handle_event(&key(KeyCode::Down));
    let _ = panel.handle_event(&key(KeyCode::Enter));
    assert_eq!(panel.desired_height(), 10);
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), styles))
        .expect("draw filename");
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Enter filename:"));
    assert!(rendered.contains("conversation.txt"));
    assert!(rendered.contains("Enter to save · Esc to go back"));
}

#[test]
fn chooser_mouse_selects_save_without_leaking_paste() {
    let mut panel =
        ConversationExportPanel::new(Arc::from("exact"), "conversation.txt").expect("panel");
    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let styles = ExportPanelStyles::new(Color::White, Color::DarkGray, Color::LightBlue);
    terminal
        .draw(|frame| panel.draw(frame, frame.area(), styles))
        .expect("draw");
    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(panel.handle_event(&click), ExportAction::None));
    assert!(panel.plain_lines().join("\n").contains("selected: 2. Save"));
    assert!(matches!(
        panel.handle_event(&Event::Paste("must-not-leak".to_owned())),
        ExportAction::None
    ));
    let _ = panel.handle_event(&key(KeyCode::Enter));
    assert!(panel.plain_lines().contains(&"Enter filename:".to_owned()));
    assert!(!panel.plain_lines().join("\n").contains("must-not-leak"));
}

#[test]
fn progress_surface_cancels_once_and_bounds_long_destination() {
    let destination = format!("nested/{}.txt", "x".repeat(300));
    let request = PlainTextExportRequest::new(&destination, Arc::from("exact")).expect("request");
    let cancellation = CancellationToken::new();
    let mut progress = PlainTextExportProgress::new(&request, cancellation.clone());
    assert_eq!(progress.desired_height(), 7);
    assert!(progress.plain_lines()[0].contains("Exporting"));
    assert!(matches!(
        progress.handle_event(&key(KeyCode::Char('x'))),
        ExportProgressAction::None
    ));
    assert!(!cancellation.is_cancelled());

    let backend = TestBackend::new(40, 7);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let styles = ExportPanelStyles::new(Color::White, Color::DarkGray, Color::LightBlue);
    terminal
        .draw(|frame| progress.draw(frame, frame.area(), styles))
        .expect("draw");
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Exporting conversation"));
    assert!(rendered.contains("Esc to cancel"));

    assert!(matches!(
        progress.handle_event(&key(KeyCode::Esc)),
        ExportProgressAction::CancellationRequested
    ));
    assert!(cancellation.is_cancelled());
    assert!(progress.plain_lines()[0].contains("Cancelling"));
    assert!(matches!(
        progress.handle_event(&key(KeyCode::Esc)),
        ExportProgressAction::None
    ));
    terminal
        .draw(|frame| progress.draw(frame, frame.area(), styles))
        .expect("draw cancelling");
    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("Cancelling conversation export"));
    assert!(rendered.contains("Cancelling safely"));
    progress.cancel();
    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn slow_filesystem_cancel_settles_without_a_late_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let policy = heycode_exec::FileSystemPolicy::new([heycode_exec::FileSystemRoot::new(
        root.path(),
        heycode_exec::FileSystemRootAccess::ReadWrite,
    )
    .expect("root")])
    .expect("policy");
    let inner = heycode_exec::FileSystemService::local(policy).expect("filesystem");
    let started = Arc::new(tokio::sync::Notify::new());
    let filesystem = heycode_exec::FileSystemService::new(Arc::new(DelayedFileSystem {
        inner,
        started: started.clone(),
    }));
    let request = PlainTextExportRequest::new("cancelled.txt", Arc::from("never committed"))
        .expect("request");
    let cancellation = CancellationToken::new();
    let mut progress = PlainTextExportProgress::new(&request, cancellation.clone());
    let cwd = root.path().to_path_buf();
    let task = tokio::spawn(async move {
        commit_plain_text_export(&filesystem, &cwd, request, cancellation).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("write started");
    assert!(!root.path().join("cancelled.txt").exists());
    assert!(matches!(
        progress.handle_event(&key(KeyCode::Esc)),
        ExportProgressAction::CancellationRequested
    ));
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("cancel settled")
        .expect("task joined")
        .expect_err("cancelled export must fail");
    assert!(matches!(
        error,
        ConversationExportError::Filesystem {
            code: heycode_exec::FileSystemErrorCode::Cancelled,
            ..
        }
    ));
    assert!(!root.path().join("cancelled.txt").exists());
    assert!(progress.plain_lines()[0].contains("Cancelling"));
}

#[test]
fn oversized_transcript_is_refused_without_truncation() {
    let text: Arc<str> = Arc::from("x".repeat(16 * 1024 * 1024 + 1));
    assert!(matches!(
        ConversationExportPanel::new(text, "conversation.txt"),
        Err(ConversationExportError::TooLarge { .. })
    ));
}
