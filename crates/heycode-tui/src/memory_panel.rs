//! Compact native chooser for attributed instruction and auto-memory sources.
//!
//! The panel snapshots only source metadata. Deliberate selection returns a
//! stable source id so the application can reuse `/memory show`; the view does
//! not open paths, mutate files, submit drafts, or invoke a model.

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::memory_commands::{
    MemoryManagerError, MemorySourceManager, MemorySourceStatus, MemorySourceView,
};

/// Application action produced by one consumed chooser event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryPanelAction {
    /// Keep the chooser open without opening a source.
    None,
    /// Close the chooser without changing the draft or transcript.
    Close,
    /// Close the chooser and render this exact source through `/memory show`.
    Show(String),
}

/// Read-only compact source picker backed by one live manager.
pub struct MemoryPanelView {
    manager: Arc<MemorySourceManager>,
    sources: Vec<MemorySourceView>,
    warnings: Vec<String>,
    selected: usize,
    offset: usize,
    area: Rect,
    rows_area: Rect,
    refresh_error: Option<String>,
}

impl MemoryPanelView {
    /// Create the chooser from a fresh source snapshot.
    ///
    /// # Errors
    /// Current workspace authority failure prevents opening an inaccurate
    /// chooser.
    pub fn new(manager: Arc<MemorySourceManager>) -> Result<Self, MemoryManagerError> {
        let mut snapshot = manager.snapshot()?;
        sort_sources_for_panel(&mut snapshot.sources);
        Ok(Self {
            manager,
            sources: snapshot.sources,
            warnings: snapshot.warnings,
            selected: 0,
            offset: 0,
            area: Rect::default(),
            rows_area: Rect::default(),
            refresh_error: None,
        })
    }

