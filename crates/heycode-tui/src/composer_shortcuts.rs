//! The compact shortcut list a standalone `?` opens under the composer.
//!
//! Two rules shape this module.
//!
//! **Every row names a binding this build actually honours.** The list is
//! built from the live [`Keymap`] wherever a chord can be rebound, and from the
//! literal composer affordances (`/`, `\` + Return, Shift+Tab) where the
//! handler is position-sensitive rather than bound. A hint for a key that does
//! nothing is worse than no hint, so entries with no heycode equivalent are absent
//! rather than approximated.
//!
//! **The geometry is a measurement, not a guess.** Column origins and the
//! wrapping budget were read from a pinned Claude Code 2.1.269 capture at
//! several terminal widths (`tmp/terminal-checks/…-footer-reference*`), which
//! places three columns at x=2, x=26 and x=61 on a 110-column terminal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use heycode_ui::keymap::{KeyChord, KeyName, Keymap, KeymapAction};

/// Left inset shared with the idle footer line.
const MARGIN: u16 = 2;

/// Blank columns between one column's text and the next column's origin.
const GAP: usize = 2;

/// Widest a column is allowed to grow before its longest entry wraps.
///
/// Read from the reference capture: at 110 columns the source lets column one
/// reach its natural 22 cells while column two stops at 33 and wraps
/// `backslash (\) + return (⏎) for newline` onto a second row.
const COLUMN_LIMIT: usize = 33;

/// The three shortcut columns, each already resolved against the live keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortcutColumns {
    columns: [Vec<String>; 3],
}

impl ShortcutColumns {
    /// Resolve the list this build can honestly offer.
    pub(crate) fn resolve(keymap: &Keymap) -> Self {
        let chord = |action: KeymapAction| spell(keymap.chord(action));
        Self {
            columns: [
                vec![
                    "/ for commands".to_owned(),
                    "/mention for files".to_owned(),
                    "/btw for side question".to_owned(),
                ],
                vec![
                    format!("{} to clear input", chord(KeymapAction::Quit)),
                    "shift + tab to cycle permissions".to_owned(),
                    format!(
                        "{} for verbose output",
                        chord(KeymapAction::ToggleTranscript)
                    ),
                    format!("{} to toggle tasks", chord(KeymapAction::ToggleTasks)),
                    "backslash (\\) + return (⏎) for newline".to_owned(),
                ],
                vec![
                    "/model to switch model".to_owned(),
                    "/keybindings to customize".to_owned(),
                ],
            ],
        }
    }

    /// Column origins and text widths for one terminal width.
    fn geometry(&self, width: u16) -> [Column; 3] {
        let inner = usize::from(width.saturating_sub(MARGIN));
        let ideal: [usize; 3] = std::array::from_fn(|index| {
            self.columns[index]
                .iter()
                .map(|entry| entry.width())
                .max()
                .unwrap_or(0)
                .min(COLUMN_LIMIT)
        });
        let content = inner.saturating_sub(GAP * 2);
        let total: usize = ideal.iter().sum();
        // Wide enough for every column at its ideal width: the last column
        // simply keeps whatever is left, exactly as the reference does.
        let share = |ideal: usize| {
            content
                .saturating_mul(ideal)
                .checked_div(total)
                .unwrap_or_default()
        };
        let (first, second) = if content >= total {
            (ideal[0], ideal[1])
        } else {
            (share(ideal[0]), share(ideal[1]))
        };
        let third = content.saturating_sub(first).saturating_sub(second);
        let mut origin = usize::from(MARGIN);
        let mut columns = Vec::with_capacity(3);
        for text_width in [first, second, third] {
            columns.push(Column {
                origin,
                width: text_width,
            });
            origin += text_width + GAP;
        }
        [columns[0], columns[1], columns[2]]
    }

