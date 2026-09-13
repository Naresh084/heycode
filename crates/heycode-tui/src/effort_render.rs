//! Horizontal effort choices from the exact active backend catalog.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

pub(crate) fn draw(
    frame: &mut Frame<'_>,
    picker: &crate::app::EffortPickerView,
    model: &str,
    styles: crate::terminal::Styles,
    area: Rect,
) {
    frame.render_widget(Clear, area);
    if area.height < 9 {
        let selected = picker
            .choices()
            .get(picker.selected())
            .map_or("unavailable", String::as_str);
        let lines = vec![
            Line::from(format!(" Effort: {selected}")),
            Line::from(format!(" {} · {model}", picker.owner().id())),
            Line::from(" ←/→ adjust · Enter default · s session · Esc cancel"),
        ];
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }

    let width = usize::from(area.width.saturating_sub(6));
    let cell_width = picker
        .choices()
        .iter()
        .map(|value| value.width().saturating_add(8))
        .max()
        .unwrap_or(10)
        .max(10)
        .min(width.max(1));
    let visible = (width / cell_width).max(1).min(picker.choices().len());
    let start = picker
        .selected()
        .saturating_sub(visible / 2)
        .min(picker.choices().len().saturating_sub(visible));
    let indent = usize::from(area.width).saturating_sub(visible * cell_width) / 2;
    let mut scale = vec![Span::raw(" ".repeat(indent))];
    let mut labels = vec![Span::raw(" ".repeat(indent))];
    let scale_width = visible * cell_width;
    let direction = format!(
        "{}Faster{}Smarter",
        " ".repeat(indent),
        " ".repeat(scale_width.saturating_sub(13))
    );
    for (offset, choice) in picker
        .choices()
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        let selected = offset == picker.selected();
        let mark = if selected { '▲' } else { '─' };
        let rail = format!(
            "{}{}{}",
            "─".repeat(cell_width / 2),
            mark,
            "─".repeat(cell_width.saturating_sub(cell_width / 2 + 1))
        );
        let style = if selected {
            Style::default().fg(styles.accent()).bold()
        } else {
            Style::default().fg(styles.dim())
        };
        scale.push(Span::styled(rail, Style::default().fg(styles.dim())));
        let label =
            crate::terminal::truncate_to_width(&crate::task_console::safe(choice), cell_width);
        let pad = cell_width.saturating_sub(label.width());
        labels.push(Span::styled(format!("{label}{}", " ".repeat(pad)), style));
    }
    let selected = picker.choices().get(picker.selected()).map(String::as_str);
    let suffix = match (
        selected == picker.current_effort(),
        selected == picker.default_effort(),
    ) {
        (true, true) => "current · default",
        (true, false) => "current",
        (false, true) => "default",
        _ => "",
    };
    let lines = vec![
        Line::from(Span::styled(
            "   Effort",
            Style::default().fg(styles.accent()).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "   {} · {model}",
                crate::task_console::safe(picker.owner().id())
            ),
            Style::default().fg(styles.dim()),
        )),
        Line::from(Span::styled(direction, Style::default().fg(styles.text()))),
        Line::from(scale),
        Line::from(labels),
        Line::from(Span::styled(
            format!(
                "   {}  {suffix}",
                selected.unwrap_or("No effort values available")
            ),
            Style::default().fg(styles.dim()),
        )),
        Line::default(),
        Line::from(Span::styled(
            "   ←/→ to adjust · Enter to set default · s for this session · Esc to cancel",
            Style::default().fg(styles.dim()),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.accent())),
        ),
        area,
    );
}
