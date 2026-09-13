//! Source-shaped Skill Stats panel backed only by current native facts.
//!
//! Seven-day usage and machine-wide last-used history are deliberately shown
//! as unavailable: heycode does not persist them, and current-session deliveries
//! are not a safe substitute.

use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// One event result owned by the Skill Stats surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillDoctorPanelAction {
    None,
    Close,
}

/// Read-only, scrollable Stats view created from one exact catalog/log pair.
pub(crate) struct SkillDoctorPanel {
    snapshot: heycode_skills::SkillDoctorSnapshot,
    offset: usize,
    area: Rect,
}

impl SkillDoctorPanel {
    pub(crate) fn capture(
        skills: &heycode_skills::SkillSet,
        session: &heycode_session::Session,
    ) -> Result<Self, heycode_skills::SkillRegistryError> {
        Ok(Self::from_snapshot(
            heycode_skills::SkillDoctorSnapshot::capture(skills, session.events())?,
        ))
    }

    fn from_snapshot(snapshot: heycode_skills::SkillDoctorSnapshot) -> Self {
        Self {
            snapshot,
            offset: 0,
            area: Rect::default(),
        }
    }

    pub(crate) fn handle(&mut self, event: &Event) -> SkillDoctorPanelAction {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return SkillDoctorPanelAction::Close,
                KeyCode::Up => self.scroll(-1),
                KeyCode::Down => self.scroll(1),
                KeyCode::PageUp => self.scroll(-10),
                KeyCode::PageDown => self.scroll(10),
                KeyCode::Home => self.offset = 0,
                KeyCode::End => self.offset = self.snapshot.rows().len().saturating_sub(1),
                _ => {}
            },
            Event::Mouse(mouse) if self.area.contains((mouse.column, mouse.row).into()) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll(-3),
                    MouseEventKind::ScrollDown => self.scroll(3),
                    _ => {}
                }
            }
            _ => {}
        }
        SkillDoctorPanelAction::None
    }

    pub(crate) fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Skills — Stats".to_owned(),
            "/skill-doctor — current-session skill usage and context costs".to_owned(),
            "skill — source — context — 7d tokens — uses — last used".to_owned(),
        ];
        lines.extend(self.snapshot.rows().iter().map(accessible_row));
        lines.push(
            "context = approximate one-line catalog listing included in each native request"
                .to_owned(),
        );
        lines.push(
            "7d tokens and historical last-used = unavailable; uses = successful instruction-body deliveries in this session"
                .to_owned(),
        );
        lines.push(format!(
            "{} skills admitted — ~{} catalog tokens per request — {} discovery issue{}",
            self.snapshot.rows().len(),
            self.snapshot.catalog_tokens(),
            self.snapshot.skipped().len(),
            if self.snapshot.skipped().len() == 1 {
                ""
            } else {
                "s"
            }
        ));
        lines.push(
            "keys: Up/Down, Home/End, PageUp/PageDown, or mouse wheel scroll; Escape closes."
                .to_owned(),
        );
        lines
    }

    pub(crate) fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        styles: crate::terminal::Styles,
    ) {
        self.area = area;
        if area.width < 20 || area.height < 12 {
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
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  Skills",
                    Style::default()
                        .fg(styles.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("    Catalog", Style::default().fg(styles.text())),
                Span::styled(
                    "    Stats ",
                    Style::default()
                        .fg(styles.text())
                        .bg(styles.accent())
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
        frame.render_widget(
            Paragraph::new("  ℹ /skill-doctor — current-session skill usage and context costs")
                .style(Style::default().fg(styles.accent())),
            Rect::new(area.x, area.y.saturating_add(3), area.width, 1),
        );
        frame.render_widget(
            Paragraph::new("  Skills loaded this session")
                .style(Style::default().fg(styles.text()).bold()),
            Rect::new(area.x, area.y.saturating_add(5), area.width, 1),
        );

        let table_width = usize::from(area.width.saturating_sub(4));
        let header = table_row(
            "skill",
            "source",
            "context",
            "7d tokens",
            "uses",
            "last used",
            table_width,
        );
        frame.render_widget(
            Paragraph::new(header).style(Style::default().fg(styles.dim())),
            Rect::new(
                area.x.saturating_add(2),
                area.y.saturating_add(7),
                area.width.saturating_sub(4),
                1,
            ),
        );

        let table_height = usize::from(area.height.saturating_sub(15)).max(1);
        self.offset = self
            .offset
            .min(self.snapshot.rows().len().saturating_sub(table_height));
        let lines = self
            .snapshot
            .rows()
            .iter()
            .skip(self.offset)
            .take(table_height)
            .map(|row| {
                Line::from(Span::styled(
                    visual_row(row, table_width),
                    Style::default().fg(if row.session_uses() == 0 {
                        styles.warn()
                    } else {
                        styles.text()
                    }),
                ))
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(if lines.is_empty() {
                vec![Line::from(Span::styled(
                    "No skills are admitted in this session",
                    Style::default().fg(styles.dim()),
                ))]
            } else {
                lines
            }),
            Rect::new(
                area.x.saturating_add(2),
                area.y.saturating_add(8),
                area.width.saturating_sub(4),
                u16::try_from(table_height).unwrap_or(u16::MAX),
            ),
        );

        let notes_y = area.bottom().saturating_sub(6);
        let retained = self
            .snapshot
            .rows()
            .iter()
            .map(heycode_skills::SkillDoctorRow::retained_in_context)
            .sum::<usize>();
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "  context = approximate one-line catalog listing in each native request",
                    Style::default().fg(styles.dim()),
                )),
                Line::from(Span::styled(
                    "  7d tokens and historical last-used = unavailable; uses = this session",
                    Style::default().fg(styles.dim()),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {} skills admitted · ~{} catalog tokens/request · {retained} delivered bod{} retained",
                        self.snapshot.rows().len(),
                        self.snapshot.catalog_tokens(),
                        if retained == 1 { "y" } else { "ies" }
                    ),
                    Style::default().fg(styles.warn()),
                )),
                Line::from(Span::styled(
                    format!(
                        "  {} discovery issue{} · loaded does not prove the model followed instructions",
                        self.snapshot.skipped().len(),
                        if self.snapshot.skipped().len() == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(styles.dim()),
                )),
                Line::from(Span::styled(
                    "  ↑↓/wheel scroll · Esc close",
                    Style::default().fg(styles.dim()),
                )),
            ]),
            Rect::new(area.x, notes_y, area.width, 5),
        );
    }

    fn scroll(&mut self, delta: isize) {
        self.offset = if delta.is_negative() {
            self.offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.offset
                .saturating_add(delta.unsigned_abs())
                .min(self.snapshot.rows().len().saturating_sub(1))
        };
    }
}

