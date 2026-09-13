//! One frame for every bottom-of-viewport command panel.
//!
//! `/permissions`, `/theme`, `/hooks`, `/keybindings`, `/scroll-speed` and the
//! settings browser used to each invent their own chrome: some drew a rounded
//! `Borders::TOP` block with an inline title, some a bare paragraph, and the
//! hint lines disagreed on wording and on whether they were indented at all.
//! The layout captured from the pinned reference terminal is one shape, so it
//! lives here once:
//!
//! ```text
//! ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔
//!    Sandbox  Mode   Overrides   Config
//!
//!    Configure mode
//!
//!      1. Sandbox BashTool, with auto-allow
//!    ❯ 3. No Sandbox ✔
//!
//!    ←/→ to switch · ↑/↓ to navigate · Enter to select · Esc to close
//! ```
//!
//! A full-width `▔` rule opens the panel, content is indented three columns,
//! options carry a two-column cursor slot ahead of a `1.` ordinal, the current
//! value is marked `✔` in the success role, and one dim hint closes the panel.
//! Every glyph here comes from a capture, not from taste, and every colour
//! goes through [`Styles`] so the dark, light and no-colour themes stay one
//! decision.
//!
//! [`crate::command_panel_frame`] draws the same rule for a different family
//! of panels and is deliberately kept separate for now; the two are recorded
//! as a known duplication for whoever merges the panel families.
//!
//! The panel replaces the composer and footer rather than stacking under them
//! ([`takes_over_composer`]); that is what makes the rule read as the top edge
//! of the panel instead of a divider floating in the middle of the shell.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::terminal::Styles;

/// Content indent shared by every row inside a panel.
pub const INDENT: &str = "   ";

/// Columns consumed by [`INDENT`] plus the two-column cursor slot.
pub const OPTION_INDENT: usize = 5;

/// Cursor slot glyph in front of a numbered option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// The highlighted row.
    Cursor,
    /// A plain row.
    None,
    /// The last visible row with more rows below it.
    MoreBelow,
    /// The first visible row with more rows above it.
    MoreAbove,
}

impl Marker {
    /// Glyph painted in the two-column slot.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Cursor => "❯ ",
            Self::None => "  ",
            Self::MoreBelow => "↓ ",
            Self::MoreAbove => "↑ ",
        }
    }

    /// Pick the cursor slot for one row of a scrolled window.
    #[must_use]
    pub const fn for_row(selected: bool, first_visible: bool, last_visible: bool) -> Self {
        if selected {
            Self::Cursor
        } else if first_visible {
            Self::MoreAbove
        } else if last_visible {
            Self::MoreBelow
        } else {
            Self::None
        }
    }
}

/// The full-width rule that opens a panel.
#[must_use]
pub fn top_rule(width: u16, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        "▔".repeat(usize::from(width)),
        Style::default().fg(styles.accent()),
    ))
}

/// A blank spacer row.
#[must_use]
pub fn blank() -> Line<'static> {
    Line::from(String::new())
}

/// The accented panel title.
#[must_use]
pub fn title(text: &str, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{text}"),
        Style::default().fg(styles.accent()).bold(),
    ))
}

/// A panel title followed by its tab strip, with the active tab reversed.
#[must_use]
pub fn title_with_tabs(text: &str, tabs: &[&str], active: usize, styles: Styles) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{INDENT}{text}"),
        Style::default().fg(styles.accent()).bold(),
    )];
    for (index, tab) in tabs.iter().enumerate() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {tab} "),
            if index == active {
                Style::default().fg(styles.accent()).reversed().bold()
            } else {
                Style::default().fg(styles.text())
            },
        ));
    }
    Line::from(spans)
}

/// The one emphasised line under the title that says what the panel decides.
#[must_use]
pub fn description(text: &str, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{text}"),
        Style::default().fg(styles.text()).bold(),
    ))
}

/// A quiet indented line: a current-value note, a read-only remark, a caption.
#[must_use]
pub fn note(text: &str, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{text}"),
        Style::default().fg(styles.dim()),
    ))
}

/// The closing hint line, e.g. `Enter to select · Esc to cancel`.
#[must_use]
pub fn hint(text: &str, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{text}"),
        Style::default().fg(styles.dim()),
    ))
}

