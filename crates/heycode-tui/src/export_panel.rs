//! Source-grounded plain-text conversation export chooser.
//!
//! The panel owns no application transcript, clipboard, command parser, or
//! durable session exporter. Its input is one immutable plain-text snapshot;
//! its output is a deliberate clipboard or create-only file action. This keeps
//! the source-style current-conversation export separate from the existing
//! lossless JSONL, Markdown, support, and session-browser export paths.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use heycode_exec::{
    CheckedWriteSpec, FileSystemError, FileSystemErrorCode, FileSystemService, PathRequest,
    WriteFileSpec,
};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MAX_EXPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DESTINATION_BYTES: usize = 4_096;

/// Colors supplied by the application-owned display mode.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExportPanelStyles {
    text: Color,
    dim: Color,
    accent: Color,
}

impl ExportPanelStyles {
    /// Bind the current theme without coupling the panel to application state.
    #[must_use]
    pub(crate) const fn new(text: Color, dim: Color, accent: Color) -> Self {
        Self { text, dim, accent }
    }
}

/// One side-effect-free action returned to the owning application loop.
#[derive(Debug, Clone)]
pub(crate) enum ExportAction {
    /// Keep the chooser open.
    None,
    /// Close without copying or writing.
    Cancel,
    /// Copy the exact immutable transcript snapshot.
    Copy { transcript: Arc<str> },
    /// Commit through the application-owned filesystem service.
    Save(PlainTextExportRequest),
}

/// One input outcome while an asynchronous file export owns the modal surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportProgressAction {
    /// Keep waiting for the filesystem operation.
    None,
    /// The user asked the active operation to cancel.
    CancellationRequested,
}

/// Create-only plain-text export intent.
#[derive(Clone)]
pub(crate) struct PlainTextExportRequest {
    destination: PathBuf,
    transcript: Arc<str>,
}

impl PlainTextExportRequest {
    /// Validate and normalize one explicit destination.
    ///
    /// A missing extension becomes `.txt`. Existing targets are not inspected
    /// here; commit keeps that check atomic at the filesystem provider.
    pub(crate) fn new(
        destination: &str,
        transcript: Arc<str>,
    ) -> Result<Self, ConversationExportError> {
        validate_transcript(&transcript)?;
        let destination = normalize_destination(destination)?;
        Ok(Self {
            destination,
            transcript,
        })
    }

    /// Caller-supplied path after source-compatible `.txt` normalization.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }

    /// Exact opaque bytes supplied by the application transcript planner.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn transcript(&self) -> &str {
        &self.transcript
    }
}

impl std::fmt::Debug for PlainTextExportRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlainTextExportRequest")
            .field("destination", &self.destination)
            .field("transcript_bytes", &self.transcript.len())
            .finish()
    }
}

/// Successful create-only export receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlainTextExportReceipt {
    path: PathBuf,
    byte_len: usize,
}

impl PlainTextExportReceipt {
    /// Exact provider-resolved destination.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Exact committed transcript byte count.
    #[must_use]
    pub(crate) const fn byte_len(&self) -> usize {
        self.byte_len
    }
}

/// Visible, cancellable lifecycle for one asynchronous create-only file export.
///
/// The application owns the task join. This value owns a clone of the same
/// cancellation token, prevents composer input from leaking through while the
/// task is unsettled, and renders only bounded request metadata — never the
/// conversation body.
pub(crate) struct PlainTextExportProgress {
    destination: String,
    byte_len: usize,
    cancellation: CancellationToken,
    cancelling: bool,
}

impl PlainTextExportProgress {
    /// Bind the immutable request metadata to the task's cancellation token.
    #[must_use]
    pub(crate) fn new(request: &PlainTextExportRequest, cancellation: CancellationToken) -> Self {
        Self {
            destination: request.destination.to_string_lossy().into_owned(),
            byte_len: request.transcript.len(),
            cancellation,
            cancelling: false,
        }
    }

    /// Height of the active-operation surface.
    #[must_use]
    pub(crate) const fn desired_height(&self) -> u16 {
        7
    }

