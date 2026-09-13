//! Effective sandbox capabilities and fenced session-reopen selection intents.
use crate::{panel_frame as panel, terminal::Styles};
use crossterm::event::{KeyCode, KeyModifiers};
use heycode_exec::{
    FileReadScope, FileWriteScope, NetworkScope, SandboxCapabilityReport, SandboxMode,
};
use ratatui::{style::Style, text::Line};

pub(crate) struct SandboxPanel {
    report: SandboxCapabilityReport,
    tab: usize,
    selected: usize,
    notice: Option<String>,
    pending_selection: Option<SandboxMode>,
}

impl SandboxPanel {
    pub(crate) fn new(mut report: SandboxCapabilityReport) -> Self {
        // Render one row per native mode in source-shaped restrictive-first order.
        report.choices.sort_by_key(|row| match row.mode {
            SandboxMode::WorkspaceWrite => 0,
            SandboxMode::ReadOnly => 1,
            SandboxMode::Off => 2,
        });
        report.choices.dedup_by_key(|row| row.mode);
        let selected = report
            .choices
            .iter()
            .position(|row| row.mode == report.effective_mode)
            .unwrap_or(0);
        Self {
            report,
            tab: 0,
            selected,
            notice: None,
            pending_selection: None,
        }
    }

    /// Only unmodified navigation and dismissal keys are owned by this panel.
    pub(crate) fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !modifiers.is_empty() {
            return false;
        }
        match code {
            KeyCode::Esc => return true,
            KeyCode::Left => {
                self.tab = (self.tab + 2) % 3;
                self.notice = None;
            }
            KeyCode::Right | KeyCode::Tab => {
                self.tab = (self.tab + 1) % 3;
                self.notice = None;
            }
            KeyCode::Up if self.tab == 0 => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if self.tab == 0 => {
                self.selected = (self.selected + 1).min(self.report.choices.len().saturating_sub(1))
            }
            KeyCode::Enter if self.tab == 0 => {
                if let Some(row) = self.report.choices.get(self.selected) {
                    self.notice = Some(if row.mode == self.report.effective_mode {
                        "This mode is already active.".to_owned()
                    } else if !row.selectable {
                        "This host cannot enforce the selected mode.".to_owned()
                    } else {
                        self.pending_selection = Some(row.mode);
                        "Reopening this session to apply the selected mode.".to_owned()
                    });
                }
            }
            _ => {}
        }
        false
    }

    pub(crate) fn take_selection(&mut self) -> Option<SandboxMode> {
        self.pending_selection.take()
    }

    pub(crate) fn lines(&self, width: u16, styles: Styles) -> Vec<Line<'static>> {
        let mut lines = vec![
            panel::title_with_tabs(
                "Sandbox",
                &["Mode", "Overrides", "Config"],
                self.tab,
                styles,
            ),
            panel::blank(),
        ];
        match self.tab {
            0 => {
                lines.push(panel::description("Configure mode", styles));
                lines.push(panel::blank());
                for (index, row) in self.report.choices.iter().enumerate() {
                    let label = format!(
                        "{}{}",
                        mode_label(row.mode),
                        if row.selectable { "" } else { " (unavailable)" }
                    );
                    lines.push(panel::option(
                        if index == self.selected {
                            panel::Marker::Cursor
                        } else {
                            panel::Marker::None
                        },
                        index + 1,
                        &label,
                        row.mode == self.report.effective_mode,
                        styles,
                    ));
                }
                lines.push(panel::blank());
                lines.extend(panel::wrap_note("Mode changes reopen this session. Command approval is configured separately in /permissions.", width, styles));
            }
            1 => {
                lines.push(panel::description("Effective execution boundaries", styles));
                lines.push(panel::blank());
                if let Some(row) = self.report.choice(self.report.effective_mode) {
                    for text in [
                        format!("Read: {}", read_label(row.file_read)),
                        format!("Write: {}", write_label(row.file_write)),
                        format!("Network: {}", network_label(row.network)),
                    ] {
                        lines.extend(panel::wrap(
                            &text,
                            width,
                            Style::default().fg(styles.text()),
                        ));
                    }
                }
                lines.push(panel::blank());
                lines.extend(panel::wrap_note(
                    "Per-command sandbox overrides are unavailable in this session.",
                    width,
                    styles,
                ));
            }
            _ => {
                lines.push(panel::description("Effective configuration", styles));
                lines.push(panel::blank());
                for text in [
                    format!(
                        "sandbox.mode = {}",
                        crate::permission_picker::sandbox_config_value(self.report.effective_mode)
                    ),
                    format!(
                        "Active backend: {}",
                        self.report.active_backend.unwrap_or("none")
                    ),
                    format!(
                        "Available backend: {}",
                        self.report.available_backend.unwrap_or("none")
                    ),
                ] {
                    lines.extend(panel::wrap(
                        &text,
                        width,
                        Style::default().fg(styles.text()),
                    ));
                }
                lines.push(panel::blank());
                lines.extend(panel::wrap_note(
                    "Select a mode in the Mode tab to reopen this session with that sandbox policy.",
                    width,
                    styles,
                ));
            }
        }
        if let Some(notice) = &self.notice {
            lines.push(panel::blank());
            lines.extend(panel::wrap_note(notice, width, styles));
        }
        lines.push(panel::blank());
        lines.extend(panel::wrap_note(
            if self.tab == 0 {
                "←/→ to switch · ↑/↓ to navigate · Enter to select · Esc to close"
            } else {
                "←/→ to switch · Esc to close"
            },
            width,
            styles,
        ));
        lines
    }
}