/// A numbered option row.
///
/// `current` marks the committed value with `✔` and paints the label in the
/// success role, exactly as the reference does; `marker` owns the two-column
/// cursor slot so a scrolled list can put `↓` where the cursor would be.
#[must_use]
pub fn option(
    marker: Marker,
    number: usize,
    label: &str,
    current: bool,
    styles: Styles,
) -> Line<'static> {
    option_spans(
        marker,
        number,
        vec![Span::styled(
            label.to_owned(),
            Style::default().fg(if current {
                styles.success()
            } else {
                styles.text()
            }),
        )],
        current,
        styles,
    )
}

/// A numbered option row whose label is already styled by the caller.
#[must_use]
pub fn option_spans(
    marker: Marker,
    number: usize,
    label: Vec<Span<'static>>,
    current: bool,
    styles: Styles,
) -> Line<'static> {
    option_spans_padded(marker, number, 1, label, current, styles)
}

/// [`option_spans`] with a right-aligned ordinal `digits` wide.
///
/// A list that runs past nine would otherwise shift every label one column
/// when the ordinal gains a digit, which turns an aligned second column into
/// a ragged one.
#[must_use]
pub fn option_spans_padded(
    marker: Marker,
    number: usize,
    digits: usize,
    label: Vec<Span<'static>>,
    current: bool,
    styles: Styles,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{INDENT}{}", marker.glyph()),
            Style::default().fg(styles.accent()),
        ),
        Span::styled(
            format!("{number:>digits$}. "),
            Style::default().fg(styles.dim()),
        ),
    ];
    spans.extend(label);
    if current {
        spans.push(Span::styled(
            " ✔",
            Style::default().fg(styles.success()).bold(),
        ));
    }
    Line::from(spans)
}

/// Wrap a numbered option while retaining its styles and hanging label indent.
///
/// The ordinal and cursor appear only on the first row. Long identifiers split
/// at grapheme boundaries so a narrow panel never loses the selectable value.
#[must_use]
pub fn wrap_option(line: Line<'static>, width: u16) -> Vec<Line<'static>> {
    if line.width() <= usize::from(width) || line.spans.len() < 3 {
        return vec![line];
    }
    let indent: usize = line.spans[..2].iter().map(Span::width).sum();
    let capacity = usize::from(width).saturating_sub(indent).max(1);
    let glyphs: Vec<(String, Style)> = line.spans[2..]
        .iter()
        .flat_map(|span| {
            span.content
                .graphemes(true)
                .map(|glyph| (glyph.to_owned(), span.style))
        })
        .collect();
    let mut rows = Vec::new();
    let mut start = 0;
    while start < glyphs.len() {
        let mut end = start;
        let mut used = 0;
        let mut last_space = None;
        while end < glyphs.len() {
            let cell_width = glyphs[end].0.width();
            if used + cell_width > capacity && end > start {
                break;
            }
            used += cell_width;
            if glyphs[end].0.chars().all(char::is_whitespace) {
                last_space = Some(end);
            }
            end += 1;
        }
        if end < glyphs.len()
            && let Some(space) = last_space.filter(|space| *space > start)
        {
            end = space;
        }
        let mut spans = if rows.is_empty() {
            line.spans[..2].to_vec()
        } else {
            vec![Span::raw(" ".repeat(indent))]
        };
        for (glyph, style) in &glyphs[start..end] {
            if let Some(previous) = spans.last_mut()
                && previous.style == *style
            {
                previous.content.to_mut().push_str(glyph);
            } else {
                spans.push(Span::styled(glyph.clone(), *style));
            }
        }
        rows.push(Line::from(spans).style(line.style));
        start = end;
        while start < glyphs.len() && glyphs[start].0.chars().all(char::is_whitespace) {
            start += 1;
        }
    }
    rows
}

/// Fit a numbered list by its wrapped row heights, reserving the closing hint.
///
/// Options stay whole whenever possible. The selected option always gets space;
/// descriptions longer than the entire viewport are shortened after its first
/// row. Header prose and preview rows yield before the selected value or hint.
#[must_use]
pub fn option_window(
    mut header: Vec<Line<'static>>,
    options: Vec<Vec<Line<'static>>>,
    mut footer: Vec<Line<'static>>,
    selected: usize,
    available_rows: usize,
) -> Vec<Line<'static>> {
    if available_rows == 0 {
        return Vec::new();
    }
    if options.is_empty() {
        if footer.len() > available_rows {
            footer.drain(..footer.len() - available_rows);
        }
        header.truncate(available_rows.saturating_sub(footer.len()));
        header.extend(footer);
        return header;
    }
    let selected = selected.min(options.len() - 1);
    let selected_rows = options[selected]
        .len()
        .max(1)
        .min(available_rows.saturating_sub(2).max(1));
    // Leave at least one row for the selected option. In an exceptionally
    // short terminal retain the tail of the footer, which contains dismissal.
    while header
        .len()
        .saturating_add(footer.len())
        .saturating_add(selected_rows)
        > available_rows
    {
        if header.len() > 1 {
            header.pop();
        } else if footer.len() > 1 {
            footer.remove(0);
        } else if !header.is_empty() {
            header.pop();
        } else {
            footer.clear();
            break;
        }
    }
    let budget = available_rows.saturating_sub(header.len() + footer.len());
    let mut start = selected;
    let mut end = selected + 1;
    let mut used = options[selected].len().min(budget);
    while start > 0 && used.saturating_add(options[start - 1].len()) <= budget {
        start -= 1;
        used += options[start].len();
    }
    while end < options.len() && used.saturating_add(options[end].len()) <= budget {
        used += options[end].len();
        end += 1;
    }
    for (index, rows) in options.iter().enumerate().take(end).skip(start) {
        let mut rows = rows.clone();
        if rows.len() > budget {
            rows.truncate(budget);
            if rows.len() > 1 {
                let style = rows.last().map_or(Style::default(), |line| line.style);
                rows.pop();
                rows.push(Line::from(Span::styled(format!("{INDENT}…"), style)));
            }
        }
        if index != selected {
            let marker = Marker::for_row(
                false,
                index == start && start > 0,
                index + 1 == end && end < options.len(),
            );
            if let Some(span) = rows.first_mut().and_then(|line| line.spans.first_mut()) {
                // Wrapping can coalesce equally styled spans (notably without
                // colour). Replace only the cursor slot, preserving the label.
                let leading = span.content.len() - span.content.trim_start_matches(' ').len();
                span.content = format!(
                    "{}{}{}",
                    " ".repeat(leading.saturating_sub(2)),
                    marker.glyph(),
                    &span.content[leading..]
                )
                .into();
            }
        }
        header.extend(rows);
    }
    header.extend(footer);
    header
}

