//! Source-shaped terminal renderer for the provider-neutral Stats view model.

use heycode_agent::ui::{SettingsShellTab, SettingsShellTab as ShellTab};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize as _};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::settings_panel::SettingsShellView;
use crate::stats_view::{
    StatsDateRange, StatsHeatmapCell, StatsNumber, StatsOverviewSummary, StatsView, StatsViewTab,
    compact_duration, compact_number,
};
use crate::terminal::Styles;

/// Draw the complete Settings shell while its active child is typed Stats.
///
/// The shared renderer should call this before its generic plain-text section
/// path when `shell.tab() == SettingsShellTab::Stats` and
/// `shell.stats_view().is_some()`.
pub(crate) fn draw(frame: &mut Frame<'_>, shell: &SettingsShellView, area: Rect, styles: Styles) {
    let Some(stats) = shell.stats_view() else {
        return;
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(Span::styled(
                " Settings ",
                Style::default().fg(styles.accent()).bold(),
            ))
            .borders(Borders::TOP)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(styles.accent())),
        area,
    );
    if area.width == 0 || area.height < 3 {
        return;
    }

    let content_x = area.x;
    let content_width = area.width;
    let outer_y = area.y.saturating_add(1);
    let inner_y = area.y.saturating_add(3);
    let body_y = area.y.saturating_add(5);
    let footer_y = area.bottom().saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);

    let (outer, outer_hits) = shell_tabs(shell.tab(), content_x, outer_y, styles);
    frame.render_widget(
        Paragraph::new(outer),
        Rect::new(content_x, outer_y, content_width, 1),
    );
    shell.set_shell_mouse_regions(outer_hits, None);

    let (inner, inner_hits) = stats_tabs(stats.tab(), content_x, inner_y, styles);
    frame.render_widget(
        Paragraph::new(inner),
        Rect::new(content_x, inner_y, content_width, 1),
    );
    shell.set_stats_mouse_regions(inner_hits);

    let body = match stats.tab() {
        StatsViewTab::Overview => overview_lines(stats, usize::from(content_width), styles),
        StatsViewTab::Models => model_lines(stats, usize::from(content_width), styles),
    };
    shell.set_content_layout_rows(usize::from(body_height), body.len());
    frame.render_widget(
        Paragraph::new(
            body.into_iter()
                .skip(shell.content_offset())
                .take(usize::from(body_height))
                .collect::<Vec<_>>(),
        ),
        Rect::new(content_x, body_y, content_width, body_height),
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shell.footer(),
            Style::default().fg(styles.border()),
        ))),
        Rect::new(content_x, footer_y, content_width, 1),
    );
}

fn shell_tabs(
    selected: SettingsShellTab,
    x: u16,
    y: u16,
    styles: Styles,
) -> (Line<'static>, Vec<(Rect, ShellTab)>) {
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut offset = x;
    for tab in SettingsShellTab::ALL {
        let label = format!(" {} ", tab.label());
        let width = u16::try_from(label.len()).unwrap_or(u16::MAX);
        hits.push((Rect::new(offset, y, width, 1), tab));
        offset = offset.saturating_add(width);
        spans.push(Span::styled(label, selected_style(tab == selected, styles)));
    }
    (Line::from(spans), hits)
}

fn stats_tabs(
    selected: StatsViewTab,
    x: u16,
    y: u16,
    styles: Styles,
) -> (Line<'static>, Vec<(Rect, StatsViewTab)>) {
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut offset = x;
    for tab in [StatsViewTab::Overview, StatsViewTab::Models] {
        let label = format!(" {} ", tab.label());
        let width = u16::try_from(label.len()).unwrap_or(u16::MAX);
        hits.push((Rect::new(offset, y, width, 1), tab));
        offset = offset.saturating_add(width);
        spans.push(Span::styled(label, selected_style(tab == selected, styles)));
    }
    (Line::from(spans), hits)
}

fn selected_style(selected: bool, styles: Styles) -> Style {
    if selected {
        Style::default().fg(styles.accent()).bold().underlined()
    } else {
        Style::default().fg(styles.dim())
    }
}