    /// Height that keeps the normal chooser compact while bounding large
    /// auto-memory inventories.
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        u16::try_from(self.sources.len())
            .unwrap_or(u16::MAX)
            .saturating_add(9)
            .clamp(11, 30)
    }

    /// Stable id of the current row, if the authority exposed any source.
    #[must_use]
    pub fn selected_source_id(&self) -> Option<&str> {
        self.sources.get(self.selected).map(MemorySourceView::id)
    }

    /// Refresh metadata in place while preserving selection by stable id.
    /// A failed refresh leaves the last safe snapshot visible and records an
    /// accessible error rather than silently replacing it with an empty list.
    pub fn refresh(&mut self) {
        let selected_id = self.selected_source_id().map(str::to_owned);
        match self.manager.snapshot() {
            Ok(snapshot) => {
                self.sources = snapshot.sources;
                sort_sources_for_panel(&mut self.sources);
                self.warnings = snapshot.warnings;
                self.selected = selected_id
                    .as_deref()
                    .and_then(|id| self.sources.iter().position(|source| source.id() == id))
                    .unwrap_or_else(|| self.selected.min(self.sources.len().saturating_sub(1)));
                self.offset = self.offset.min(self.selected);
                self.refresh_error = None;
            }
            Err(error) => self.refresh_error = Some(error.to_string()),
        }
    }

    /// Consume one modal event. Paste is intentionally ignored here so the
    /// application can route it to the open modal without mutating the draft.
    #[must_use]
    pub fn handle(&mut self, event: &Event) -> MemoryPanelAction {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return MemoryPanelAction::Close,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return MemoryPanelAction::Close;
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-10),
                KeyCode::PageDown => self.move_selection(10),
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = self.sources.len().saturating_sub(1),
                KeyCode::Enter => {
                    if let Some(id) = self.selected_source_id() {
                        return MemoryPanelAction::Show(id.to_owned());
                    }
                }
                KeyCode::Char('r')
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.refresh();
                }
                _ => {}
            },
            Event::Mouse(mouse) if self.area.contains((mouse.column, mouse.row).into()) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.move_selection(-3),
                    MouseEventKind::ScrollDown => self.move_selection(3),
                    MouseEventKind::Down(MouseButton::Left)
                        if self.rows_area.contains((mouse.column, mouse.row).into()) =>
                    {
                        let index = self.offset + usize::from(mouse.row - self.rows_area.y);
                        if index < self.sources.len() {
                            self.selected = index;
                        }
                    }
                    _ => {}
                }
            }
            Event::Paste(_) => {}
            _ => {}
        }
        MemoryPanelAction::None
    }

    /// Screen-reader projection of the same concise choices and selected source
    /// detail shown visually. Revisions and source content remain in `show`.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        let auto_memory = self
            .sources
            .iter()
            .filter(|source| source.kind() == crate::memory_commands::MemorySourceKind::AutoMemory)
            .count();
        let mut lines = vec![format!(
            "Memory — Auto-memory: {} — {} warnings",
            if auto_memory == 0 {
                "no sources".to_owned()
            } else {
                format!(
                    "{auto_memory} source{}",
                    if auto_memory == 1 { "" } else { "s" }
                )
            },
            self.warnings.len()
        )];
        if self.sources.is_empty() {
            lines.push("No memory sources are available in the current trusted scope.".to_owned());
        }
        for (index, source) in self.sources.iter().enumerate() {
            lines.push(format!(
                "{}{}. {} — {}",
                if index == self.selected {
                    "selected: "
                } else {
                    ""
                },
                index + 1,
                choice_label(source),
                choice_status(source)
            ));
        }
        if let Some(source) = self.sources.get(self.selected) {
            lines.push(format!("Selected source: {}", source_detail(source)));
        }
        lines.extend(
            self.warnings
                .iter()
                .map(|warning| format!("warning: {warning}")),
        );
        if let Some(error) = &self.refresh_error {
            lines.push(format!("refresh failed: {error}"));
        }
        lines.push(
            "keys: Up/Down, Home/End, PageUp/PageDown, or mouse wheel moves; click selects; Enter shows the selected source; R refreshes; Escape closes."
                .to_owned(),
        );
        lines
    }

    /// Render the compact chooser in the application-provided modal area.
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, styles: crate::terminal::Styles) {
        self.area = area;
        self.rows_area = Rect::default();
        if area.width < 2 || area.height < 9 {
            return;
        }

        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.accent())),
            area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Memory",
                Style::default()
                    .fg(styles.accent())
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        let auto_memory = self
            .sources
            .iter()
            .filter(|source| source.kind() == crate::memory_commands::MemorySourceKind::AutoMemory)
            .count();
        frame.render_widget(
            Paragraph::new(format!(
                "  Auto-memory: {}",
                if auto_memory == 0 {
                    "no sources".to_owned()
                } else {
                    format!(
                        "{auto_memory} source{}",
                        if auto_memory == 1 { "" } else { "s" }
                    )
                }
            ))
            .style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );

        let body = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(4),
            area.width.saturating_sub(4),
            area.height.saturating_sub(9),
        );
        self.rows_area = body;
        self.keep_selected_visible(usize::from(body.height));
        let rows = self
            .sources
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(usize::from(body.height))
            .map(|(index, source)| {
                let selected = index == self.selected;
                let style = if selected {
                    Style::default().fg(styles.accent())
                } else {
                    Style::default().fg(styles.text())
                };
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{}{}. {}  ",
                            if selected { "› " } else { "  " },
                            index + 1,
                            choice_label(source),
                        ),
                        style,
                    ),
                    Span::styled(choice_status(source), Style::default().fg(styles.dim())),
                ])
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new("No memory sources are available in this trusted scope.")
                    .style(Style::default().fg(styles.dim())),
                body,
            );
        } else {
            frame.render_widget(Paragraph::new(rows), body);
        }

        if let Some(source) = self.sources.get(self.selected) {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("  Source  ", Style::default().fg(styles.dim())),
                        Span::styled(source_location(source), Style::default().fg(styles.text())),
                    ]),
                    Line::from(vec![
                        Span::styled("  State   ", Style::default().fg(styles.dim())),
                        Span::styled(source_detail(source), Style::default().fg(styles.text())),
                    ]),
                ]),
                Rect::new(area.x, area.bottom().saturating_sub(3), area.width, 2),
            );
        }

        let footer = if area.width < 72 {
            "  ↑↓ move · Enter show · R refresh · Esc close"
        } else {
            "  ↑↓/wheel move · click select · Enter show source · R refresh · Esc close"
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }

    fn move_selection(&mut self, delta: isize) {
        if self.sources.is_empty() {
            return;
        }
        let last = self.sources.len() - 1;
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta.unsigned_abs()).min(last)
        };
    }

    fn keep_selected_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(visible_rows) {
            self.offset = self.selected + 1 - visible_rows;
        }
        self.offset = self
            .offset
            .min(self.sources.len().saturating_sub(visible_rows));
    }
}

fn status_text(status: &MemorySourceStatus) -> String {
    match status {
        MemorySourceStatus::Missing => "Missing".to_owned(),
        MemorySourceStatus::Ready { bytes, .. } => format!("Ready · {bytes} bytes"),
        MemorySourceStatus::Blocked { reason } => format!("Blocked · {reason}"),
    }
}