/// A secondary line belonging to the option above it.
#[must_use]
pub fn option_detail(text: &str, styles: Styles) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{}{text}", " ".repeat(OPTION_INDENT - INDENT.len())),
        Style::default().fg(styles.dim()),
    ))
}

/// The dashed rule that fences a preview block.
#[must_use]
pub fn preview_rule(width: u16, styles: Styles) -> Line<'static> {
    let inner = usize::from(width).saturating_sub(INDENT.len() * 2).max(1);
    Line::from(Span::styled(
        format!("{INDENT}{}", "╌".repeat(inner)),
        Style::default().fg(styles.border()),
    ))
}

/// Wrap `text` into indented rows that fit `width`.
///
/// Long words are not broken: a row that cannot fit one word carries it anyway
/// and lets the terminal clip, which is what the reference does rather than
/// splitting an identifier down the middle.
#[must_use]
pub fn wrap(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let available = usize::from(width).saturating_sub(INDENT.len() * 2).max(8);
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        if !row.is_empty() && UnicodeWidthStr::width(row.as_str()) + 1 + word.width() > available {
            rows.push(row.clone());
            row.clear();
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.into_iter()
        .map(|row| Line::from(Span::styled(format!("{INDENT}{row}"), style)))
        .collect()
}

/// Wrap body prose in the dim role.
#[must_use]
pub fn wrap_note(text: &str, width: u16, styles: Styles) -> Vec<Line<'static>> {
    wrap(text, width, Style::default().fg(styles.dim()))
}

/// Paint a panel: clear `area`, open with the rule, then the caller's rows.
///
/// Rows past the bottom of `area` are dropped here rather than by the terminal
/// so a short viewport degrades to a truncated panel instead of a scrolled
/// shell.
pub fn render(frame: &mut Frame<'_>, area: Rect, styles: Styles, lines: Vec<Line<'static>>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut rows = Vec::with_capacity(lines.len() + 1);
    rows.push(top_rule(area.width, styles));
    rows.extend(lines);
    rows.truncate(usize::from(area.height));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(rows), area);
}

