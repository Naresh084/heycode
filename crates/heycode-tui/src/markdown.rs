//! Markdown → styled ratatui lines, with syntect-highlighted fenced code.

use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

use crate::palette;

struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

static HIGHLIGHTER: std::sync::OnceLock<Highlighter> = std::sync::OnceLock::new();

/// Syntax spans for a declared, recognized language. Unknown languages retain
/// the caller's plain-text palette; no language is guessed from source text.
pub(crate) fn highlight_code(
    language: &str,
    code: &str,
    light_background: bool,
) -> Option<Vec<Line<'static>>> {
    let highlighter = HIGHLIGHTER.get_or_init(Highlighter::new);
    highlighter.syntaxes.find_syntax_by_token(language)?;
    Some(highlighter.highlight_block_with_theme(
        Some(language),
        &terminal_safe_markdown(code),
        if light_background {
            "base16-ocean.light"
        } else {
            "base16-ocean.dark"
        },
    ))
}

impl Highlighter {
    fn new() -> Self {
        Self {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            themes: ThemeSet::load_defaults(),
        }
    }

    fn highlight_block(&self, lang: Option<&str>, code: &str) -> Vec<Line<'static>> {
        self.highlight_block_with_theme(lang, code, "base16-ocean.dark")
    }

    fn highlight_block_with_theme(
        &self,
        lang: Option<&str>,
        code: &str,
        theme: &str,
    ) -> Vec<Line<'static>> {
        let syntax = lang
            .and_then(|l| self.syntaxes.find_syntax_by_token(l))
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let theme = &self.themes.themes[theme];
        let mut out = Vec::new();
        let mut hl = syntect::easy::HighlightLines::new(syntax, theme);
        for line in syntect::util::LinesWithEndings::from(code) {
            let ranges = hl.highlight_line(line, &self.syntaxes).unwrap_or_default();
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (style, text) in ranges {
                let trimmed = text.trim_end_matches('\n');
                if trimmed.is_empty() {
                    continue;
                }
                let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                spans.push(Span::styled(trimmed.to_owned(), Style::default().fg(fg)));
            }
            out.push(Line::from(spans));
        }
        if out.is_empty() {
            out.push(Line::from(Span::raw(String::new())));
        }
        out
    }
}