fn sort_sources_for_panel(sources: &mut [MemorySourceView]) {
    sources.sort_by_key(|source| match source.id() {
        "project:agents" => (0_u8, String::new()),
        "user:agents" => (1, String::new()),
        "project:heycode" => (2, String::new()),
        "project:claude" => (3, String::new()),
        "project:agents-local" => (4, String::new()),
        "project:claude-local" => (5, String::new()),
        _ => (6, choice_label(source)),
    });
}

fn choice_label(source: &MemorySourceView) -> String {
    match source.id() {
        "project:agents" => "Project instructions".to_owned(),
        "user:agents" => "User instructions".to_owned(),
        "project:heycode" => "heycode project instructions".to_owned(),
        "project:claude" => "Claude project instructions".to_owned(),
        "project:agents-local" => "Local project instructions".to_owned(),
        "project:claude-local" => "Local Claude instructions".to_owned(),
        _ => source.label().to_owned(),
    }
}

fn source_location(source: &MemorySourceView) -> String {
    match source.scope() {
        crate::memory_commands::MemorySourceScope::Project
            if source.kind() == crate::memory_commands::MemorySourceKind::Instructions =>
        {
            format!("./{}", source.label())
        }
        _ => source.label().to_owned(),
    }
}

fn choice_status(source: &MemorySourceView) -> String {
    match source.status() {
        MemorySourceStatus::Ready { .. }
            if source.kind() == crate::memory_commands::MemorySourceKind::Instructions =>
        {
            format!("Saved in {}", source_location(source))
        }
        MemorySourceStatus::Missing => format!("Not created ({})", source_location(source)),
        MemorySourceStatus::Ready { .. } => "Available".to_owned(),
        MemorySourceStatus::Blocked { .. } => "Unavailable".to_owned(),
    }
}