fn overview_lines(stats: &StatsView, width: usize, styles: Styles) -> Vec<Line<'static>> {
    let heatmap = stats.heatmap();
    let chart_width = width.saturating_sub(6).max(1);
    let visible = chart_width.min(heatmap.weeks().len());
    let start = heatmap.weeks().len().saturating_sub(visible);
    let mut month_header = vec![' '; visible];
    for marker in heatmap.months() {
        if marker.week() < start {
            continue;
        }
        let position = marker.week() - start;
        for (offset, character) in marker.label().chars().enumerate() {
            if let Some(slot) = month_header.get_mut(position.saturating_add(offset)) {
                *slot = character;
            }
        }
    }
    let mut lines = vec![Line::from(Span::styled(
        format!("     {}", month_header.into_iter().collect::<String>()),
        Style::default().fg(styles.dim()),
    ))];
    for weekday in 0..7 {
        let label = match weekday {
            0 => "Mon  ",
            2 => "Wed  ",
            4 => "Fri  ",
            _ => "     ",
        };
        let mut spans = vec![Span::styled(label, Style::default().fg(styles.dim()))];
        for week in heatmap.weeks().iter().skip(start) {
            spans.push(heatmap_span(&week.cells()[weekday], styles));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::from(vec![
        Span::styled("     Less ", Style::default().fg(styles.dim())),
        heatmap_legend(1, styles),
        heatmap_legend(2, styles),
        heatmap_legend(3, styles),
        heatmap_legend(4, styles),
        Span::styled(" More", Style::default().fg(styles.dim())),
    ]));
    lines.push(Line::from(""));
    lines.push(range_line(stats.range(), styles));
    lines.push(Line::from(""));
    lines.extend(summary_lines(&stats.summary(), width, styles));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        stats.scope_note(),
        Style::default().fg(styles.dim()),
    )));
    lines
}

fn heatmap_span(cell: &StatsHeatmapCell, styles: Styles) -> Span<'static> {
    let (glyph, color) = match cell.intensity() {
        0 => ('·', styles.dim()),
        1 => ('░', styles.border()),
        2 => ('▒', styles.code()),
        3 => ('▓', styles.accent()),
        _ => ('█', styles.text()),
    };
    Span::styled(glyph.to_string(), Style::default().fg(color))
}

fn heatmap_legend(intensity: u8, styles: Styles) -> Span<'static> {
    let cell = StatsHeatmapCellForLegend(intensity);
    let (glyph, color) = match cell.0 {
        1 => ('░', styles.border()),
        2 => ('▒', styles.code()),
        3 => ('▓', styles.accent()),
        _ => ('█', styles.text()),
    };
    Span::styled(glyph.to_string(), Style::default().fg(color))
}

struct StatsHeatmapCellForLegend(u8);

fn range_line(selected: StatsDateRange, styles: Styles) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, range) in [
        StatsDateRange::AllTime,
        StatsDateRange::LastSevenDays,
        StatsDateRange::LastThirtyDays,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(styles.dim())));
        }
        spans.push(Span::styled(
            range.label(),
            selected_style(range == selected, styles),
        ));
    }
    Line::from(spans)
}

fn summary_lines(
    summary: &StatsOverviewSummary,
    width: usize,
    styles: Styles,
) -> Vec<Line<'static>> {
    let favorite = format!(
        "Favorite model: {}",
        summary.favorite_model().unwrap_or("unavailable")
    );
    let tokens = format!("Total tokens: {}", format_number(summary.total_tokens()));
    let sessions = summary.sessions().map_or_else(
        || "Sessions: — (all-time only)".to_owned(),
        |value| format!("Sessions: {value}"),
    );
    let longest = summary.longest_session_ms().map_or_else(
        || "Longest session: unavailable".to_owned(),
        |value| format!("Longest session: {}", compact_duration(value)),
    );
    let active_days = format!(
        "Active days: {}/{}",
        summary.active_days(),
        summary.calendar_days()
    );
    let longest_streak = format!("Longest streak: {} days", summary.longest_streak_days());
    let active_day = summary.most_active_day().map_or_else(
        || "Most active day: unavailable".to_owned(),
        |(date, _)| format!("Most active day: {}", date.format("%b %-d")),
    );
    let current_streak = format!("Current streak: {} days", summary.current_streak_days());
    let mut lines = vec![
        two_columns(favorite, tokens, width, styles),
        two_columns(sessions, longest, width, styles),
        two_columns(active_days, longest_streak, width, styles),
        two_columns(active_day, current_streak, width, styles),
        Line::from(Span::styled(
            format!(
                "Input {} · Output {} · Cache read {} · Cache write {}",
                format_number(summary.input_tokens()),
                format_number(summary.output_tokens()),
                format_number(summary.cache_read_tokens()),
                format_number(summary.cache_write_tokens())
            ),
            Style::default().fg(styles.text()),
        )),
    ];
    if summary.unreadable_sessions() > 0
        || summary.unreported_responses() > 0
        || summary.unattributed_responses() > 0
    {
        lines.push(Line::from(Span::styled(
            format!(
                "Evidence gaps · unreadable {} · unreported {} · unattributed {}",
                summary.unreadable_sessions(),
                summary.unreported_responses(),
                summary.unattributed_responses()
            ),
            Style::default().fg(styles.warn()),
        )));
    }
    lines
}