    /// The rendered rows, as plain text, for one terminal width.
    ///
    /// Rendering and assertion share this function so a wrapping test never
    /// has to reconstruct the layout it is checking.
    pub(crate) fn rows(&self, width: u16) -> Vec<String> {
        let geometry = self.geometry(width);
        let wrapped: Vec<Vec<String>> = (0..3)
            .map(|index| {
                self.columns[index]
                    .iter()
                    .flat_map(|entry| wrap(entry, geometry[index].width))
                    .collect()
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(0);
        (0..height)
            .map(|row| {
                let mut line = String::new();
                for index in 0..3 {
                    let Some(text) = wrapped[index].get(row) else {
                        continue;
                    };
                    if text.is_empty() {
                        continue;
                    }
                    let origin = geometry[index].origin;
                    let pad = origin.saturating_sub(line.width());
                    line.push_str(&" ".repeat(pad));
                    line.push_str(text);
                }
                line
            })
            .collect()
    }

    /// Rows this list needs at one terminal width.
    pub(crate) fn height(&self, width: u16) -> u16 {
        u16::try_from(self.rows(width).len()).unwrap_or(u16::MAX)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    origin: usize,
    width: usize,
}

/// Draw the list in the dim role, one entry per grid cell.
pub(crate) fn draw(frame: &mut Frame<'_>, area: Rect, columns: &ShortcutColumns, dim: Style) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let rows = columns.rows(area.width);
    frame.render_widget(
        Paragraph::new(
            rows.into_iter()
                .take(usize::from(area.height))
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .style(dim),
        area,
    );
}

/// Spell a chord the way the reference spells one: lowercase names joined by
/// ` + `, so `ctrl+t` reads as `ctrl + t`.
fn spell(chord: KeyChord) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    let modifiers = chord.modifiers();
    if modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if modifiers.alt {
        parts.push("alt".to_owned());
    }
    if modifiers.shift {
        parts.push("shift".to_owned());
    }
    parts.push(match chord.key() {
        KeyName::Enter => "return".to_owned(),
        other => other.to_string(),
    });
    parts.join(" + ")
}

/// Break one entry on spaces to fit a column, keeping an over-long word whole
/// rather than cutting a chord name in half.
fn wrap(entry: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    for word in entry.split(' ') {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if candidate.width() > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            current = word.to_owned();
        } else {
            current = candidate;
        }
    }
    rows.push(current);
    rows
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{COLUMN_LIMIT, ShortcutColumns};
    use heycode_ui::keymap::Keymap;
    use unicode_width::UnicodeWidthStr;

    fn columns() -> ShortcutColumns {
        ShortcutColumns::resolve(&Keymap::default())
    }

    /// The reference places the three columns at x=2, x=26 and x=61 on a
    /// 110-column terminal; matching that is the whole point of the layout.
    #[test]
    fn wide_terminal_uses_the_reference_column_origins() {
        let rows = columns().rows(110);
        let first = rows.first().unwrap();
        assert!(first.starts_with("  / for commands"), "{first}");
        let second = &rows[1];
        assert_eq!(second.find("/mention for files"), Some(2));
        assert_eq!(
            rows.iter().find_map(|row| row.find("shift + tab")),
            Some(26)
        );
        assert_eq!(
            rows.iter().find_map(|row| row.find("/keybindings")),
            Some(61)
        );
    }

    #[test]
    fn every_entry_names_a_binding_this_build_honours() {
        let rows = columns().rows(110).join("\n");
        for expected in [
            "/ for commands",
            "/mention for files",
            "/btw for side question",
            "ctrl + c to clear input",
            "shift + tab to cycle permissions",
            "ctrl + o for verbose output",
            "ctrl + t to toggle tasks",
            "/model to switch model",
            "/keybindings to customize",
        ] {
            assert!(rows.contains(expected), "missing {expected} in\n{rows}");
        }
        for absent in ["for shell mode", "to suspend", "$EDITOR", "stash prompt"] {
            assert!(!rows.contains(absent), "unsupported {absent} in\n{rows}");
        }
    }

    /// The longest column-two entry is the one the reference wraps.
    #[test]
    fn newline_entry_wraps_onto_a_second_row() {
        let rows = columns().rows(110);
        assert!(
            rows.iter()
                .any(|row| row.contains("backslash (\\) + return (⏎) for")),
            "{rows:?}"
        );
        assert!(rows.iter().any(|row| row.trim() == "newline"), "{rows:?}");
        assert!(
            rows.iter().all(|row| row.width() <= 110),
            "a row overflowed: {rows:?}"
        );
    }

    #[test]
    fn narrow_terminal_keeps_three_columns_inside_the_viewport() {
        let rows = columns().rows(60);
        assert!(!rows.is_empty());
        for row in &rows {
            assert!(row.width() <= 60, "row overflowed 60 columns: {row:?}");
        }
        assert!(rows.len() > columns().rows(110).len(), "{rows:?}");
    }

    #[test]
    fn height_matches_the_rendered_row_count() {
        for width in [40_u16, 60, 80, 110, 200] {
            assert_eq!(
                usize::from(columns().height(width)),
                columns().rows(width).len(),
                "width {width}"
            );
        }
    }

    #[test]
    fn no_column_exceeds_the_measured_wrapping_budget() {
        let rows = columns().rows(200);
        let origin = rows.iter().find_map(|row| row.find("shift + tab")).unwrap();
        assert!(origin <= 2 + COLUMN_LIMIT + 2, "{origin}");
    }

    /// Every theme and colour tier reaches the list through one role, so the
    /// text is identical everywhere and the colour is whatever `dim` resolves
    /// to — including "no colour at all".
    #[test]
    fn every_theme_and_colour_tier_paints_the_list_in_the_dim_role() {
        use heycode_ui::terminal::ColorLevel;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::style::Style;

        let list = columns();
        let expected = list.rows(110);
        for theme in heycode_ui::theme::builtin_themes().unwrap() {
            for level in [
                ColorLevel::None,
                ColorLevel::Basic,
                ColorLevel::Ansi256,
                ColorLevel::TrueColor,
            ] {
                let styles = crate::terminal::Styles::new(&theme.resolve(level));
                let mut terminal = Terminal::new(TestBackend::new(110, 8)).unwrap();
                terminal
                    .draw(|frame| {
                        super::draw(
                            frame,
                            Rect::new(0, 0, 110, 8),
                            &list,
                            Style::default().fg(styles.dim()),
                        );
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer().clone();
                for (row, text) in expected.iter().enumerate() {
                    let rendered: String = (0..110)
                        .map(|column| {
                            buffer[(column, u16::try_from(row).unwrap())]
                                .symbol()
                                .to_owned()
                        })
                        .collect();
                    assert_eq!(
                        rendered.trim_end(),
                        text.as_str(),
                        "{} at {level:?}",
                        theme.id().as_str()
                    );
                }
                assert_eq!(
                    buffer[(2, 0)].style().fg,
                    Some(styles.dim()),
                    "{} at {level:?}",
                    theme.id().as_str()
                );
            }
        }
    }

    mod interaction {
        use crate::app::AppState;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        fn state() -> AppState {
            AppState::new("m", std::path::PathBuf::from("/p"))
        }

        fn press(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
            state.handle_terminal_event(&Event::Key(KeyEvent::new(code, modifiers)));
        }

        fn typed(state: &AppState) -> String {
            state.input.lines().join("\n")
        }

        #[test]
        fn question_mark_on_an_empty_composer_opens_the_list_without_typing() {
            let mut state = state();
            press(&mut state, KeyCode::Char('?'), KeyModifiers::NONE);
            assert!(state.shortcut_list_visible());
            assert_eq!(typed(&state), "");
        }

        /// Terminals differ on whether `?` arrives with the shift flag set.
        #[test]
        fn shifted_question_mark_opens_the_list_too() {
            let mut state = state();
            press(&mut state, KeyCode::Char('?'), KeyModifiers::SHIFT);
            assert!(state.shortcut_list_visible());
            assert_eq!(typed(&state), "");
        }

        #[test]
        fn backspace_and_escape_each_close_the_list() {
            for closing in [KeyCode::Backspace, KeyCode::Esc] {
                let mut state = state();
                press(&mut state, KeyCode::Char('?'), KeyModifiers::NONE);
                assert!(state.shortcut_list_visible());
                press(&mut state, closing, KeyModifiers::NONE);
                assert!(!state.shortcut_list_visible(), "{closing:?}");
                assert_eq!(typed(&state), "", "{closing:?}");
            }
        }

        #[test]
        fn question_mark_inside_a_draft_stays_ordinary_text() {
            let mut state = state();
            for character in "hello?".chars() {
                press(&mut state, KeyCode::Char(character), KeyModifiers::NONE);
            }
            assert_eq!(typed(&state), "hello?");
            assert!(!state.shortcut_list_visible());
        }

        #[test]
        fn typing_after_the_list_opens_returns_the_footer_and_keeps_the_character() {
            let mut state = state();
            press(&mut state, KeyCode::Char('?'), KeyModifiers::NONE);
            press(&mut state, KeyCode::Char('a'), KeyModifiers::NONE);
            assert!(!state.shortcut_list_visible());
            assert_eq!(typed(&state), "a");
        }
    }

    mod footer {
        use crate::permission_picker::permission_footer_label;

        /// The wording per mode, read from one Claude Code 2.1.269 capture per
        /// mode; `full_access` and `deny` keep heycode's own names.
        #[test]
        fn each_mode_has_its_captured_phrasing() {
            for (mode, expected) in [
                ("ask", "⏸ manual mode on"),
                ("default", "⏸ manual mode on"),
                ("plan", "⏸ plan mode on"),
                ("accepted_edits", "⏵⏵ accept edits on"),
                ("auto", "⏵⏵ auto mode on"),
                ("full_access", "⏵⏵ full access on"),
                ("deny", "⏸ tools blocked"),
            ] {
                assert_eq!(permission_footer_label(mode), expected, "{mode}");
            }
        }

        #[test]
        fn an_unknown_mode_never_claims_a_glyph_it_has_not_earned() {
            assert_eq!(permission_footer_label(""), "");
            assert_eq!(permission_footer_label("custom"), "⏸ custom on");
        }
    }

    #[test]
    fn rebound_chord_is_spelled_from_the_live_keymap() {
        use heycode_ui::keymap::{KeyChord, KeymapAction};
        let overrides = [(
            KeymapAction::ToggleTasks,
            KeyChord::parse("alt+shift+k").unwrap(),
        )]
        .into_iter()
        .collect();
        let keymap = Keymap::resolve(&overrides).unwrap();
        let rows = ShortcutColumns::resolve(&keymap).rows(110).join("\n");
        assert!(rows.contains("alt + shift + k to toggle tasks"), "{rows}");
        assert!(!rows.contains("ctrl + t to toggle tasks"), "{rows}");
    }
}