fn inline_style(fg: Option<Color>, bold: bool, italic: bool) -> Style {
    let mut style = Style::default().fg(fg.unwrap_or(palette::TEXT));
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// Render markdown `text` into wrapped styled lines at `width`.
#[must_use]
pub fn render_markdown(text: &str, width: usize) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, Tag, TagEnd};
    let mut lines = Vec::new();
    let text = terminal_safe_markdown(text);
    let mut events = pulldown_cmark::Parser::new_ext(
        &text,
        pulldown_cmark::Options::ENABLE_STRIKETHROUGH | pulldown_cmark::Options::ENABLE_TABLES,
    )
    .peekable();
    let mut paragraph = Vec::<Span<'static>>::new();
    let mut lists = Vec::<Option<u64>>::new();
    let mut indents = Vec::<usize>::new();
    let mut heading = false;
    let mut strong = 0_usize;
    let mut emphasis = 0_usize;
    let mut strike = 0_usize;
    let flush = |lines: &mut Vec<Line<'static>>,
                 paragraph: &mut Vec<Span<'static>>,
                 indent: usize,
                 separate: bool| {
        if paragraph.iter().all(|span| span.content.trim().is_empty()) {
            paragraph.clear();
            return;
        }
        lines.extend(wrap_styled(paragraph, width, indent));
        paragraph.clear();
        if separate {
            lines.push(Line::default());
        }
    };
    while let Some(event) = events.next() {
        let indent = indents.last().copied().unwrap_or(0);
        match event {
            Event::Start(Tag::Table(alignments)) => {
                flush(&mut lines, &mut paragraph, indent, false);
                lines.extend(render_table(&mut events, &alignments, width, indent));
                lines.push(Line::default());
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut lines, &mut paragraph, indent, lists.is_empty());
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().map(str::to_owned)
                    }
                    _ => None,
                };
                let mut code = String::new();
                for event in events.by_ref() {
                    match event {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(text) => code.push_str(&text),
                        _ => {}
                    }
                }
                for mut line in HIGHLIGHTER
                    .get_or_init(Highlighter::new)
                    .highlight_block(lang.as_deref(), &code)
                {
                    line.spans.insert(0, Span::raw(" ".repeat(indent + 2)));
                    lines.extend(wrap_styled(&line.spans, width, indent + 2));
                }
                lines.push(Line::default());
            }
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut lines, &mut paragraph, indent, false);
                heading = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut lines, &mut paragraph, indent, true);
                heading = false;
            }
            Event::Start(Tag::List(start)) => {
                flush(&mut lines, &mut paragraph, indent, false);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut lines, &mut paragraph, indent, false);
                lists.pop();
                if lists.is_empty() {
                    lines.push(Line::default());
                }
            }
            Event::Start(Tag::Item) => {
                flush(&mut lines, &mut paragraph, indent, false);
                let marker = if let Some(Some(number)) = lists.last_mut() {
                    let marker = format!("{number}. ");
                    *number = number.saturating_add(1);
                    marker
                } else {
                    "• ".into()
                };
                let prefix = format!("{}{marker}", "  ".repeat(lists.len().saturating_sub(1)));
                indents.push(prefix.width());
                paragraph.push(Span::raw(prefix));
            }
            Event::End(TagEnd::Item) => {
                flush(&mut lines, &mut paragraph, indent, false);
                indents.pop();
            }
            Event::Start(Tag::Paragraph) if paragraph.is_empty() && indent > 0 => {
                paragraph.push(Span::raw(" ".repeat(indent)))
            }
            Event::End(TagEnd::Paragraph) => {
                flush(&mut lines, &mut paragraph, indent, lists.is_empty())
            }
            Event::Start(Tag::Strong) => strong += 1,
            Event::End(TagEnd::Strong) => strong = strong.saturating_sub(1),
            Event::Start(Tag::Emphasis) => emphasis += 1,
            Event::End(TagEnd::Emphasis) => emphasis = emphasis.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::End(TagEnd::Strikethrough) => strike = strike.saturating_sub(1),
            Event::Text(text) => {
                let mut style = if heading {
                    inline_style(Some(palette::TEXT), true, false)
                } else {
                    Style::default()
                };
                if strong > 0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if emphasis > 0 {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if strike > 0 {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                paragraph.push(Span::styled(text.into_string(), style));
            }
            Event::Code(code) => paragraph.push(Span::styled(
                code.into_string(),
                Style::default().fg(palette::CODE),
            )),
            Event::SoftBreak => paragraph.push(Span::raw(" ")),
            Event::HardBreak => {
                flush(&mut lines, &mut paragraph, indent, false);
                paragraph.push(Span::raw(" ".repeat(indent)));
            }
            _ => {}
        }
    }
    flush(&mut lines, &mut paragraph, 0, false);
    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    lines
}

