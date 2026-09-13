//! Bottom-anchored placement for the command panels drawn inside a region that
//! is taller than the panel itself.
//!
//! Chrome — the `▔` rule, the indent, the title/note/hint roles — belongs to
//! [`crate::panel_frame`] and is not re-decided here. What this module adds is
//! placement: `/workflows` and the connect wizard are painted into the whole
//! transcript region rather than into a surface sized to their content, and the
//! reference puts their rule directly above their own rows at the bottom of the
//! viewport instead of floating it in the middle of the shell.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::panel_frame;
use crate::terminal::Styles;

/// Columns between a panel's left edge and its content.
pub(crate) const INDENT: u16 = 3;

/// The bottom `rows + 1` rows of `area`, which is where a panel of that size is
/// drawn. The extra row is the top rule.
pub(crate) fn anchored(area: Rect, rows: usize) -> Rect {
    let wanted = panel_frame::height(rows).min(area.height);
    Rect::new(
        area.x,
        area.bottom().saturating_sub(wanted),
        area.width,
        wanted,
    )
}

/// Paint `lines` as a panel anchored to the bottom of `area`.
pub(crate) fn render_anchored(
    frame: &mut Frame<'_>,
    area: Rect,
    styles: Styles,
    lines: Vec<Line<'static>>,
) {
    let panel = anchored(area, lines.len());
    panel_frame::render(frame, panel, styles, lines);
}

/// Paint the common "title, spacer, rows, spacer, hint" panel at the bottom of
/// `area`.
#[cfg(test)]
pub(crate) fn render_simple(
    frame: &mut Frame<'_>,
    area: Rect,
    styles: Styles,
    title: &str,
    rows: Vec<Line<'static>>,
    hint: &str,
) {
    let mut lines = vec![panel_frame::title(title, styles), panel_frame::blank()];
    lines.extend(rows);
    lines.push(panel_frame::blank());
    lines.push(panel_frame::hint(hint, styles));
    render_anchored(frame, area, styles, lines);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn styles() -> Styles {
        let theme = heycode_ui::theme::default_theme().unwrap();
        Styles::new(&theme.resolve(heycode_ui::terminal::ColorLevel::TrueColor))
    }

    fn render(rows: Vec<Line<'static>>, hint: &str) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_simple(frame, area, styles(), "Workflows", rows, hint);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn panel_sits_at_the_bottom_under_a_full_width_rule() {
        let styles = styles();
        let lines = render(
            vec![panel_frame::note("No workflows in this session.", styles)],
            "Esc to close",
        );
        assert_eq!(lines[6], "▔".repeat(40));
        assert_eq!(lines[7], "   Workflows");
        assert_eq!(lines[8], "");
        assert_eq!(lines[9], "   No workflows in this session.");
        assert_eq!(lines[10], "");
        assert_eq!(lines[11], "   Esc to close");
        assert!(lines[5].is_empty(), "rows above the rule stay untouched");
    }

    #[test]
    fn anchored_leaves_the_rows_above_the_panel_alone() {
        let area = Rect::new(0, 0, 30, 20);
        assert_eq!(anchored(area, 4), Rect::new(0, 15, 30, 5));
        assert_eq!(anchored(area, 100), Rect::new(0, 0, 30, 20));
    }
}