fn source_detail(source: &MemorySourceView) -> String {
    format!(
        "{} {} · {}",
        source.scope().as_str(),
        source.kind().as_str(),
        status_text(source.status())
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crossterm::event::{KeyEvent, MouseEvent};
    use heycode_prompt::instructions::InstructionSources;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::memory_commands::MemoryAuthority;

    struct FixedAuthority {
        sources: Mutex<InstructionSources>,
    }

    impl FixedAuthority {
        fn new(user_home: PathBuf, workspace: PathBuf) -> Self {
            Self {
                sources: Mutex::new(InstructionSources {
                    user_home: Some(user_home),
                    workspace: Some(workspace),
                }),
            }
        }
    }

    impl MemoryAuthority for FixedAuthority {
        fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
            Ok(self.sources.lock().unwrap().clone())
        }
    }

    struct SwitchableAuthority {
        sources: InstructionSources,
        unavailable: AtomicBool,
    }

    impl MemoryAuthority for SwitchableAuthority {
        fn instruction_sources(&self) -> Result<InstructionSources, MemoryManagerError> {
            if self.unavailable.load(Ordering::SeqCst) {
                Err(MemoryManagerError::AuthorityUnavailable)
            } else {
                Ok(self.sources.clone())
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, MemoryPanelView) {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("AGENTS.md"), "user law").unwrap();
        let manager = Arc::new(MemorySourceManager::new(Arc::new(FixedAuthority::new(
            home.path().to_path_buf(),
            project.path().to_path_buf(),
        ))));
        let view = MemoryPanelView::new(manager).unwrap();
        (home, project, view)
    }

    fn key(view: &mut MemoryPanelView, code: KeyCode) -> MemoryPanelAction {
        view.handle(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn accessible_rows_are_attributed_and_compact_without_paths_or_revisions() {
        let (home, _project, view) = fixture();
        let text = view.accessible_lines().join("\n");
        let revision = view.manager.read("user:agents").unwrap().revision;
        assert!(text.contains("selected: 1. Project instructions — Not created (./AGENTS.md)"));
        assert!(text.contains("2. User instructions — Saved in ~/.heycode/AGENTS.md"));
        assert!(text.contains("Selected source: project instructions · Missing"));
        assert!(!text.contains("user:agents"));
        assert!(!text.contains("project:agents"));
        assert!(!text.contains(&revision));
        assert!(!text.contains(&home.path().display().to_string()));
    }

    #[test]
    fn keyboard_navigation_returns_only_a_stable_id_and_paste_is_consumed() {
        let (_home, _project, mut view) = fixture();
        assert_eq!(key(&mut view, KeyCode::Down), MemoryPanelAction::None);
        assert_eq!(view.selected_source_id(), Some("user:agents"));
        assert_eq!(key(&mut view, KeyCode::End), MemoryPanelAction::None);
        assert_eq!(view.selected_source_id(), Some("project:claude-local"));
        assert_eq!(key(&mut view, KeyCode::Home), MemoryPanelAction::None);
        assert_eq!(view.selected_source_id(), Some("project:agents"));
        assert_eq!(
            key(&mut view, KeyCode::Enter),
            MemoryPanelAction::Show("project:agents".to_owned())
        );
        assert_eq!(
            view.handle(&Event::Paste("/memory clear project:agents".to_owned())),
            MemoryPanelAction::None
        );
        assert_eq!(key(&mut view, KeyCode::Esc), MemoryPanelAction::Close);
        assert_eq!(
            view.handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))),
            MemoryPanelAction::Close
        );
    }

    #[test]
    fn mouse_wheel_moves_and_click_selects_without_activating() {
        let (_home, _project, mut view) = fixture();
        let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                view.draw(frame, area, crate::terminal::Styles::default());
            })
            .unwrap();
        let rows = view.rows_area;
        assert_eq!(
            view.handle(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: rows.x,
                row: rows.y,
                modifiers: KeyModifiers::NONE,
            })),
            MemoryPanelAction::None
        );
        assert_eq!(view.selected_source_id(), Some("project:claude"));
        assert_eq!(
            view.handle(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rows.x + 1,
                row: rows.y + 1,
                modifiers: KeyModifiers::NONE,
            })),
            MemoryPanelAction::None
        );
        assert_eq!(view.selected_source_id(), Some("user:agents"));
    }

    #[test]
    fn refresh_preserves_id_and_updates_presence_without_exposing_revision() {
        let (_home, project, mut view) = fixture();
        assert_eq!(view.selected_source_id(), Some("project:agents"));
        std::fs::write(project.path().join("AGENTS.md"), "project law").unwrap();
        assert_eq!(key(&mut view, KeyCode::Char('r')), MemoryPanelAction::None);
        assert_eq!(view.selected_source_id(), Some("project:agents"));
        let text = view.accessible_lines().join("\n");
        assert!(text.contains("selected: 1. Project instructions — Saved in ./AGENTS.md"));
        assert!(text.contains("Selected source: project instructions · Ready · 11 bytes"));
        assert!(!text.contains(&view.manager.read("project:agents").unwrap().revision));
    }

    #[test]
    fn failed_refresh_retains_the_safe_snapshot_and_exposes_the_error() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("AGENTS.md"), "user law").unwrap();
        let authority = Arc::new(SwitchableAuthority {
            sources: InstructionSources {
                user_home: Some(home.path().to_path_buf()),
                workspace: Some(project.path().to_path_buf()),
            },
            unavailable: AtomicBool::new(false),
        });
        let manager = Arc::new(MemorySourceManager::new(authority.clone()));
        let mut view = MemoryPanelView::new(manager).unwrap();
        authority.unavailable.store(true, Ordering::SeqCst);

        assert_eq!(key(&mut view, KeyCode::Char('r')), MemoryPanelAction::None);
        let text = view.accessible_lines().join("\n");
        assert!(
            text.contains("User instructions — Saved in ~/.heycode/AGENTS.md"),
            "{text}"
        );
        assert!(
            text.contains("refresh failed: memory authority is unavailable"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_source_is_visible_as_blocked_without_exposing_its_target() {
        use std::os::unix::fs::symlink;

        let (home, _project, mut view) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("private.txt");
        std::fs::write(&target, "outside").unwrap();
        std::fs::remove_file(home.path().join("AGENTS.md")).unwrap();
        symlink(&target, home.path().join("AGENTS.md")).unwrap();
        view.refresh();
        key(&mut view, KeyCode::Down);

        let text = view.accessible_lines().join("\n");
        assert!(text.contains("User instructions — Unavailable"), "{text}");
        assert!(
            text.contains("Blocked · unsafe file or directory topology"),
            "{text}"
        );
        assert!(!text.contains(&target.display().to_string()), "{text}");
    }

    #[test]
    fn narrow_render_keeps_selection_and_the_compact_controls_visible() {
        let (_home, _project, mut view) = fixture();
        key(&mut view, KeyCode::Down);
        let mut terminal = Terminal::new(TestBackend::new(45, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                view.draw(frame, area, crate::terminal::Styles::default());
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered.contains("Memory"), "{rendered}");
        assert!(rendered.contains("User instructions"), "{rendered}");
        assert!(!rendered.contains("user:agents"), "{rendered}");
        assert!(rendered.contains("↑↓ move · Enter show"), "{rendered}");
    }
}