fn render_table<'a>(
    events: &mut impl Iterator<Item = pulldown_cmark::Event<'a>>,
    alignments: &[pulldown_cmark::Alignment],
    width: usize,
    indent: usize,
) -> Vec<Line<'static>> {
    use pulldown_cmark::{Alignment, Event, Tag, TagEnd};
    let mut rows: Vec<Vec<Vec<Span<'static>>>> = Vec::new();
    let mut row = Vec::new();
    let mut cell = Vec::new();
    let mut strong = 0usize;
    let mut emphasis = 0usize;
    for event in events.by_ref() {
        match event {
            Event::End(TagEnd::Table) => break,
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut cell)),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
            Event::Start(Tag::Strong) => strong += 1,
            Event::End(TagEnd::Strong) => strong = strong.saturating_sub(1),
            Event::Start(Tag::Emphasis) => emphasis += 1,
            Event::End(TagEnd::Emphasis) => emphasis = emphasis.saturating_sub(1),
            Event::Text(text) => {
                let mut style = Style::default();
                if strong > 0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if emphasis > 0 {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                cell.push(Span::styled(text.into_string(), style));
            }
            Event::Code(text) => cell.push(Span::styled(
                text.into_string(),
                Style::default().fg(palette::CODE),
            )),
            Event::SoftBreak | Event::HardBreak => cell.push(Span::raw(" ")),
            _ => {}
        }
    }
    let columns = alignments.len();
    if columns == 0 || rows.is_empty() {
        return Vec::new();
    }
    let available = width.saturating_sub(indent);
    let content_width = available.saturating_sub(columns.saturating_mul(3).saturating_add(1));
    // A table with fewer than three columns of space per cell becomes labeled records. No
    // cell is truncated or silently hidden on a narrow terminal.
    if content_width < columns.saturating_mul(3) {
        let mut lines = Vec::new();
        for row in rows.iter().skip(1) {
            for (column, cell) in row.iter().enumerate() {
                let mut spans = rows[0].get(column).cloned().unwrap_or_default();
                for span in &mut spans {
                    span.style = span.style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::raw(": "));
                spans.extend(cell.iter().cloned());
                lines.extend(wrap_styled(&spans, width.max(1), indent));
            }
            lines.push(Line::default());
        }
        if rows.len() == 1 {
            for cell in &rows[0] {
                lines.extend(wrap_styled(cell, width.max(1), indent));
            }
        }
        return lines;
    }
    let mut widths = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.iter().map(Span::width).sum::<usize>())
                .max()
                .unwrap_or(1)
                .max(1)
                .min(content_width)
        })
        .collect::<Vec<_>>();
    let mut used = widths.iter().sum::<usize>();
    while used > content_width {
        let Some((column, _)) = widths.iter().enumerate().max_by_key(|(_, size)| **size) else {
            break;
        };
        widths[column] -= 1;
        used -= 1;
    }
    let border = |left: char, middle: char, right: char| {
        let body = widths
            .iter()
            .map(|size| "─".repeat(size + 2))
            .collect::<Vec<_>>()
            .join(&middle.to_string());
        Line::from(Span::styled(
            format!("{}{left}{body}{right}", " ".repeat(indent)),
            Style::default().fg(palette::DIM),
        ))
    };
    let mut lines = vec![border('┌', '┬', '┐')];
    for (row_index, row) in rows.iter().enumerate() {
        let wrapped = (0..columns)
            .map(|column| {
                let mut cell = row.get(column).cloned().unwrap_or_default();
                if row_index == 0 {
                    for span in &mut cell {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                }
                wrap_styled(&cell, widths[column], 0)
            })
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for y in 0..height {
            let mut spans = vec![Span::raw(format!("{}│", " ".repeat(indent)))];
            for (column, cell) in wrapped.iter().enumerate() {
                let line = cell.get(y).cloned().unwrap_or_default();
                let padding = widths[column].saturating_sub(line.width());
                let leading = match alignments[column] {
                    Alignment::Right => padding,
                    Alignment::Center => padding / 2,
                    _ => 0,
                };
                spans.push(Span::raw(" ".repeat(leading + 1)));
                spans.extend(line.spans);
                spans.push(Span::raw(format!("{}│", " ".repeat(padding - leading + 1))));
            }
            lines.push(Line::from(spans));
        }
        if row_index == 0 {
            lines.push(border('├', '┼', '┤'));
        }
    }
    lines.push(border('└', '┴', '┘'));
    lines
}

/// Upper bound on the rows `render_markdown` produces for `text` at `width`.
///
/// The transcript height index treats this as a correctness contract, not a
/// hint (`crate::transcript`). One source row occupies `ceil(width / columns)`
/// ideal rows; greedy word wrapping never breaks a row before it is half full,
/// so it costs at most twice that, and each block adds one blank separator,
/// which is itself bounded by the source row count.
#[must_use]
pub(crate) fn render_markdown_height_bound(text: &str, width: usize) -> usize {
    if text.contains('|')
        && pulldown_cmark::Parser::new_ext(text, pulldown_cmark::Options::ENABLE_TABLES).any(
            |event| {
                matches!(
                    event,
                    pulldown_cmark::Event::Start(pulldown_cmark::Tag::Table(_))
                )
            },
        )
    {
        return render_markdown(text, width).len().max(1);
    }
    let indentation = text
        .lines()
        .map(|line| {
            let leading = line.chars().take_while(|ch| ch.is_whitespace()).count();
            let numbered = line
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .count();
            leading.saturating_add(numbered).saturating_add(4)
        })
        .max()
        .unwrap_or(4);
    let columns = width.saturating_sub(indentation).max(1);
    text.lines()
        .map(|line| line.width().max(1).div_ceil(columns))
        .sum::<usize>()
        .max(1)
        .saturating_mul(3)
        .saturating_add(2)
}