/// Shrink `area` to the bottom `rows` content rows plus the top rule.
///
/// A panel that is handed more room than it fills would otherwise draw its
/// rule where the surface starts and leave dead space under the hint. The
/// reference always seats the rule directly above the panel's own first row.
#[must_use]
pub fn anchored(area: Rect, rows: usize) -> Rect {
    let wanted = height(rows);
    if wanted >= area.height {
        return area;
    }
    Rect {
        x: area.x,
        y: area.y + area.height - wanted,
        width: area.width,
        height: wanted,
    }
}

/// Height a panel needs for `rows` content rows, including the top rule.
#[must_use]
pub fn height(rows: usize) -> u16 {
    u16::try_from(rows.saturating_add(1)).unwrap_or(u16::MAX)
}

/// Receipt left behind when `/scroll-speed` closes without committing.
///
/// The reference reports the outcome of every dismissed picker rather than
/// closing in silence, so the transcript still records what the command did.
pub const SCROLL_SPEED_UNCHANGED: &str = "Scroll speed unchanged";

/// Receipt for a committed wheel speed, e.g. `Scroll speed set to: 1.5×`.
#[must_use]
pub fn scroll_speed_receipt(speed: &str) -> String {
    format!("Scroll speed set to: {speed}×")
}

/// Receipt for a committed theme, e.g. `Theme set to heycode-light`.
#[must_use]
pub fn theme_receipt(theme_id: &str) -> String {
    format!("Theme set to {theme_id}")
}