    /// Stable flat projection for accessibility and lifecycle assertions.
    #[must_use]
    pub(crate) fn plain_lines(&self) -> Vec<String> {
        vec![
            if self.cancelling {
                "Cancelling conversation export".to_owned()
            } else {
                "Exporting conversation".to_owned()
            },
            format!("Destination: {}", self.destination),
            format!("Payload: {} bytes", self.byte_len),
            if self.cancelling {
                "Waiting for the filesystem operation to stop safely.".to_owned()
            } else {
                "Escape: cancel.".to_owned()
            },
        ]
    }

    /// Consume all modal input and request cancellation on Escape or Ctrl+C.
    #[must_use]
    pub(crate) fn handle_event(&mut self, event: &Event) -> ExportProgressAction {
        let cancel = matches!(
            event,
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && (key.code == KeyCode::Esc
                        || (key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')))
        );
        if cancel && !self.cancelling {
            self.cancelling = true;
            self.cancellation.cancel();
            ExportProgressAction::CancellationRequested
        } else {
            ExportProgressAction::None
        }
    }

    /// Request safe teardown when the owning application exits.
    pub(crate) fn cancel(&mut self) {
        self.cancelling = true;
        self.cancellation.cancel();
    }

    /// Draw the active lifecycle without exposing or copying transcript bytes.
    pub(crate) fn draw(&self, frame: &mut Frame<'_>, area: Rect, styles: ExportPanelStyles) {
        if area.width < 4 || area.height < 5 {
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.dim)),
            area,
        );
        let title = if self.cancelling {
            "  Cancelling conversation export"
        } else {
            "  Exporting conversation"
        };
        frame.render_widget(
            Paragraph::new(title).style(
                Style::default()
                    .fg(styles.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
        let detail_width = usize::from(area.width.saturating_sub(4));
        let destination = truncate_display(&self.destination, detail_width);
        frame.render_widget(
            Paragraph::new(format!("  {destination}")).style(Style::default().fg(styles.text)),
            Rect::new(area.x, area.y.saturating_add(3), area.width, 1),
        );
        let footer = if self.cancelling {
            format!("  Cancelling safely · {} bytes", self.byte_len)
        } else {
            format!("  {} bytes · Esc to cancel", self.byte_len)
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(styles.dim)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }
}

/// Input validation or safe filesystem refusal.
#[derive(Debug, Error)]
pub(crate) enum ConversationExportError {
    /// The transcript cannot be copied or written without truncation.
    #[error("conversation is {actual} bytes; the export limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },
    /// A destination is required before save can begin.
    #[error("enter a filename")]
    EmptyDestination,
    /// The terminal-owned destination must be one display-safe path.
    #[error("filename must be a single line without control characters")]
    InvalidDestination,
    /// The active filesystem provider refused the request with a fixed code.
    #[error("{message}")]
    Filesystem {
        /// Stable provider failure code for logging and tests.
        code: FileSystemErrorCode,
        message: &'static str,
    },
}

impl From<FileSystemError> for ConversationExportError {
    fn from(error: FileSystemError) -> Self {
        let code = error.code();
        let message = match code {
            FileSystemErrorCode::ChangedAtCommit => {
                "the destination already exists or changed; choose a new filename"
            }
            FileSystemErrorCode::NotFile => "the destination is not a regular file",
            FileSystemErrorCode::NotDirectory => "a destination parent is not a directory",
            FileSystemErrorCode::OutsideAllowedRoots => {
                "the destination is outside the allowed filesystem roots"
            }
            FileSystemErrorCode::PathTraversal => {
                "the destination crosses a protected path or symlink boundary"
            }
            FileSystemErrorCode::ReadOnlyRoot => "the destination is under a read-only root",
            FileSystemErrorCode::PermissionDenied => {
                "permission to write the destination was denied"
            }
            FileSystemErrorCode::Cancelled => "the export was cancelled before commit",
            FileSystemErrorCode::ServiceStopped => "the filesystem service is unavailable",
            FileSystemErrorCode::UnsupportedOperation => {
                "the active filesystem does not support create-only export"
            }
            _ => "the filesystem could not commit the export",
        };
        Self::Filesystem { code, message }
    }
}

/// Resolve and atomically create one export through the active filesystem.
///
/// The provider owns allowed-root, symlink, parent creation, race, permission,
/// and cancellation policy. `CheckedWriteSpec::create` refuses every existing
/// file or directory rather than overwriting conversation data.
pub(crate) async fn commit_plain_text_export(
    filesystem: &FileSystemService,
    cwd: &Path,
    request: PlainTextExportRequest,
    cancellation: CancellationToken,
) -> Result<PlainTextExportReceipt, ConversationExportError> {
    validate_transcript(&request.transcript)?;
    let resolved = filesystem.resolve(PathRequest::new(cwd, &request.destination)?)?;
    let spec = WriteFileSpec::new(resolved.clone(), request.transcript.as_bytes())?;
    let output = filesystem
        .write_checked(CheckedWriteSpec::create(spec), cancellation)
        .await?;
    Ok(PlainTextExportReceipt {
        path: resolved.as_path().to_path_buf(),
        byte_len: output.bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportPhase {
    Method,
    Filename,
}

/// Source-faithful chooser for the current rendered conversation.
pub(crate) struct ConversationExportPanel {
    transcript: Arc<str>,
    phase: ExportPhase,
    selected: usize,
    filename: TextArea<'static>,
    error: Option<String>,
    area: Rect,
    method_hits: [Option<Rect>; 2],
}

impl ConversationExportPanel {
    /// Open on Copy to clipboard with an independent generated filename draft.
    pub(crate) fn new(
        transcript: Arc<str>,
        suggested_filename: impl Into<String>,
    ) -> Result<Self, ConversationExportError> {
        validate_transcript(&transcript)?;
        let suggested_filename = normalize_destination(&suggested_filename.into())?;
        let mut filename = TextArea::from(vec![suggested_filename.to_string_lossy().into_owned()]);
        filename.set_cursor_line_style(Style::default());
        filename.move_cursor(tui_textarea::CursorMove::End);
        Ok(Self {
            transcript,
            phase: ExportPhase::Method,
            selected: 0,
            filename,
            error: None,
            area: Rect::default(),
            method_hits: [None, None],
        })
    }

    /// Height of the current source-style panel.
    #[must_use]
    pub(crate) const fn desired_height(&self) -> u16 {
        match self.phase {
            ExportPhase::Method => 9,
            ExportPhase::Filename => 10,
        }
    }

    /// Flat projection for screen-reader presentation and assertions.
    #[must_use]
    pub(crate) fn plain_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Export conversation".to_owned(),
            "Select export method".to_owned(),
        ];
        match self.phase {
            ExportPhase::Method => {
                lines.push(format!(
                    "{}1. Copy to clipboard — Copy the conversation to your system clipboard",
                    if self.selected == 0 { "selected: " } else { "" }
                ));
                lines.push(format!(
                    "{}2. Save to file — Save the conversation to a file in the current directory",
                    if self.selected == 1 { "selected: " } else { "" }
                ));
                lines.push("Up/Down: choose. Enter: select. Escape: cancel.".to_owned());
            }
            ExportPhase::Filename => {
                lines.push("Enter filename:".to_owned());
                lines.push(self.filename_text().to_owned());
                lines.push("Enter: save. Escape: go back.".to_owned());
            }
        }
        if let Some(error) = &self.error {
            lines.push(error.clone());
        }
        lines
    }

    /// Consume one modal event without touching the clipboard or filesystem.
    #[must_use]
    pub(crate) fn handle_event(&mut self, event: &Event) -> ExportAction {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(*key),
            Event::Mouse(mouse) => {
                if !self.area.contains(Position::new(mouse.column, mouse.row)) {
                    return ExportAction::None;
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp if self.phase == ExportPhase::Method => {
                        self.selected = 0;
                    }
                    MouseEventKind::ScrollDown if self.phase == ExportPhase::Method => {
                        self.selected = 1;
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if self.phase == ExportPhase::Method =>
                    {
                        let point = Position::new(mouse.column, mouse.row);
                        if let Some(index) = self
                            .method_hits
                            .iter()
                            .position(|area| area.is_some_and(|area| area.contains(point)))
                        {
                            self.selected = index;
                        }
                    }
                    _ => {}
                }
                ExportAction::None
            }
            Event::Paste(text) if self.phase == ExportPhase::Filename => {
                self.insert_filename(text);
                ExportAction::None
            }
            Event::Paste(_) => ExportAction::None,
            _ => ExportAction::None,
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> ExportAction {
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            self.error = None;
            return if self.phase == ExportPhase::Filename {
                self.phase = ExportPhase::Method;
                self.selected = 1;
                ExportAction::None
            } else {
                ExportAction::Cancel
            };
        }
        match self.phase {
            ExportPhase::Method => match key.code {
                KeyCode::Up | KeyCode::Home => self.selected = 0,
                KeyCode::Down | KeyCode::End => self.selected = 1,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                    self.selected = usize::from(self.selected == 0);
                }
                KeyCode::Enter if self.selected == 0 => {
                    return ExportAction::Copy {
                        transcript: self.transcript.clone(),
                    };
                }
                KeyCode::Enter => {
                    self.phase = ExportPhase::Filename;
                    self.error = None;
                }
                _ => {}
            },
            ExportPhase::Filename => {
                if key.code == KeyCode::Enter {
                    return match PlainTextExportRequest::new(
                        self.filename_text().trim(),
                        self.transcript.clone(),
                    ) {
                        Ok(request) => ExportAction::Save(request),
                        Err(error) => {
                            self.error = Some(error.to_string());
                            ExportAction::None
                        }
                    };
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
                    self.filename.delete_line_by_head();
                    self.error = None;
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
                    self.filename.input(key);
                    self.error = None;
                } else if let KeyCode::Char(ch) = key.code
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    self.insert_filename(&ch.to_string());
                }
            }
        }
        ExportAction::None
    }

    fn insert_filename(&mut self, text: &str) {
        let current = self.filename_text();
        if text.chars().any(char::is_control)
            || current.len().saturating_add(text.len()) > MAX_DESTINATION_BYTES
        {
            self.error = Some(
                "Filename must be one line of at most 4096 bytes without control characters."
                    .to_owned(),
            );
            return;
        }
        self.filename.insert_str(text);
        self.error = None;
    }

    fn filename_text(&self) -> &str {
        self.filename.lines().first().map_or("", String::as_str)
    }

    /// Draw the source-ordered chooser inside the application-owned modal area.
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, styles: ExportPanelStyles) {
        self.area = area;
        self.method_hits = [None, None];
        if area.width < 4 || area.height < 6 {
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.dim)),
            area,
        );
        frame.render_widget(
            Paragraph::new("  Export conversation").style(
                Style::default()
                    .fg(styles.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
        frame.render_widget(
            Paragraph::new("  Select export method").style(Style::default().fg(styles.dim)),
            Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
        );
        match self.phase {
            ExportPhase::Method => self.draw_methods(frame, area, styles),
            ExportPhase::Filename => self.draw_filename(frame, area, styles),
        }
    }

    fn draw_methods(&mut self, frame: &mut Frame<'_>, area: Rect, styles: ExportPanelStyles) {
        let rows = [
            (
                "Copy to clipboard",
                "Copy the conversation to your system clipboard",
            ),
            (
                "Save to file",
                "Save the conversation to a file in the current directory",
            ),
        ];
        for (index, (label, description)) in rows.into_iter().enumerate() {
            let y = area
                .y
                .saturating_add(4)
                .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
            let row = Rect::new(area.x.saturating_add(2), y, area.width.saturating_sub(4), 1);
            self.method_hits[index] = Some(row);
            let marker = if self.selected == index { "› " } else { "  " };
            let number = index + 1;
            let label_width = 21_usize.min(usize::from(row.width).saturating_sub(5));
            let label = pad_display(&truncate_display(label, label_width), label_width);
            let prefix = format!("{marker}{number}. {label}  ");
            let description = truncate_display(
                description,
                usize::from(row.width).saturating_sub(prefix.width()),
            );
            let label_style = if self.selected == index {
                Style::default()
                    .fg(styles.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(styles.text)
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, label_style),
                    Span::styled(description, Style::default().fg(styles.dim)),
                ])),
                row,
            );
        }
        frame.render_widget(
            Paragraph::new("  Esc to cancel").style(Style::default().fg(styles.dim)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }

    fn draw_filename(&mut self, frame: &mut Frame<'_>, area: Rect, styles: ExportPanelStyles) {
        frame.render_widget(
            Paragraph::new("  Enter filename:").style(Style::default().fg(styles.text)),
            Rect::new(area.x, area.y.saturating_add(4), area.width, 1),
        );
        let input = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(6),
            area.width.saturating_sub(4),
            1,
        );
        let max_text_width = usize::from(input.width).saturating_sub(2);
        let (visible, cursor) = filename_window(
            self.filename_text(),
            self.filename.cursor().1,
            max_text_width,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(styles.accent)),
                Span::styled(visible, Style::default().fg(styles.text)),
            ])),
            input,
        );
        let cursor_x = input
            .x
            .saturating_add(2)
            .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX))
            .min(input.right().saturating_sub(1));
        frame.set_cursor_position((cursor_x, input.y));
        let footer = self
            .error
            .as_deref()
            .unwrap_or("Enter to save · Esc to go back");
        frame.render_widget(
            Paragraph::new(format!("  {footer}")).style(Style::default().fg(styles.dim)),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }
}

