//! Installed-plugin list and detail presentation over the shared lifecycle.
use super::{PluginPanelFocus, PluginPanelTone, PluginPanelView, section_view};
use crate::terminal::Styles;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Draw the current local Installed view without exposing package diagnostics by default.
pub fn draw(frame: &mut Frame<'_>, panel: &PluginPanelView, area: Rect, styles: Styles) {
    frame.render_widget(Clear, area);
    if area.height < 12 && panel.focus == PluginPanelFocus::Actions && !panel.is_prompting() {
        let id = panel
            .selected_row()
            .map_or("No selected plugin", |row| row.id.as_str());
        let actions = panel.actions();
        let action = actions
            .get(panel.action_selected)
            .map_or("No action", |action| action.label);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(format!("Plugins · {id}")),
                Line::from(format!("❯ {action}")),
                Line::from("↑↓ action · Enter select · Esc back"),
            ]),
            area,
        );
        return;
    }

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                "   Plugins    ",
                Style::default().fg(styles.accent()).bold(),
            ),
            Span::styled(" Installed ", Style::default().fg(styles.text()).reversed()),
            Span::styled("   Local packages", Style::default().fg(styles.dim())),
        ]),
        Line::default(),
    ];
    if panel.is_prompting() {
        for line in panel.visual_tail_lines() {
            lines.push(toned(line.text, line.tone, styles));
        }
    } else if panel.focus == PluginPanelFocus::Plugins {
        lines.extend([
            Line::default(),
            Line::default(),
            Line::default(),
            Line::from(Span::styled(
                "   Installed packages",
                Style::default().fg(styles.dim()),
            )),
        ]);
        let matches = panel.matching_indices();
        let position = matches
            .iter()
            .position(|index| *index == panel.selected)
            .unwrap_or(0);
        let start = position.saturating_sub(3);
        let available = usize::from(area.height.saturating_sub(8)).max(1);
        for index in matches.iter().skip(start).take(available) {
            let row = &panel.rows[*index];
            let selected = *index == panel.selected;
            let source = row
                .facts
                .as_ref()
                .and_then(|facts| facts.provenance.as_ref())
                .map_or("source unavailable", |source| source.kind);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "   {} {} ",
                        if selected { "❯" } else { " " },
                        row.id.as_str()
                    ),
                    Style::default().fg(if selected {
                        styles.accent()
                    } else {
                        styles.text()
                    }),
                ),
                Span::styled("Plugin", Style::default().fg(styles.text()).reversed()),
                Span::styled(
                    format!(
                        " · {source} · {} {}",
                        if row.enabled { "✔" } else { "○" },
                        if row.enabled { "enabled" } else { "disabled" }
                    ),
                    Style::default().fg(styles.dim()),
                ),
            ]));
        }
        if matches.is_empty() {
            lines.push(Line::from("   No matching plugins"));
        }
    } else if let Some(row) = panel.selected_row() {
        let source = row
            .facts
            .as_ref()
            .and_then(|facts| facts.provenance.as_ref())
            .map_or("source unavailable", |source| source.kind);
        lines.push(Line::from(Span::styled(
            format!("   {} @ {source}", row.id.as_str()),
            Style::default().fg(styles.text()).bold(),
        )));
        lines.push(Line::from(format!("   Version: {}", row.active.as_str())));
        lines.push(Line::from(Span::styled(
            format!(
                "   Status: {}",
                if row.enabled { "Enabled" } else { "Disabled" }
            ),
            Style::default().fg(if row.enabled {
                styles.success()
            } else {
                styles.dim()
            }),
        )));
        lines.push(Line::default());
        if panel.show_details {
            lines.push(Line::from(Span::styled(
                format!("   {}", panel.section.title()),
                Style::default().fg(styles.accent()),
            )));
            lines.extend(
                section_view(panel.section, Some(row))
                    .lines
                    .into_iter()
                    .skip(panel.section_offset)
                    .take(4)
                    .map(|line| Line::from(format!("   {line}"))),
            );
            lines.push(Line::default());
        }
        for (index, action) in panel.actions().into_iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!(
                    "   {} {}{}",
                    if index == panel.action_selected {
                        "❯"
                    } else {
                        " "
                    },
                    action.label,
                    if action.is_available() {
                        ""
                    } else {
                        " (unavailable)"
                    }
                ),
                Style::default()
                    .fg(if action.is_available() {
                        styles.text()
                    } else {
                        styles.dim()
                    })
                    .add_modifier(if index == panel.action_selected {
                        ratatui::style::Modifier::BOLD
                    } else {
                        ratatui::style::Modifier::empty()
                    }),
            )));
        }
    }
    if let Some(notice) = &panel.notice {
        lines.push(toned(
            format!("   {}", notice.text),
            if notice.ok {
                PluginPanelTone::Good
            } else {
                PluginPanelTone::Bad
            },
            styles,
        ));
    }
    let footer = if panel.is_prompting() {
        panel.hint()
    } else if panel.focus == PluginPanelFocus::Plugins {
        "Type to search · Space to toggle · Enter to view · Ctrl+i to install · Esc to go back"
    } else {
        "↑/↓ to navigate · Enter to select · i for details · Tab detail section · Esc to go back"
    };
    let body_height = area.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.border())),
        ),
        Rect::new(area.x, area.y, area.width, body_height),
    );
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("   {footer}"),
                Style::default().fg(styles.dim()),
            ))),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
    if panel.focus == PluginPanelFocus::Plugins
        && !panel.is_prompting()
        && area.height >= 7
        && area.width >= 8
    {
        let search = Rect::new(area.x + 3, area.y + 3, area.width - 6, 3);
        let text = if panel.query.is_empty() {
            "⌕ Search…"
        } else {
            &panel.query
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(styles.dim()))
                .block(Block::default().borders(Borders::ALL)),
            search,
        );
    }
}

fn toned(text: String, tone: PluginPanelTone, styles: Styles) -> Line<'static> {
    let color = match tone {
        PluginPanelTone::Heading | PluginPanelTone::Selected => styles.accent(),
        PluginPanelTone::Body => styles.text(),
        PluginPanelTone::Dim => styles.dim(),
        PluginPanelTone::Good => styles.success(),
        PluginPanelTone::Warn => styles.warn(),
        PluginPanelTone::Bad => styles.error(),
    };
    Line::from(Span::styled(text, Style::default().fg(color)))
}