/// Whether the open surface is a command panel that replaces the composer.
///
/// The reference draws these panels where the composer and footer normally
/// are, so the shell hides both while one is open. Keeping the predicate here
/// rather than repeating three boolean chains in `draw_shell` means a new
/// panel opts in once.
#[must_use]
pub fn takes_over_composer(state: &crate::app::AppState) -> bool {
    state.permission_picker().is_some()
        || state.theme_picker().is_some()
        || state.keymap_picker().is_some()
        || state.scroll_speed_picker().is_some()
        || state.sandbox_panel().is_some()
        || state.autocompact_panel().is_some()
        // `/login` and `/connect` reopen setup as a panel inside a running
        // session; typing into the composer behind a login method chooser is
        // not an affordance the reference offers.
        || state.onboarding_is_panel()
        || state.capability_catalog().is_some_and(|panel| {
            matches!(
                panel.panel(),
                crate::panel_commands::CapabilityPanel::Hooks
                    | crate::panel_commands::CapabilityPanel::Agents
            )
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn top_rule_spans_the_whole_width() {
        let line = top_rule(12, Styles::default());
        assert_eq!(plain(&line), "▔".repeat(12));
    }

    #[test]
    fn render_opens_with_the_rule_and_keeps_row_order() {
        let styles = Styles::default();
        let backend = ratatui::backend::TestBackend::new(24, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 24, 4),
                    styles,
                    vec![
                        title("Theme", styles),
                        blank(),
                        hint("Esc to cancel", styles),
                    ],
                );
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(24)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered[0], "▔".repeat(24));
        assert_eq!(rendered[1], "   Theme");
        assert_eq!(rendered[2], "");
        assert_eq!(rendered[3], "   Esc to cancel");
    }

    #[test]
    fn render_truncates_instead_of_overflowing_a_short_panel() {
        let styles = Styles::default();
        let backend = ratatui::backend::TestBackend::new(12, 2);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 12, 2),
                    styles,
                    vec![title("A", styles), title("B", styles), title("C", styles)],
                );
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(12)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(rendered, vec!["▔".repeat(12), "   A".to_owned()]);
    }

    #[test]
    fn numbered_options_indent_and_mark_the_current_value() {
        let styles = Styles::default();
        assert_eq!(
            plain(&option(Marker::None, 1, "Auto", false, styles)),
            "     1. Auto"
        );
        assert_eq!(
            plain(&option(Marker::Cursor, 2, "Dark mode", true, styles)),
            "   ❯ 2. Dark mode ✔"
        );
        assert_eq!(
            plain(&option(Marker::MoreBelow, 5, "Last", false, styles)),
            "   ↓ 5. Last"
        );
    }

    #[test]
    fn narrow_option_keeps_description_current_mark_and_grapheme_styles() {
        let styles = Styles::default();
        let original = option_spans(
            Marker::Cursor,
            3,
            vec![
                Span::styled(
                    "Connect a subscription ",
                    Style::default().fg(styles.text()).bold(),
                ),
                Span::styled(
                    "· Claude and ChatGPT subscriptions with café and 界",
                    Style::default().fg(styles.dim()),
                ),
            ],
            true,
            styles,
        );
        let rows = wrap_option(original, 32);
        assert!(rows.len() > 1);
        assert!(plain(&rows[0]).starts_with("   ❯ 3. "));
        assert!(
            rows.iter()
                .skip(1)
                .all(|row| plain(row).starts_with("        "))
        );
        assert!(rows.iter().all(|row| row.width() <= 32));
        let text = rows.iter().map(plain).collect::<Vec<_>>().join(" ");
        assert!(text.contains("café"));
        assert!(text.contains('界'));
        assert!(text.ends_with(" ✔"));
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.content.contains("Claude") && span.style.fg == Some(styles.dim()))
        );
    }

    #[test]
    fn the_current_mark_uses_the_success_role() {
        let styles = Styles::default();
        let line = option(Marker::Cursor, 2, "Dark mode", true, styles);
        let mark = line.spans.last().unwrap();
        assert_eq!(mark.content.as_ref(), " ✔");
        assert_eq!(mark.style.fg, Some(styles.success()));
        let label = &line.spans[2];
        assert_eq!(label.style.fg, Some(styles.success()));
    }

    #[test]
    fn a_plain_option_label_stays_in_the_text_role() {
        let styles = Styles::default();
        let line = option(Marker::None, 1, "Auto", false, styles);
        assert_eq!(line.spans[2].style.fg, Some(styles.text()));
        assert!(line.spans.iter().all(|span| span.content != " ✔"));
    }

    #[test]
    fn option_details_align_under_the_option_label() {
        let styles = Styles::default();
        assert_eq!(
            plain(&option_detail("We won't ask.", styles)),
            "     We won't ask."
        );
    }

    #[test]
    fn hint_and_note_lines_share_the_content_indent() {
        let styles = Styles::default();
        assert_eq!(
            plain(&hint("Enter to select · Esc to cancel", styles)),
            "   Enter to select · Esc to cancel"
        );
        assert_eq!(plain(&note("0 registered", styles)), "   0 registered");
        assert_eq!(
            hint("x", styles).spans[0].style.fg,
            Some(styles.dim()),
            "hints stay quiet"
        );
    }

    #[test]
    fn the_active_tab_is_the_only_reversed_one() {
        let styles = Styles::default();
        let line = title_with_tabs("Sandbox", &["Mode", "Overrides"], 0, styles);
        assert_eq!(plain(&line), "   Sandbox  Mode   Overrides ");
        assert_eq!(line.spans[2].style.fg, Some(styles.accent()));
        assert!(
            line.spans[2]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            !line.spans[4]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
    }

    #[test]
    fn narrow_panels_wrap_prose_on_word_boundaries() {
        let styles = Styles::default();
        let rows = wrap_note(
            "Commands will try to run in the sandbox automatically",
            30,
            styles,
        );
        let text = rows.iter().map(plain).collect::<Vec<_>>();
        assert!(text.len() > 1, "{text:?} should wrap");
        for row in &text {
            assert!(row.starts_with(INDENT));
            assert!(UnicodeWidthStr::width(row.as_str()) <= 27, "{row:?}");
        }
        assert_eq!(
            text.join(" ").replace(INDENT, ""),
            "Commands will try to run in the sandbox automatically"
        );
    }

    #[test]
    fn a_word_wider_than_the_panel_is_kept_whole() {
        let styles = Styles::default();
        let rows = wrap_note("short supercalifragilisticexpialidocious", 20, styles);
        let text = rows.iter().map(plain).collect::<Vec<_>>();
        assert_eq!(
            text,
            vec![
                "   short".to_owned(),
                "   supercalifragilisticexpialidocious".to_owned()
            ]
        );
    }

    #[test]
    fn scroll_markers_prefer_the_cursor() {
        assert_eq!(Marker::for_row(true, true, false), Marker::Cursor);
        assert_eq!(Marker::for_row(false, true, false), Marker::MoreAbove);
        assert_eq!(Marker::for_row(false, false, true), Marker::MoreBelow);
        assert_eq!(Marker::for_row(false, false, false), Marker::None);
    }

    #[test]
    fn receipts_read_as_the_reference_writes_them() {
        assert_eq!(SCROLL_SPEED_UNCHANGED, "Scroll speed unchanged");
        assert_eq!(scroll_speed_receipt("1.5"), "Scroll speed set to: 1.5×");
        assert_eq!(theme_receipt("heycode-light"), "Theme set to heycode-light");
    }

    #[test]
    fn height_accounts_for_the_top_rule() {
        assert_eq!(height(0), 1);
        assert_eq!(height(7), 8);
    }

    #[test]
    fn anchoring_seats_a_short_panel_on_the_bottom_edge() {
        let area = Rect::new(0, 4, 30, 18);
        assert_eq!(anchored(area, 6), Rect::new(0, 15, 30, 7));
        assert_eq!(anchored(area, 40), area, "a tall panel keeps every row");
    }
}