/// Normalize one already-laid-out span so no control byte reaches a cell.
///
/// A span is part of exactly one rendered line, so unlike
/// `terminal_safe_markdown` there is no newline to preserve: `\n`, `\r` and
/// `\t` collapse to a space and every other control character becomes
/// U+FFFD. Clean input is returned borrowed, so the common path allocates
/// nothing.
pub(crate) fn terminal_safe_span(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|character| match character {
                '\n' | '\r' | '\t' => ' ',
                value if value.is_control() => '\u{fffd}',
                value => value,
            })
            .collect(),
    )
}

fn terminal_safe_markdown(text: &str) -> Cow<'_, str> {
    if text
        .chars()
        .all(|character| character == '\n' || !character.is_control())
    {
        return Cow::Borrowed(text);
    }
    let mut output = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\n' => output.push('\n'),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                output.push('\n');
            }
            '\t' => output.push(' '),
            value if value.is_control() => output.push('\u{fffd}'),
            value => output.push(value),
        }
    }
    Cow::Owned(output)
}

/// Wrap semantic spans without reconstructing Markdown delimiters. Words can
/// span multiple styles; long paths wrap by grapheme without losing characters.
pub(crate) fn wrap_styled(
    spans: &[Span<'static>],
    width: usize,
    hanging: usize,
) -> Vec<Line<'static>> {
    use unicode_segmentation::UnicodeSegmentation;
    let width = width.max(1);
    let hanging = hanging.min(width.saturating_sub(1));
    let mut rows = Vec::new();
    let mut row = Vec::<Span<'static>>::new();
    let mut columns = 0;
    let mut word = Vec::<(String, Style)>::new();
    let mut spaces = Vec::<(String, Style)>::new();
    fn append(row: &mut Vec<Span<'static>>, text: String, style: Style) {
        if let Some(last) = row.last_mut()
            && last.style == style
        {
            last.content.to_mut().push_str(&text);
        } else {
            row.push(Span::styled(text, style));
        }
    }
    let flush_word = |rows: &mut Vec<Line<'static>>,
                      row: &mut Vec<Span<'static>>,
                      columns: &mut usize,
                      word: &mut Vec<(String, Style)>,
                      spaces: &mut Vec<(String, Style)>| {
        if word.is_empty() {
            return;
        }
        let word_width: usize = word.iter().map(|(text, _)| text.width()).sum();
        let space_width: usize = spaces.iter().map(|(text, _)| text.width()).sum();
        if *columns > 0 && *columns + space_width + word_width > width {
            rows.push(Line::from(std::mem::take(row)));
            *columns = hanging;
            if hanging > 0 {
                append(row, " ".repeat(hanging), Style::default());
            }
            spaces.clear();
        }
        for (space, style) in spaces.drain(..) {
            if *columns + space.width() <= width {
                *columns += space.width();
                append(row, space, style);
            }
        }
        for (text, style) in word.drain(..) {
            for grapheme in text.graphemes(true) {
                let size = grapheme.width();
                if *columns + size > width && *columns > 0 {
                    rows.push(Line::from(std::mem::take(row)));
                    *columns = hanging;
                    if hanging > 0 {
                        append(row, " ".repeat(hanging), Style::default());
                    }
                }
                append(row, grapheme.to_owned(), style);
                *columns += size;
            }
        }
    };
    for span in spans {
        for grapheme in span.content.graphemes(true) {
            if grapheme.chars().all(char::is_whitespace) {
                flush_word(&mut rows, &mut row, &mut columns, &mut word, &mut spaces);
                spaces.push((grapheme.to_owned(), span.style));
            } else {
                word.push((grapheme.to_owned(), span.style));
            }
        }
    }
    flush_word(&mut rows, &mut row, &mut columns, &mut word, &mut spaces);
    if !row.is_empty() {
        rows.push(Line::from(row));
    }
    rows
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    #[test]
    fn tables_preserve_cells_alignment_and_code_without_flattening_rows() {
        let source = "| Item | Result |\n| :--- | ---: |\n| alpha | **ready** |\n| beta | `done` |";
        let lines = super::render_markdown(source, 80);
        let text = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("┌"));
        assert!(text.contains("│ alpha │  ready │"), "{text}");
        assert!(text.contains("│ beta  │   done │"), "{text}");
        assert!(!text.contains("---"));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content == "ready"
                && span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        }));
        let fenced = super::render_markdown(&format!("```text\n{source}\n```"), 80);
        assert!(
            !fenced
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains('┌'))
        );
    }

    #[test]
    fn narrow_tables_wrap_or_stack_without_losing_cells_or_height_bounds() {
        let source = "| Name | Outcome |\n| --- | --- |\n| alpha | a very long outcome with words |\n| beta | completed |";
        for width in [10, 18, 30, 80] {
            let lines = super::render_markdown(source, width);
            assert!(
                lines.iter().all(|line| line.width() <= width),
                "width {width}: {lines:?}"
            );
            assert!(lines.len() <= super::render_markdown_height_bound(source, width));
            let text = lines
                .iter()
                .flat_map(|line| &line.spans)
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(text.contains("alpha"), "width {width}: {text}");
            assert!(text.contains("beta"), "width {width}: {text}");
        }
    }
    use super::*;

    fn text_of(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect()
    }

    #[test]
    fn paragraphs_wrap_at_width() {
        let lines = render_markdown("one two three four five six seven", 10);
        let t = text_of(&lines);
        assert!(t.iter().all(|l| l.width() <= 10), "{t:?}");
        assert_eq!(t[0], "one two");
    }

    #[test]
    fn headings_are_bold_body_text_not_navigation_accent() {
        let lines = render_markdown("# Title", 40);
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "Title");
        assert_eq!(span.style.fg, Some(palette::TEXT));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn fenced_code_is_highlighted_with_indent() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md, 40);
        let t = text_of(&lines);
        assert!(t.iter().any(|l| l.contains("fn")), "{t:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.spans.len() >= 2 && l.spans[0].content == "  ")
        );
    }

    #[test]
    fn bullets_render_with_dot() {
        let lines = render_markdown("- alpha\n- beta", 40);
        let t = text_of(&lines);
        assert!(t.iter().any(|l| l.starts_with("• alpha")), "{t:?}");
        assert!(t.iter().any(|l| l.starts_with("• beta")), "{t:?}");
    }

    #[test]
    fn inline_code_is_tinted() {
        let lines = render_markdown("use `run_tool` here", 40);
        let found = lines
            .iter()
            .flatten()
            .find(|s| s.content == "run_tool")
            .unwrap();
        assert!(found.style.fg == Some(crate::palette::CODE));
    }

    #[test]
    fn ansi_and_osc_controls_never_enter_rendered_markdown_cells() {
        let lines = render_markdown(
            "synthetic \u{1b}[31mred\u{1b}[0m text\nsynthetic-link \u{1b}]8;;https://example.invalid/\u{1b}\\label\u{1b}]8;;\u{1b}\\",
            80,
        );
        assert!(lines.iter().flat_map(|line| &line.spans).all(|span| {
            span.content
                .chars()
                .all(|character| !character.is_control())
        }));
        let rendered = text_of(&lines).join("\n");
        assert!(rendered.contains("red"), "{rendered}");
        assert!(rendered.contains("label"), "{rendered}");
    }
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    fn plain(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn multiword_code_keeps_styles_spaces_and_adjacent_punctuation() {
        let lines = render_markdown("Use `readme recovery reader`, then `printf 'a b'`.", 80);
        assert_eq!(
            plain(&lines),
            "Use readme recovery reader, then printf 'a b'."
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "readme recovery reader"
                    && span.style.fg == Some(crate::palette::CODE))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "printf 'a b'"
                    && span.style.fg == Some(crate::palette::CODE))
        );
        assert_eq!(
            plain(&render_markdown("Run ``printf `ok` now``.", 80)),
            "Run printf `ok` now."
        );
    }
    #[test]
    fn nested_lists_and_long_paths_wrap_without_losing_content() {
        let path = "/workspace/project/a_very_long_filename.rs";
        let markdown = format!("- outer\n  - `{path}`");
        for width in [10, 16, 35] {
            let lines = render_markdown(&markdown, width);
            assert!(
                lines.iter().all(|line| line.width() <= width),
                "{}",
                plain(&lines)
            );
            assert!(
                lines.iter().any(|line| line.to_string().starts_with("  •")),
                "{}",
                plain(&lines)
            );
            let flattened: String = plain(&lines)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect();
            assert_eq!(flattened, format!("•outer•{path}"));
            assert!(lines.len() <= render_markdown_height_bound(&markdown, width));
        }
    }
}