fn visual_row(row: &heycode_skills::SkillDoctorRow, width: usize) -> String {
    table_row(
        &format!("{} [{}]", row.name(), row.admission().as_str()),
        row.source(),
        &context_label(row.context_tokens()),
        "unknown",
        &format!("{}x", row.session_uses()),
        if row.session_uses() == 0 {
            "not this session"
        } else {
            "this session"
        },
        width,
    )
}

fn accessible_row(row: &heycode_skills::SkillDoctorRow) -> String {
    format!(
        "{} [{}] — {} — context {} — 7d tokens unavailable — {} use{} this session — {} retained — {} bytes / ~{} body tokens{}{}",
        row.name(),
        row.admission().as_str(),
        row.source(),
        context_label(row.context_tokens()),
        row.session_uses(),
        if row.session_uses() == 1 { "" } else { "s" },
        row.retained_in_context(),
        row.body_bytes(),
        row.body_tokens(),
        if row.missing_description() {
            " — missing description"
        } else {
            ""
        },
        if row.empty_instructions() {
            " — empty instructions"
        } else {
            ""
        },
    )
}

fn context_label(tokens: Option<usize>) -> String {
    match tokens {
        None => "—".to_owned(),
        Some(tokens) if tokens < 20 => "< 20".to_owned(),
        Some(tokens) => format!("~{tokens}"),
    }
}

fn table_row(
    skill: &str,
    source: &str,
    context: &str,
    history: &str,
    uses: &str,
    last_used: &str,
    width: usize,
) -> String {
    if width < 78 {
        return format!(
            "{} · {} · ctx {} · 7d {} · {} · {}",
            bounded(skill, 22),
            bounded(source, 12),
            context,
            history,
            uses,
            last_used
        );
    }
    format!(
        "{:<24} {:<13} {:<9} {:<11} {:<7} {}",
        bounded(skill, 23),
        bounded(source, 12),
        bounded(context, 8),
        bounded(history, 10),
        bounded(uses, 6),
        bounded(last_used, width.saturating_sub(69).max(1)),
    )
}

fn bounded(value: &str, max_chars: usize) -> String {
    let safe = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if safe.chars().count() <= max_chars {
        return safe;
    }
    let mut output = safe
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn snapshot() -> heycode_skills::SkillDoctorSnapshot {
        let skills = heycode_skills::SkillSet::new(vec![
            heycode_skills::Skill {
                name: "alpha".to_owned(),
                description: "deployment helper".to_owned(),
                disable_model_invocation: false,
                body: "alpha body".to_owned(),
            },
            heycode_skills::Skill {
                name: "beta".to_owned(),
                description: "review helper".to_owned(),
                disable_model_invocation: false,
                body: "beta body".to_owned(),
            },
        ])
        .unwrap();
        heycode_skills::SkillDoctorSnapshot::capture(&skills, &[]).unwrap()
    }

    #[test]
    fn projection_marks_historical_usage_unknown_instead_of_inferred() {
        let panel = SkillDoctorPanel::from_snapshot(snapshot());
        let text = panel.accessible_lines().join("\n");
        assert!(text.contains("7d tokens unavailable"), "{text}");
        assert!(text.contains("0 uses this session"), "{text}");
        assert!(
            text.contains("historical last-used = unavailable"),
            "{text}"
        );
        assert!(!text.contains("never invoked"), "{text}");
    }

    #[test]
    fn source_shaped_stats_render_is_a_panel_not_transcript_prose() {
        let mut panel = SkillDoctorPanel::from_snapshot(snapshot());
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), crate::terminal::Styles::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in [
            "Skills",
            "Catalog",
            "Stats",
            "Skills loaded this session",
            "7d tokens",
            "unknown",
            "alpha [on]",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected}: {rendered}"
            );
        }
    }
}