fn mode_label(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::Off => "No Sandbox",
        SandboxMode::ReadOnly => "Read-only sandbox",
        SandboxMode::WorkspaceWrite => "Workspace-write sandbox",
    }
}
fn read_label(scope: FileReadScope) -> &'static str {
    match scope {
        FileReadScope::Host => "Host files (ordinary OS permissions apply)",
        FileReadScope::Unspecified => "Unspecified",
    }
}
fn write_label(scope: FileWriteScope) -> &'static str {
    match scope {
        FileWriteScope::Host => "Host files (ordinary OS permissions apply)",
        FileWriteScope::DeviceOnly => "Device sinks only",
        FileWriteScope::WorkspaceAndTemp => "Workspace and backend temporary roots",
        FileWriteScope::Unspecified => "Unspecified",
    }
}
fn network_label(scope: NetworkScope) -> &'static str {
    match scope {
        NetworkScope::Host => "Host network",
        NetworkScope::Isolated => "Isolated or denied",
        NetworkScope::Unspecified => "Unspecified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modal_input_does_not_reach_the_composer() {
        use crossterm::event::{Event, KeyEvent, MouseEvent, MouseEventKind};
        let mut state = crate::app::AppState::new("test", std::env::temp_dir());
        state.input.insert_str("retained draft");
        state.apply(&heycode_agent::UiEvent::SandboxPanelRequested {
            report: SandboxCapabilityReport {
                effective_mode: SandboxMode::Off,
                active_backend: None,
                available_backend: None,
                choices: vec![],
            },
        });
        for event in [
            Event::Paste("/quit\nunsafe draft".to_owned()),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
        ] {
            assert!(!state.handle_terminal_event(&event));
        }
        assert_eq!(state.input.lines(), &["retained draft"]);
        assert!(state.sandbox_panel().is_some());
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(state.sandbox_panel().is_none());
        assert_eq!(state.input.lines(), &["retained draft"]);
    }

    #[test]
    fn selection_and_tabs_never_mutate_effective_policy() {
        let report = SandboxCapabilityReport {
            effective_mode: SandboxMode::Off,
            active_backend: None,
            available_backend: Some("test"),
            choices: vec![
                heycode_exec::SandboxChoiceCapability {
                    mode: SandboxMode::Off,
                    selectable: true,
                    file_read: FileReadScope::Host,
                    file_write: FileWriteScope::Host,
                    network: NetworkScope::Host,
                },
                heycode_exec::SandboxChoiceCapability {
                    mode: SandboxMode::ReadOnly,
                    selectable: true,
                    file_read: FileReadScope::Host,
                    file_write: FileWriteScope::DeviceOnly,
                    network: NetworkScope::Host,
                },
            ],
        };
        let mut view = SandboxPanel::new(report);
        view.handle_key(KeyCode::Up, KeyModifiers::NONE);
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(view.take_selection(), Some(SandboxMode::ReadOnly));
        assert_eq!(view.take_selection(), None);
        assert_eq!(view.report.effective_mode, SandboxMode::Off);
        view.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(view.tab, 1);
        view.handle_key(KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(view.tab, 1);
        assert!(view.handle_key(KeyCode::Esc, KeyModifiers::NONE));
    }
}
