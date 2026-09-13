//! Read-only, keyboard and mouse command help from one live catalog snapshot.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use heycode_agent::CommandCatalogEntry;
use heycode_ui::keymap::Keymap;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

pub(crate) struct HelpView {
    header: String,
    commands: Vec<CommandCatalogEntry>,
    custom_rows: Vec<CustomHelpRow>,
    custom_notice: Option<String>,
    columns: crate::composer_shortcuts::ShortcutColumns,
    tab: HelpTab,
    query: String,
    offset: usize,
    maximum_offset: usize,
    area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTab {
    General,
    Commands,
    Custom,
}

impl HelpTab {
    const fn next(self) -> Self {
        match self {
            Self::General => Self::Commands,
            Self::Commands => Self::Custom,
            Self::Custom => Self::General,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::General => Self::Custom,
            Self::Commands => Self::General,
            Self::Custom => Self::Commands,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Commands => "Commands",
            Self::Custom => "Custom commands",
        }
    }
}

/// One real custom command or discovered skill exposed by the host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CustomHelpRow {
    invocation: String,
    description: String,
    source: String,
}

impl CustomHelpRow {
    /// Build a bounded terminal-safe row. Description may be empty, while the
    /// invocable text and its admitted source must both be present.
    pub(crate) fn new(
        invocation: impl AsRef<str>,
        description: impl AsRef<str>,
        source: impl AsRef<str>,
    ) -> Option<Self> {
        let invocation = bounded_text(invocation.as_ref(), 160);
        let description = bounded_text(description.as_ref(), 240);
        let source = bounded_text(source.as_ref(), 120);
        if invocation.is_empty() || source.is_empty() {
            return None;
        }
        Some(Self {
            invocation,
            description,
            source,
        })
    }

    fn help_line(&self) -> String {
        format!("{} {} {}", self.invocation, self.description, self.source)
    }
}

impl HelpView {
    pub(crate) fn new(
        header: String,
        mut commands: Vec<CommandCatalogEntry>,
        keymap: &Keymap,
    ) -> Self {
        commands.sort_by(|a, b| a.descriptor.id().cmp(b.descriptor.id()));
        Self {
            header,
            commands,
            custom_rows: Vec::new(),
            custom_notice: None,
            // One shortcut list, shared with the standalone `?` panel, so the
            // two can never drift into naming different bindings.
            columns: crate::composer_shortcuts::ShortcutColumns::resolve(keymap),
            tab: HelpTab::General,
            query: String::new(),
            offset: 0,
            maximum_offset: 0,
            area: Rect::default(),
        }
    }

    /// Attach the exact installed plugin-command and discovered-skill rows
    /// supplied by the application snapshot.
    pub(crate) fn with_custom_rows(mut self, mut rows: Vec<CustomHelpRow>) -> Self {
        rows.sort();
        rows.dedup();
        self.custom_rows = rows;
        self
    }

    /// Name a failed custom-source snapshot instead of presenting it as an
    /// honestly empty inventory.
    pub(crate) fn with_custom_notice(mut self, notice: Option<String>) -> Self {
        self.custom_notice = notice
            .as_deref()
            .map(|value| bounded_text(value, 240))
            .filter(|value| !value.is_empty());
        self
    }

    fn switch_tab(&mut self, tab: HelpTab) {
        self.tab = tab;
        self.offset = 0;
    }

    pub(crate) fn desired_height(&self) -> u16 {
        // General is one sentence, the shortcut grid and one closing line; the
        // searchable tabs want every row they can get.
        if self.tab == HelpTab::General { 17 } else { 38 }
    }

    fn content(&self, width: u16) -> Vec<String> {
        match self.tab {
            HelpTab::General => self.general_content(width),
            HelpTab::Commands => self.command_content(),
            HelpTab::Custom => self.custom_content(),
        }
    }

    /// The reference's General tab: one sentence, a `Shortcuts` heading, the
    /// three-column binding grid, and one place to look next. Route and model
    /// stay out of it because the shell header already carries them.
    fn general_content(&self, width: u16) -> Vec<String> {
        let mut lines = vec![
            "heycode works with your codebase, makes edits with your permission, and runs commands \
             — right from your terminal."
                .to_owned(),
            "Shortcuts".to_owned(),
        ];
        lines.extend(
            self.columns
                .rows(width)
                .into_iter()
                .map(|row| row.strip_prefix("  ").unwrap_or(&row).trim_end().to_owned()),
        );
        lines.push(String::new());
        lines.push(
            "For more help: Tab for every command, or `heycode --help` for command-line options."
                .to_owned(),
        );
        lines
    }