fn validate_transcript(transcript: &str) -> Result<(), ConversationExportError> {
    if transcript.len() > MAX_EXPORT_BYTES {
        return Err(ConversationExportError::TooLarge {
            actual: transcript.len(),
            limit: MAX_EXPORT_BYTES,
        });
    }
    Ok(())
}

fn normalize_destination(raw: &str) -> Result<PathBuf, ConversationExportError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ConversationExportError::EmptyDestination);
    }
    if raw.len() > MAX_DESTINATION_BYTES || raw.chars().any(char::is_control) {
        return Err(ConversationExportError::InvalidDestination);
    }
    let unquoted = if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\'')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    if unquoted.is_empty() {
        return Err(ConversationExportError::EmptyDestination);
    }
    let mut path = PathBuf::from(unquoted);
    if path.file_name().is_none() {
        return Err(ConversationExportError::InvalidDestination);
    }
    if path.extension().is_none() {
        path.set_extension("txt");
    }
    Ok(path)
}

fn truncate_display(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    value
        .chars()
        .scan(0_usize, |used, ch| {
            let next = used.saturating_add(ch.width().unwrap_or(0));
            if next > width {
                None
            } else {
                *used = next;
                Some(ch)
            }
        })
        .collect()
}

fn pad_display(value: &str, width: usize) -> String {
    format!("{value}{}", " ".repeat(width.saturating_sub(value.width())))
}

fn filename_window(value: &str, cursor_chars: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let cursor_byte = value
        .char_indices()
        .nth(cursor_chars)
        .map_or(value.len(), |(byte, _)| byte);
    let prefix = &value[..cursor_byte];
    let cursor_width = prefix.width();
    if value.width() <= width {
        return (value.to_owned(), cursor_width.min(width.saturating_sub(1)));
    }
    let keep_before = width.saturating_sub(1);
    let mut used = 0_usize;
    let start = prefix
        .char_indices()
        .rev()
        .find_map(|(byte, ch)| {
            let next = used.saturating_add(ch.width().unwrap_or(0));
            if next > keep_before {
                Some(byte + ch.len_utf8())
            } else {
                used = next;
                None
            }
        })
        .unwrap_or(0);
    let visible = truncate_display(&value[start..], width);
    (visible, used.min(width.saturating_sub(1)))
}