/// Render with the actual selected theme and terminal color capability.
#[must_use]
pub fn render_markdown_with_styles(
    text: &str,
    width: usize,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let mut lines = render_markdown(text, width);
    for line in &mut lines {
        for span in &mut line.spans {
            if let Some(fg) = span.style.fg {
                span.style.fg = Some(if fg == palette::ACCENT {
                    styles.accent()
                } else if fg == palette::CODE {
                    styles.code()
                } else if fg == palette::TEXT {
                    styles.text()
                } else if fg == palette::DIM {
                    styles.dim()
                } else {
                    // Syntect themes contain literal foregrounds authored for
                    // one dark background. Treat them as syntax metadata, not
                    // drawable theme colours: every selected heycode theme owns
                    // the readable code foreground at every capability tier.
                    styles.code()
                });
            }
        }
    }
    lines
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod theme_tests {
    use super::*;
    #[test]
    fn selected_theme_applies_to_markdown_and_no_color_has_no_rgb() {
        for level in [
            heycode_ui::terminal::ColorLevel::None,
            heycode_ui::terminal::ColorLevel::Basic,
            heycode_ui::terminal::ColorLevel::Ansi256,
        ] {
            let theme = heycode_ui::theme::default_theme().unwrap();
            let styles = crate::terminal::Styles::new(&theme.resolve(level));
            let lines = render_markdown_with_styles(
                "# Heading\n\n`code`\n\n```rust\nfn main() {}\n```",
                60,
                styles,
            );
            for span in lines.iter().flat_map(|line| &line.spans) {
                assert!(!matches!(span.style.fg, Some(Color::Rgb(..))));
                assert!(!matches!(span.style.bg, Some(Color::Rgb(..))));
                if level == heycode_ui::terminal::ColorLevel::None {
                    assert!(span.style.fg.is_none() || span.style.fg == Some(Color::Reset));
                }
            }
            assert_eq!(lines[0].to_string(), "Heading");
        }
    }

    #[test]
    fn light_theme_owns_heading_inline_and_fenced_code_foregrounds() {
        use heycode_ui::terminal::ColorLevel;
        use heycode_ui::theme::{ThemeRole, builtin_themes};

        let theme = builtin_themes()
            .unwrap()
            .into_iter()
            .find(|theme| theme.id().as_str() == "heycode-light")
            .unwrap();
        let styles = crate::terminal::Styles::new(&theme.resolve(ColorLevel::TrueColor));
        let lines = render_markdown_with_styles(
            "# Heading\n\n`inline`\n\n```rust\nfn main() {}\n```",
            60,
            styles,
        );
        let heading = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "Heading")
            .unwrap();
        assert_eq!(heading.style.fg, Some(styles.text()));
        assert!(heading.style.add_modifier.contains(Modifier::BOLD));
        let code_spans = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.content.contains("inline") || span.content.contains("fn"))
            .collect::<Vec<_>>();
        assert!(!code_spans.is_empty());
        assert!(
            code_spans
                .iter()
                .all(|span| span.style.fg == Some(styles.code()))
        );
        assert_ne!(styles.text(), styles.accent());
        assert_eq!(
            theme.resolve(ColorLevel::TrueColor).color(ThemeRole::Code),
            heycode_ui::theme::TerminalColor::Rgb(theme.rgb(ThemeRole::Code))
        );
    }
}