fn two_columns(left: String, right: String, width: usize, styles: Styles) -> Line<'static> {
    if width < 64 {
        return Line::from(vec![
            Span::styled(left, Style::default().fg(styles.text())),
            Span::styled(" · ", Style::default().fg(styles.dim())),
            Span::styled(right, Style::default().fg(styles.text())),
        ]);
    }
    let left_width = width / 2;
    Line::from(vec![
        Span::styled(
            format!("{left:<left_width$}"),
            Style::default().fg(styles.text()),
        ),
        Span::styled(right, Style::default().fg(styles.text())),
    ])
}

fn format_number(number: StatsNumber) -> String {
    let prefix = if number.is_lower_bound() { "≥" } else { "" };
    format!("{prefix}{}", compact_number(number.value()))
}

fn model_lines(stats: &StatsView, width: usize, styles: Styles) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "All local history · provider-reported tokens",
        Style::default().fg(styles.dim()),
    ))];
    lines.push(Line::from(""));
    if stats.models().is_empty() {
        lines.push(Line::from(Span::styled(
            "No attributable model responses",
            Style::default().fg(styles.dim()),
        )));
    } else if width >= 82 {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<36}", "Provider / model"),
                Style::default().fg(styles.dim()).bold(),
            ),
            Span::styled(
                format!(
                    "{:>9} {:>9} {:>10} {:>10}",
                    "Responses", "Reported", "Input", "Output"
                ),
                Style::default().fg(styles.dim()).bold(),
            ),
        ]));
        for model in stats.models() {
            let identity = format!("{}/{}", model.provider(), model.model());
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<36}", truncate(&identity, 35)),
                    Style::default().fg(styles.text()),
                ),
                Span::styled(
                    format!(
                        "{:>9} {:>9} {:>10} {:>10}",
                        model.responses(),
                        model.reported_responses(),
                        compact_number(model.input_tokens()),
                        compact_number(model.output_tokens())
                    ),
                    Style::default().fg(if model.usage_complete() {
                        styles.text()
                    } else {
                        styles.warn()
                    }),
                ),
            ]));
        }
    } else {
        for model in stats.models() {
            lines.push(Line::from(Span::styled(
                format!("{}/{}", model.provider(), model.model()),
                Style::default().fg(styles.text()).bold(),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  responses {} · reported {} · input {} · output {} · total {}",
                    model.responses(),
                    model.reported_responses(),
                    compact_number(model.input_tokens()),
                    compact_number(model.output_tokens()),
                    compact_number(model.total_tokens())
                ),
                Style::default().fg(if model.usage_complete() {
                    styles.dim()
                } else {
                    styles.warn()
                }),
            )));
        }
    }
    if stats.omitted_models() > 0 {
        lines.push(Line::from(Span::styled(
            format!("{} additional route(s) omitted", stats.omitted_models()),
            Style::default().fg(styles.warn()),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        stats.scope_note(),
        Style::default().fg(styles.dim()),
    )));
    lines
}

fn truncate(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_owned();
    }
    let mut result = value
        .chars()
        .take(maximum.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_number_keeps_lower_bound_visible() {
        assert_eq!(format_number(StatsNumber::new(17_200, false)), "17.2k");
        assert_eq!(format_number(StatsNumber::new(17_200, true)), "≥17.2k");
    }

    #[test]
    fn narrow_identity_truncation_retains_a_visible_ellipsis() {
        assert_eq!(truncate("provider/a-very-long-model", 12), "provider/a-…");
    }
}