    fn command_content(&self) -> Vec<String> {
        let query = self.query.to_lowercase();
        let mut lines = vec![format!("Search commands: {}", self.query)];
        let matching = self
            .commands
            .iter()
            .filter(|entry| entry.help_line().to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        lines.push(format!(
            "{} of {} commands · unavailable commands include a reason",
            matching.len(),
            self.commands.len()
        ));
        lines.push(String::new());
        if matching.is_empty() {
            lines.push(
                "No matching commands. Backspace edits the search; Escape closes help.".to_owned(),
            );
        } else {
            for entry in matching {
                lines.push(entry.descriptor.synopsis());
                lines.push(format!("  {}", entry.descriptor.description()));
                if !entry.descriptor.aliases().is_empty() {
                    lines.push(format!(
                        "  aliases: {}",
                        entry
                            .descriptor
                            .aliases()
                            .iter()
                            .map(|alias| format!("/{alias}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if let Some(reason) = entry.availability.reason() {
                    lines.push(format!("  unavailable: {reason}"));
                }
            }
        }
        lines
    }

    fn custom_content(&self) -> Vec<String> {
        let query = self.query.to_lowercase();
        let matching = self
            .custom_rows
            .iter()
            .filter(|row| row.help_line().to_lowercase().contains(&query))
            .collect::<Vec<_>>();
        let mut lines = vec![
            format!("Search custom commands: {}", self.query),
            format!(
                "{} of {} installed custom commands and skills",
                matching.len(),
                self.custom_rows.len()
            ),
            String::new(),
        ];
        if let Some(notice) = self.custom_notice.as_deref() {
            lines.push(format!("Custom inventory unavailable: {notice}"));
            lines.push(String::new());
        }
        if self.custom_rows.is_empty() {
            if self.custom_notice.is_none() {
                lines.push("No installed custom commands or skills were discovered.".to_owned());
            }
        } else if matching.is_empty() {
            lines.push(
                "No matching custom entries. Backspace edits the search; Escape closes help."
                    .to_owned(),
            );
        } else {
            for row in matching {
                lines.push(row.invocation.clone());
                if !row.description.is_empty() {
                    lines.push(format!("  {}", row.description));
                }
                lines.push(format!("  source: {}", row.source));
            }
        }
        lines
    }

    pub(crate) fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec![format!("Help — {}", self.tab.label())];
        lines.extend(self.header.lines().map(str::to_owned));
        lines.extend(self.content(80));
        lines.push("keys: Tab or Left/Right changes tab; type to search Commands or Custom commands; Up/Down or mouse wheel scrolls; Escape closes without changing the draft.".to_owned());
        lines
    }

    /// Consume modal input. True requests closure; this view never runs a command.
    pub(crate) fn handle(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
                KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.switch_tab(self.tab.previous())
                }
                KeyCode::Tab => self.switch_tab(self.tab.next()),
                KeyCode::BackTab => self.switch_tab(self.tab.previous()),
                KeyCode::Left => self.switch_tab(self.tab.previous()),
                KeyCode::Right => self.switch_tab(self.tab.next()),
                KeyCode::Enter if self.tab == HelpTab::General => {
                    self.switch_tab(HelpTab::Commands)
                }
                KeyCode::Up => self.offset = self.offset.saturating_sub(1),
                KeyCode::Down => {
                    self.offset = self.offset.saturating_add(1).min(self.maximum_offset)
                }
                KeyCode::PageUp => self.offset = self.offset.saturating_sub(10),
                KeyCode::PageDown => {
                    self.offset = self.offset.saturating_add(10).min(self.maximum_offset)
                }
                KeyCode::Home => self.offset = 0,
                KeyCode::End => self.offset = self.maximum_offset,
                KeyCode::Backspace if self.tab != HelpTab::General => {
                    self.query.pop();
                    self.offset = 0;
                }
                KeyCode::Char(character)
                    if !character.is_control()
                        && !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                {
                    if self.tab == HelpTab::General {
                        self.switch_tab(HelpTab::Commands);
                    }
                    if self.query.chars().count() < 128 {
                        self.query.push(character);
                    }
                }
                _ => {}
            },
            Event::Mouse(mouse) if self.area.contains((mouse.column, mouse.row).into()) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.offset = self.offset.saturating_sub(3),
                    MouseEventKind::ScrollDown => {
                        self.offset = self.offset.saturating_add(3).min(self.maximum_offset)
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if mouse.row == self.area.y.saturating_add(1) =>
                    {
                        let column = mouse.column.saturating_sub(self.area.x);
                        if (7..17).contains(&column) {
                            self.switch_tab(HelpTab::General);
                        }
                        if (17..28).contains(&column) {
                            self.switch_tab(HelpTab::Commands);
                        }
                        if (28..46).contains(&column) {
                            self.switch_tab(HelpTab::Custom);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        false
    }

    pub(crate) fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        styles: crate::terminal::Styles,
    ) {
        self.area = area;
        if area.width < 2 || area.height < 3 {
            return;
        }
        frame.render_widget(Clear, area);
        // One full-width rule opens every command panel; see `panel_frame`.
        frame.render_widget(
            Paragraph::new(crate::panel_frame::top_rule(area.width, styles)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let normal = Style::default().fg(styles.text());
        // The shared panel frame owns the tab strip: title, then one reversed
        // chip on the active tab, at the reference's own column origins.
        frame.render_widget(
            Paragraph::new(crate::panel_frame::title_with_tabs(
                "Help",
                &["General", "Commands", "Custom commands"],
                match self.tab {
                    HelpTab::General => 0,
                    HelpTab::Commands => 1,
                    HelpTab::Custom => 2,
                },
                styles,
            )),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        let body = Rect::new(
            area.x + 3,
            area.y + 3,
            area.width.saturating_sub(4),
            area.height.saturating_sub(5),
        );
        let rows = wrap_lines(self.content(body.width), usize::from(body.width.max(1)));
        self.maximum_offset = rows.len().saturating_sub(usize::from(body.height));
        self.offset = self.offset.min(self.maximum_offset);
        let visible = rows
            .into_iter()
            .skip(self.offset)
            .take(usize::from(body.height))
            .map(|row| {
                let style = if row == "Shortcuts" {
                    normal.add_modifier(Modifier::BOLD)
                } else if row.starts_with("  ") {
                    Style::default().fg(styles.dim())
                } else {
                    normal
                };
                Line::styled(row, style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible).style(normal), body);
        let footer = if self.tab == HelpTab::General {
            "   Esc to cancel"
        } else if area.width < 65 {
            "   Tab: tabs · ↑↓: scroll · Esc: close"
        } else {
            "   Tab: switch · type: search · ↑↓: scroll · Esc: close"
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn wrap_lines(lines: Vec<String>, width: usize) -> Vec<String> {
    lines
        .into_iter()
        .flat_map(|line| {
            let hanging = if line.starts_with("  ") { 2 } else { 0 };
            let rows = if line.is_empty() {
                vec![Line::default()]
            } else {
                crate::markdown::wrap_styled(&[Span::raw(line)], width.max(1), hanging)
            };
            rows.into_iter().map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
        })
        .collect()
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    crate::markdown::terminal_safe_span(value.trim())
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{ScreenReaderSnapshot, app::AppState};
    use crossterm::event::{KeyEvent, MouseEvent};
    use heycode_agent::{
        CommandAvailability, CommandDescriptor, CommandSource, CommandTiming, UiEvent,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn request() -> UiEvent {
        UiEvent::HelpRequested {
            header: "provider: fake\nmodel: test".to_owned(),
            commands: vec![CommandCatalogEntry {
                descriptor: CommandDescriptor::new(
                    "new",
                    "Create a conversation",
                    vec![],
                    CommandTiming::Immediate,
                    CommandSource::from_plugin("test").unwrap(),
                )
                .unwrap(),
                availability: CommandAvailability::unavailable("session owner is unavailable")
                    .unwrap(),
            }],
        }
    }

    fn key(state: &mut AppState, code: KeyCode) {
        state.handle_terminal_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    #[test]
    fn help_modal_searches_aliases_and_preserves_draft_without_submitting() {
        let mut state = AppState::new("test", "/workspace".into());
        state.input.insert_str("draft stays intact");
        state.apply(&request());
        let opened = ScreenReaderSnapshot::from_state(&state);
        assert!(
            opened.as_text().contains("Shortcuts"),
            "{}",
            opened.as_text()
        );
        assert!(
            opened.as_text().contains("For more help:"),
            "{}",
            opened.as_text()
        );
        key(&mut state, KeyCode::Tab);
        for character in "reset".chars() {
            key(&mut state, KeyCode::Char(character));
        }
        let flat = ScreenReaderSnapshot::from_state(&state);
        assert!(flat.as_text().contains("/clear, /reset"));
        assert!(flat.as_text().contains("session owner is unavailable"));
        key(&mut state, KeyCode::Enter);
        assert!(state.items.is_empty());
        key(&mut state, KeyCode::Esc);
        assert!(state.help_panel.is_none());
        assert_eq!(state.input.lines(), &["draft stays intact"]);
    }

    #[test]
    fn help_modal_mouse_tabs_narrow_render_and_empty_search_remain_usable() {
        let mut state = AppState::new("test", "/workspace".into());
        state.apply(&request());
        let mut terminal = Terminal::new(TestBackend::new(45, 24)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        let area = state.help_panel.as_ref().unwrap().area;
        let footer = (area.x..area.right())
            .map(|x| terminal.backend().buffer()[(x, area.bottom() - 1)].symbol())
            .collect::<String>();
        // The General tab closes with the reference's own hint; the searchable
        // tabs keep the fuller one, because search and scrolling exist there.
        assert!(footer.contains("Esc to cancel"), "{footer}");
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 32,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Custom);
        assert!(
            ScreenReaderSnapshot::from_state(&state)
                .as_text()
                .contains("No installed custom commands or skills were discovered")
        );
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 21,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Commands);
        for character in "no-such-command".chars() {
            key(&mut state, KeyCode::Char(character));
        }
        terminal
            .draw(|frame| crate::render::draw(frame, &mut state))
            .unwrap();
        assert!(
            ScreenReaderSnapshot::from_state(&state)
                .as_text()
                .contains("No matching commands")
        );
        key(&mut state, KeyCode::Esc);
        assert!(state.help_panel.is_none());
    }

    #[test]
    fn help_modal_uses_active_runtime_identity_and_keeps_catalog_local() {
        let mut state = AppState::new("active-model", "/workspace".into());
        state.runtime = "codex".to_owned();
        state.apply(&request());
        let snapshot = ScreenReaderSnapshot::from_state(&state);
        assert!(snapshot.as_text().contains("runtime: codex"));
        assert!(snapshot.as_text().contains("model: active-model"));
        assert!(!snapshot.as_text().contains("provider: fake"));
        assert!(state.pending_send.is_none());
    }

    #[test]
    fn custom_tab_cycles_searches_real_invocations_and_reports_partial_inventory() {
        let mut state = AppState::new("test", "/workspace".into());
        let UiEvent::HelpRequested { header, commands } = request() else {
            unreachable!();
        };
        let duplicate =
            CustomHelpRow::new("/skill review", "Review the current patch", "project skill")
                .unwrap();
        state.help_panel = Some(
            HelpView::new(header, commands, &Keymap::default())
                .with_custom_rows(vec![
                    CustomHelpRow::new("/deploy [target]", "", "installed plugin: deploy").unwrap(),
                    duplicate.clone(),
                    duplicate,
                ])
                .with_custom_notice(Some("skill registry refresh failed".to_owned())),
        );

        key(&mut state, KeyCode::Tab);
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Commands);
        key(&mut state, KeyCode::Tab);
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Custom);
        for character in "review".chars() {
            key(&mut state, KeyCode::Char(character));
        }
        let snapshot = ScreenReaderSnapshot::from_state(&state);
        let flat = snapshot.as_text();
        assert!(flat.contains("Help — Custom commands"), "{flat}");
        assert!(
            flat.contains("1 of 2 installed custom commands and skills"),
            "{flat}"
        );
        assert!(flat.contains("/skill review"), "{flat}");
        assert!(!flat.contains("/deploy [target]"), "{flat}");
        assert!(flat.contains("skill registry refresh failed"), "{flat}");

        key(&mut state, KeyCode::Tab);
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::General);
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::SHIFT,
        )));
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Custom);
        key(&mut state, KeyCode::BackTab);
        assert_eq!(state.help_panel.as_ref().unwrap().tab, HelpTab::Commands);
    }

    #[test]
    fn custom_rows_are_bounded_terminal_safe_and_require_invocation_and_source() {
        assert!(CustomHelpRow::new("", "description", "source").is_none());
        assert!(CustomHelpRow::new("/skill safe", "description", "\n\t").is_none());
        let row = CustomHelpRow::new(
            format!("/skill {}\u{1b}[31m", "x".repeat(200)),
            format!("line one\nline two{}", "y".repeat(300)),
            format!("project\t{}", "z".repeat(200)),
        )
        .unwrap();
        assert_eq!(row.invocation.chars().count(), 160);
        assert_eq!(row.description.chars().count(), 240);
        assert_eq!(row.source.chars().count(), 120);
        assert!(!row.help_line().chars().any(char::is_control));
    }
}