#[cfg(test)]
mod option_window_tests {
    use super::*;

    #[test]
    fn scroll_markers_preserve_labels_coalesced_in_no_colour_rows() {
        let rows = option_window(
            Vec::new(),
            vec![vec![Line::raw("❯ 1. Alpha")], vec![Line::raw("  2. Beta")]],
            Vec::new(),
            0,
            3,
        );
        assert_eq!(rows[1].to_string(), "  2. Beta");
    }

    fn options(selected: usize, width: u16) -> Vec<Vec<Line<'static>>> {
        (0..12)
            .map(|index| {
                wrap_option(
                    option_spans(
                        if index == selected {
                            Marker::Cursor
                        } else {
                            Marker::None
                        },
                        index + 1,
                        vec![
                            Span::styled(
                                format!("Provider {index} with a descriptive label "),
                                Style::default().fg(ratatui::style::Color::Green),
                            ),
                            Span::styled(
                                "· configurable extension settings and connection requirements",
                                Style::default().fg(ratatui::style::Color::Gray),
                            ),
                        ],
                        false,
                        Styles::default(),
                    ),
                    width,
                )
            })
            .collect()
    }

    #[test]
    fn wrapped_window_keeps_each_selection_and_footer_visible() {
        for selected in 0..12 {
            let lines = option_window(
                vec![title("Providers", Styles::default()), blank()],
                options(selected, 60),
                vec![blank(), hint("Esc to close", Styles::default())],
                selected,
                17,
            );
            assert!(lines.len() <= 17);
            assert!(lines.iter().all(|line| line.width() <= 60));
            assert!(lines.iter().any(|line| {
                line.to_string()
                    .contains(&format!("❯ {}. Provider {selected}", selected + 1))
            }));
            assert_eq!(
                lines.last().map(ToString::to_string).as_deref(),
                Some("   Esc to close")
            );
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.content.contains("Provider")
                        && span.style.fg == Some(ratatui::style::Color::Green))
            );
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.content.contains("configurable")
                        && span.style.fg == Some(ratatui::style::Color::Gray))
            );
        }
    }

    #[test]
    fn long_selected_description_yields_prose_space_before_identity_or_hint() {
        let options = vec![wrap_option(
            option(
                Marker::Cursor,
                1,
                &"long-identifier-".repeat(50),
                false,
                Styles::default(),
            ),
            60,
        )];
        let lines = option_window(
            vec![
                title("Title", Styles::default()),
                blank(),
                note("Explanatory header", Styles::default()),
            ],
            options,
            vec![blank(), hint("Esc to close", Styles::default())],
            0,
            8,
        );
        assert!(lines.len() <= 8);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().starts_with("   ❯ 1. long-identifier"))
        );
        assert_eq!(
            lines.last().map(ToString::to_string).as_deref(),
            Some("   Esc to close")
        );
    }

    #[test]
    fn wide_window_keeps_complete_option_order() {
        let options = options(5, 110);
        let expected = options
            .iter()
            .flatten()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let actual = option_window(Vec::new(), options, Vec::new(), 5, usize::MAX)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}
