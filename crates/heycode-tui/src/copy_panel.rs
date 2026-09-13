//! Source-grounded `/copy` chooser with exact-content clipboard/file actions.
//!
//! The panel owns no clipboard, settings, transcript, or workspace service.
//! It parses one completed assistant answer, presents bounded previews, and
//! returns a deliberate action containing source-compatible selected bytes.
//! Full Markdown answers normalize table layout like Claude's copy surface;
//! fenced-code selections and fenced regions remain byte-for-byte intact.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use pulldown_cmark::{CodeBlockKind, Event as MarkdownEvent, Tag, TagEnd};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use thiserror::Error;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::markdown::terminal_safe_span;
use crate::terminal::Styles;

const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;
const MAX_CODE_BLOCKS: usize = 1_024;
const MAX_PREVIEW_COLUMNS: usize = 60;

/// Exact content selected for one explicit copy or file-write action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopySelection {
    text: String,
    filename: String,
}

impl CopySelection {
    /// Construct the normalized full-answer selection used by direct-copy and
    /// preference-bypass paths. Only Markdown table layout outside fenced code
    /// changes; all other source bytes remain intact.
    pub(crate) fn full_response(answer: String) -> Result<Self, CopyPanelError> {
        validate_size(answer.len())?;
        let answer = normalize_markdown_tables(&answer);
        validate_size(answer.len())?;
        Ok(Self {
            text: answer,
            filename: "response.md".to_owned(),
        })
    }

    /// Unmodified selected answer or fenced-code body.
    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Safe single-component recovery filename derived by the panel.
    #[must_use]
    pub(crate) fn filename(&self) -> &str {
        &self.filename
    }

    /// Write the exact selection into a new private temporary directory.
    ///
    /// The directory is removed on every failure and kept only after the new
    /// file has been fully written and synced. No ambient or workspace path is
    /// accepted, and an existing file is never opened or overwritten.
    pub(crate) fn write_recovery(&self) -> Result<PathBuf, CopyPanelError> {
        validate_size(self.text.len())?;
        validate_filename(&self.filename)?;
        let directory = tempfile::Builder::new().prefix("heycode-copy-").tempdir()?;
        #[cfg(unix)]
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = directory.path().join(&self.filename);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path)?;
        file.write_all(self.text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        let root = directory.keep();
        Ok(root.join(&self.filename))
    }
}

/// One side-effect-free action returned to the owning application loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CopyAction {
    /// Keep the chooser open.
    None,
    /// Close without copying, writing, or changing preferences.
    Cancel,
    /// Copy the selected exact content. `always_full` is true only for the
    /// explicit persistent-preference row.
    Copy {
        selection: CopySelection,
        always_full: bool,
    },
    /// Write the selected exact content without changing copy preferences.
    Write { selection: CopySelection },
}

/// Construction or recovery-write refusal.
#[derive(Debug, Error)]
pub(crate) enum CopyPanelError {
    /// The selected answer cannot be held or written without truncation.
    #[error("copy content is {actual} bytes; the limit is {limit} bytes")]
    TooLarge { actual: usize, limit: usize },
    /// The derived output name is not one safe path component.
    #[error("copy output filename is invalid")]
    InvalidFilename,
    /// A private output directory or file could not be created or written.
    #[error("copy output failed: {0}")]
    Io(#[from] std::io::Error),
    /// A hostile answer produced an unreasonable number of code choices.
    #[error("copy answer contains more than {limit} fenced code blocks")]
    TooManyCodeBlocks { limit: usize },
}

#[derive(Debug, Clone)]
struct CopyRow {
    selection: CopySelection,
    label: String,
    description: String,
}

/// Local chooser opened only for an answer containing fenced code blocks.
#[derive(Debug)]
pub(crate) struct CopyPanel {
    full: CopySelection,
    code_rows: Vec<CopyRow>,
    selected: usize,
    offset: usize,
    area: Rect,
    row_hits: Vec<(Rect, usize)>,
}

impl CopyPanel {
    /// Parse one completed answer. `Ok(None)` means that it has no fenced code
    /// and should take the application's existing direct full-answer path.
    /// Oversized input is an explicit error, never a truncated or direct copy.
    pub(crate) fn new(answer: String) -> Result<Option<Self>, CopyPanelError> {
        let full = CopySelection::full_response(answer)?;
        let code_rows = extract_code_rows(&full.text)?;
        if code_rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            full,
            code_rows,
            selected: 0,
            offset: 0,
            area: Rect::default(),
            row_hits: Vec::new(),
        }))
    }

    /// Compact target height; long code-block inventories scroll in place.
    #[must_use]
    pub(crate) fn desired_height(&self) -> u16 {
        u16::try_from(self.option_count())
            .unwrap_or(u16::MAX)
            .saturating_add(5)
            .clamp(8, 30)
    }

    /// Flat projection of the same order, selection, metadata, and actions.
    #[must_use]
    pub(crate) fn plain_lines(&self) -> Vec<String> {
        let mut lines = vec!["Select content to copy:".to_owned()];
        for index in 0..self.option_count() {
            let (label, description) = self.row_text(index);
            let prefix = if index == self.selected {
                "selected: "
            } else {
                ""
            };
            let number = index + 1;
            if description.is_empty() {
                lines.push(format!("{prefix}{number}. {label}"));
            } else {
                lines.push(format!("{prefix}{number}. {label} — {description}"));
            }
        }
        lines.push(
            "keys: Up/Down, Home/End, PageUp/PageDown, or mouse wheel moves; click selects; Enter copies; W writes to a private file; Escape cancels."
                .to_owned(),
        );
        lines
    }

    /// Consume one modal event without performing a side effect. Paste is
    /// deliberately swallowed so it cannot reach the composer or activate a
    /// selected row.
    #[must_use]
    pub(crate) fn handle_event(&mut self, event: &Event) -> CopyAction {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return CopyAction::Cancel,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return CopyAction::Cancel;
                }
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-10),
                KeyCode::PageDown => self.move_selection(10),
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = self.option_count().saturating_sub(1),
                KeyCode::Enter => {
                    return CopyAction::Copy {
                        selection: self.selected_value(),
                        always_full: self.selected == self.option_count().saturating_sub(1),
                    };
                }
                KeyCode::Char('w')
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL
                            | KeyModifiers::ALT
                            | KeyModifiers::SUPER
                            | KeyModifiers::SHIFT,
                    ) =>
                {
                    return CopyAction::Write {
                        selection: self.selected_value(),
                    };
                }
                _ => {}
            },
            Event::Mouse(mouse) if self.area.contains(Position::new(mouse.column, mouse.row)) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.move_selection(-3),
                    MouseEventKind::ScrollDown => self.move_selection(3),
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some((_, index)) = self
                            .row_hits
                            .iter()
                            .find(|(area, _)| area.contains(Position::new(mouse.column, mouse.row)))
                        {
                            self.selected = *index;
                        }
                    }
                    _ => {}
                }
            }
            Event::Paste(_) => {}
            _ => {}
        }
        CopyAction::None
    }

    /// Render the source-ordered chooser inside the application-owned modal
    /// slice. Long inventories scroll; narrow rows retain action and selection
    /// text without exposing raw code control characters.
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, styles: Styles) {
        self.area = area;
        self.row_hits.clear();
        if area.width < 2 || area.height < 6 {
            return;
        }

        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.dim())),
            area,
        );
        frame.render_widget(
            Paragraph::new("  Select content to copy:").style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );

        let body = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(3),
            area.width.saturating_sub(4),
            area.height.saturating_sub(5),
        );
        self.keep_selected_visible(usize::from(body.height));
        let normal = Style::default().fg(styles.text());
        let selected = Style::default()
            .fg(styles.accent())
            .add_modifier(Modifier::BOLD);
        let lines = (0..self.option_count())
            .skip(self.offset)
            .take(usize::from(body.height))
            .enumerate()
            .map(|(visible_index, option_index)| {
                let row_area = Rect::new(
                    body.x,
                    body.y
                        .saturating_add(u16::try_from(visible_index).unwrap_or(u16::MAX)),
                    body.width,
                    1,
                );
                self.row_hits.push((row_area, option_index));
                self.render_row(option_index, usize::from(body.width), normal, selected)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), body);

        let footer = if area.width < 58 {
            "  Enter copy · W write · Esc cancel"
        } else {
            "  enter to copy · w to write to file · esc to cancel"
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
        );
    }

    fn option_count(&self) -> usize {
        self.code_rows.len().saturating_add(2)
    }

    fn selected_value(&self) -> CopySelection {
        if self.selected == 0 || self.selected == self.option_count().saturating_sub(1) {
            self.full.clone()
        } else {
            self.code_rows[self.selected - 1].selection.clone()
        }
    }

    fn row_text(&self, index: usize) -> (&str, String) {
        if index == 0 {
            return ("Full response", count_description(&self.full.text));
        }
        if index == self.option_count().saturating_sub(1) {
            return (
                "Always copy full response",
                "Skip this picker in the future (revert via /config)".to_owned(),
            );
        }
        let row = &self.code_rows[index - 1];
        (&row.label, row.description.clone())
    }

    fn render_row(
        &self,
        index: usize,
        width: usize,
        normal: Style,
        selected_style: Style,
    ) -> Line<'static> {
        let (label, description) = self.row_text(index);
        let prefix = format!(
            "{}{}. ",
            if index == self.selected { "› " } else { "  " },
            index + 1
        );
        let available = width.saturating_sub(prefix.width());
        let content = if description.is_empty() {
            truncate_display(label, available)
        } else if width >= 64 {
            let label_width = available.saturating_sub(24).clamp(12, 32);
            let label = pad_display(&truncate_display(label, label_width), label_width);
            let detail_width = available.saturating_sub(label_width.saturating_add(2));
            format!("{label}  {}", truncate_display(&description, detail_width))
        } else {
            truncate_display(&format!("{label} · {description}"), available)
        };
        let style = if index == self.selected {
            selected_style
        } else {
            normal
        };
        Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(content, style),
        ])
    }

    fn move_selection(&mut self, delta: isize) {
        let last = self.option_count().saturating_sub(1);
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta.unsigned_abs()).min(last)
        };
    }

    fn keep_selected_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(visible_rows) {
            self.offset = self.selected + 1 - visible_rows;
        }
        self.offset = self
            .offset
            .min(self.option_count().saturating_sub(visible_rows));
    }
}

fn extract_code_rows(answer: &str) -> Result<Vec<CopyRow>, CopyPanelError> {
    let mut rows = Vec::new();
    let mut parser = pulldown_cmark::Parser::new(answer);
    while let Some(event) = parser.next() {
        let MarkdownEvent::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) = event else {
            continue;
        };
        if rows.len() >= MAX_CODE_BLOCKS {
            return Err(CopyPanelError::TooManyCodeBlocks {
                limit: MAX_CODE_BLOCKS,
            });
        }
        let language = info
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let mut text = String::new();
        for event in parser.by_ref() {
            match event {
                MarkdownEvent::End(TagEnd::CodeBlock) => break,
                MarkdownEvent::Text(value) | MarkdownEvent::Code(value) => text.push_str(&value),
                MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak => text.push('\n'),
                _ => {}
            }
        }
        // pulldown-cmark includes the delimiter-separating newline in fenced
        // code text. It is Markdown structure rather than selected code; drop
        // exactly that one newline while retaining any intentional blank line.
        if text.ends_with('\n') {
            text.pop();
        }
        validate_size(text.len())?;
        let preview = preview_for_code(&text);
        let line_count = logical_line_count(&text);
        let description = [
            language
                .as_deref()
                .map(terminal_safe_span)
                .map(std::borrow::Cow::into_owned),
            (line_count > 1).then(|| format!("{line_count} lines")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        rows.push(CopyRow {
            selection: CopySelection {
                text,
                filename: code_filename(language.as_deref()),
            },
            label: preview,
            description,
        });
    }
    Ok(rows)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableAlignment {
    None,
    Left,
    Right,
    Center,
}

/// Normalize pipe-table spacing without reparsing or serializing the rest of
/// the answer. A local scanner is deliberately narrower than a Markdown
/// renderer: source prose, inline markup, line endings, and fenced code stay
/// untouched while confirmed header/delimiter table blocks are reformatted.
fn normalize_markdown_tables(answer: &str) -> String {
    let lines = answer.split_inclusive('\n').collect::<Vec<_>>();
    let mut output = String::with_capacity(answer.len());
    let mut index = 0;
    let mut fence = None;

    while index < lines.len() {
        let (body, _) = split_line_ending(lines[index]);
        if let Some((marker, minimum)) = fence {
            output.push_str(lines[index]);
            if is_fence_close(body, marker, minimum) {
                fence = None;
            }
            index += 1;
            continue;
        }
        if let Some(marker) = fence_open(body) {
            fence = Some(marker);
            output.push_str(lines[index]);
            index += 1;
            continue;
        }

        let Some(header) = parse_table_row(body) else {
            output.push_str(lines[index]);
            index += 1;
            continue;
        };
        let Some(delimiter_line) = lines.get(index + 1) else {
            output.push_str(lines[index]);
            index += 1;
            continue;
        };
        let (delimiter_body, _) = split_line_ending(delimiter_line);
        let Some(alignments) = parse_table_delimiter(delimiter_body) else {
            output.push_str(lines[index]);
            index += 1;
            continue;
        };
        if alignments.len() != header.len() {
            output.push_str(lines[index]);
            index += 1;
            continue;
        }

        let mut rows = vec![header];
        let mut end = index + 2;
        while let Some(line) = lines.get(end) {
            let (body, _) = split_line_ending(line);
            let Some(row) = parse_table_row(body) else {
                break;
            };
            if row.len() > alignments.len() {
                break;
            }
            rows.push(row);
            end += 1;
        }
        let widths = (0..alignments.len())
            .map(|column| {
                let content = rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| cell.width())
                    .max()
                    .unwrap_or(0);
                content.max(match alignments[column] {
                    TableAlignment::None => 3,
                    TableAlignment::Left | TableAlignment::Right => 4,
                    TableAlignment::Center => 5,
                })
            })
            .collect::<Vec<_>>();

        output.push_str(&format_table_row(&rows[0], &widths));
        output.push_str(split_line_ending(lines[index]).1);
        output.push_str(&format_table_delimiter(&alignments, &widths));
        output.push_str(split_line_ending(lines[index + 1]).1);
        for (offset, row) in rows.iter().skip(1).enumerate() {
            output.push_str(&format_table_row(row, &widths));
            output.push_str(split_line_ending(lines[index + 2 + offset]).1);
        }
        index = end;
    }
    output
}

fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

fn fence_open(line: &str) -> Option<(char, usize)> {
    let trimmed = trim_markdown_indent(line)?;
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let run = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    (run >= 3).then_some((marker, run))
}

fn is_fence_close(line: &str, marker: char, minimum: usize) -> bool {
    let Some(trimmed) = trim_markdown_indent(line) else {
        return false;
    };
    let run = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    run >= minimum && trimmed.chars().skip(run).all(char::is_whitespace)
}

fn trim_markdown_indent(line: &str) -> Option<&str> {
    let spaces = line
        .chars()
        .take_while(|character| *character == ' ')
        .count();
    (spaces <= 3 && !line.starts_with('\t')).then(|| &line[spaces..])
}

fn parse_table_row(line: &str) -> Option<Vec<String>> {
    trim_markdown_indent(line)?;
    let source = line.trim();
    if source.is_empty() {
        return None;
    }
    let characters = source.chars().collect::<Vec<_>>();
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut index = 0;
    let mut code_run = None;
    let mut backslashes = 0usize;
    let mut separators = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character == '\\' {
            cell.push(character);
            backslashes += 1;
            index += 1;
            continue;
        }
        let escaped = backslashes % 2 == 1;
        backslashes = 0;
        if character == '`' && !escaped {
            let run = characters[index..]
                .iter()
                .take_while(|character| **character == '`')
                .count();
            match code_run {
                Some(open) if open == run => code_run = None,
                None => code_run = Some(run),
                _ => {}
            }
            cell.extend(std::iter::repeat_n('`', run));
            index += run;
            continue;
        }
        if character == '|' && code_run.is_none() && !escaped {
            cells.push(cell.trim().to_owned());
            cell.clear();
            separators += 1;
        } else {
            cell.push(character);
        }
        index += 1;
    }
    cells.push(cell.trim().to_owned());
    if source.starts_with('|') && cells.first().is_some_and(String::is_empty) {
        cells.remove(0);
    }
    if source.ends_with('|') && cells.last().is_some_and(String::is_empty) {
        cells.pop();
    }
    (separators > 0 && cells.len() >= 2).then_some(cells)
}

fn parse_table_delimiter(line: &str) -> Option<Vec<TableAlignment>> {
    parse_table_row(line)?
        .into_iter()
        .map(|cell| {
            let cell = cell.trim();
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            let dashes = cell
                .strip_prefix(':')
                .unwrap_or(cell)
                .strip_suffix(':')
                .unwrap_or_else(|| cell.strip_prefix(':').unwrap_or(cell));
            (!dashes.is_empty() && dashes.chars().all(|character| character == '-')).then_some(
                match (left, right) {
                    (false, false) => TableAlignment::None,
                    (true, false) => TableAlignment::Left,
                    (false, true) => TableAlignment::Right,
                    (true, true) => TableAlignment::Center,
                },
            )
        })
        .collect()
}

fn format_table_row(cells: &[String], widths: &[usize]) -> String {
    let mut output = String::from("|");
    for (column, width) in widths.iter().copied().enumerate() {
        let cell = cells.get(column).map_or("", String::as_str);
        output.push(' ');
        output.push_str(cell);
        output.push_str(&" ".repeat(width.saturating_sub(cell.width())));
        output.push_str(" |");
    }
    output
}

fn format_table_delimiter(alignments: &[TableAlignment], widths: &[usize]) -> String {
    let cells = alignments
        .iter()
        .zip(widths)
        .map(|(alignment, width)| match alignment {
            TableAlignment::None => "-".repeat(*width),
            TableAlignment::Left => format!(":{}", "-".repeat(width.saturating_sub(1))),
            TableAlignment::Right => format!("{}:", "-".repeat(width.saturating_sub(1))),
            TableAlignment::Center => format!(":{}:", "-".repeat(width.saturating_sub(2))),
        })
        .collect::<Vec<_>>();
    format_table_row(&cells, widths)
}

fn preview_for_code(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("(empty code block)")
        .trim();
    truncate_display(terminal_safe_span(first).as_ref(), MAX_PREVIEW_COLUMNS)
}

fn count_description(text: &str) -> String {
    format!(
        "{} chars, {} lines",
        text.chars().count(),
        logical_line_count(text)
    )
}

fn logical_line_count(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        .saturating_add(1)
}

fn code_filename(language: Option<&str>) -> String {
    let sanitized = language
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if sanitized.is_empty() || sanitized == "plaintext" {
        "copy.txt".to_owned()
    } else {
        format!("copy.{sanitized}")
    }
}

fn validate_size(actual: usize) -> Result<(), CopyPanelError> {
    if actual > MAX_COPY_BYTES {
        Err(CopyPanelError::TooLarge {
            actual,
            limit: MAX_COPY_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_filename(filename: &str) -> Result<(), CopyPanelError> {
    let mut components = Path::new(filename).components();
    if filename.is_empty()
        || matches!(filename, "." | "..")
        || !matches!(components.next(), Some(Component::Normal(value)) if value == OsStr::new(filename))
        || components.next().is_some()
    {
        Err(CopyPanelError::InvalidFilename)
    } else {
        Ok(())
    }
}

fn truncate_display(text: &str, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    if text.width() <= maximum {
        return text.to_owned();
    }
    let target = maximum.saturating_sub(1);
    let mut width: usize = 0;
    let mut output = String::new();
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if width.saturating_add(character_width) > target {
            break;
        }
        output.push(character);
        width = width.saturating_add(character_width);
    }
    output.push('…');
    output
}

fn pad_display(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crossterm::event::{KeyEvent, MouseEvent};
    use heycode_ui::terminal::ColorLevel;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    fn fixture() -> CopyPanel {
        CopyPanel::new(
            "Prose before.\n\n```python\nprint(\"alpha\")\nprint(\"beta\")\n```\n\n```json\n{\"ok\": true}\n```"
                .to_owned(),
        )
        .unwrap()
        .unwrap()
    }

    fn key(panel: &mut CopyPanel, code: KeyCode) -> CopyAction {
        panel.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn fenced_bodies_remain_exact_and_plain_answers_skip_the_panel() {
        let plain = "plain answer\nwith exact spacing  ".to_owned();
        assert!(CopyPanel::new(plain.clone()).unwrap().is_none());
        let direct = CopySelection::full_response(plain.clone()).unwrap();
        assert_eq!(direct.text(), plain);
        assert_eq!(direct.filename(), "response.md");
        let panel = fixture();
        assert_eq!(panel.code_rows.len(), 2);
        assert_eq!(
            panel.code_rows[0].selection.text(),
            "print(\"alpha\")\nprint(\"beta\")"
        );
        assert_eq!(panel.code_rows[0].selection.filename(), "copy.python");
        assert_eq!(panel.code_rows[0].label, "print(\"alpha\")");
        assert_eq!(panel.code_rows[0].description, "python, 2 lines");
        assert_eq!(panel.code_rows[1].selection.text(), "{\"ok\": true}");
        assert_eq!(panel.code_rows[1].selection.filename(), "copy.json");
        assert_eq!(panel.full.filename(), "response.md");
        assert!(panel.full.text().contains("```python"));
    }

    #[test]
    fn full_copy_and_recovery_match_the_verified_table_normalization() {
        let raw = concat!(
            "Synthetic markdown table normalization.\n\n",
            "| Item | Result |\n",
            "|:--|--:|\n",
            "| a | b |\n\n",
            "```python\n",
            "print(\"alpha\")\n",
            "```\n\n",
            "```json\n",
            "{\"ok\": true}\n",
            "```",
        );
        let expected = concat!(
            "Synthetic markdown table normalization.\n\n",
            "| Item | Result |\n",
            "| :--- | -----: |\n",
            "| a    | b      |\n\n",
            "```python\n",
            "print(\"alpha\")\n",
            "```\n\n",
            "```json\n",
            "{\"ok\": true}\n",
            "```",
        );
        let selection = CopySelection::full_response(raw.to_owned()).unwrap();
        assert_eq!(selection.text(), expected);

        let panel = CopyPanel::new(raw.to_owned()).unwrap().unwrap();
        assert_eq!(panel.full.text(), expected);
        assert_eq!(panel.code_rows[0].selection.text(), "print(\"alpha\")");
        assert_eq!(panel.code_rows[1].selection.text(), "{\"ok\": true}");

        let path = selection.write_recovery().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let root = path.parent().unwrap().to_path_buf();
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn table_like_text_in_fences_and_non_table_source_stay_exact() {
        let source = concat!(
            "before\r\n",
            "```text\r\n",
            "| Item | Result |\r\n",
            "|:--|--:|\r\n",
            "| a | b |\r\n",
            "```\r\n",
            "after `a | b`\r\n",
        );
        assert_eq!(
            CopySelection::full_response(source.to_owned())
                .unwrap()
                .text(),
            source
        );
    }

    #[test]
    fn keyboard_actions_preserve_selection_and_always_is_explicit() {
        let mut panel = fixture();
        let full = panel.full.text().to_owned();
        assert_eq!(
            key(&mut panel, KeyCode::Enter),
            CopyAction::Copy {
                selection: panel.full.clone(),
                always_full: false,
            }
        );
        assert_eq!(key(&mut panel, KeyCode::Down), CopyAction::None);
        let CopyAction::Copy {
            selection,
            always_full,
        } = key(&mut panel, KeyCode::Enter)
        else {
            panic!("expected copy action");
        };
        assert_eq!(selection.text(), "print(\"alpha\")\nprint(\"beta\")");
        assert!(!always_full);
        assert_eq!(key(&mut panel, KeyCode::End), CopyAction::None);
        let CopyAction::Copy {
            selection,
            always_full,
        } = key(&mut panel, KeyCode::Enter)
        else {
            panic!("expected persistent full copy");
        };
        assert_eq!(selection.text(), full);
        assert!(always_full);
        let CopyAction::Write { selection } = key(&mut panel, KeyCode::Char('w')) else {
            panic!("expected full write without preference change");
        };
        assert_eq!(selection.text(), panel.full.text());
        assert_eq!(key(&mut panel, KeyCode::Esc), CopyAction::Cancel);
    }

    #[test]
    fn paste_is_consumed_and_mouse_selects_without_executing() {
        let mut panel = fixture();
        assert_eq!(
            panel.handle_event(&Event::Paste("\n/run something".to_owned())),
            CopyAction::None
        );
        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), Styles::default()))
            .unwrap();
        let python = panel.row_hits[1].0;
        assert_eq!(
            panel.handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: python.x,
                row: python.y,
                modifiers: KeyModifiers::NONE,
            })),
            CopyAction::None
        );
        assert_eq!(panel.selected, 1);
        assert_eq!(
            panel.handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: python.x,
                row: python.y,
                modifiers: KeyModifiers::NONE,
            })),
            CopyAction::None
        );
        assert_eq!(panel.selected, panel.option_count().saturating_sub(1));
        assert_eq!(key(&mut panel, KeyCode::Home), CopyAction::None);
        assert_eq!(key(&mut panel, KeyCode::Down), CopyAction::None);
        let CopyAction::Copy { selection, .. } = key(&mut panel, KeyCode::Enter) else {
            panic!("selected mouse row should copy only after Enter");
        };
        assert_eq!(selection.filename(), "copy.python");
    }

    #[test]
    fn narrow_and_colorless_render_keep_selection_and_actions_visible() {
        let mut panel = fixture();
        key(&mut panel, KeyCode::Down);
        let theme = heycode_ui::theme::default_theme().unwrap();
        let styles = Styles::new(&theme.resolve(ColorLevel::None));
        let mut terminal = Terminal::new(TestBackend::new(45, 10)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), styles))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Select content to copy:"), "{text}");
        assert!(text.contains("› 2. print"), "{text}");
        assert!(text.contains("Enter copy · W write · Esc cancel"), "{text}");
        assert!(
            buffer
                .content()
                .iter()
                .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
        );
        assert!(panel.plain_lines().join("\n").contains("selected: 2."));
    }

    #[test]
    fn recovery_write_is_private_exact_and_never_overwrites() {
        let selection = CopySelection {
            text: "{\"ok\": true}".to_owned(),
            filename: "copy.json".to_owned(),
        };
        let path = selection.write_recovery().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), selection.text().as_bytes());
        assert_eq!(path.file_name(), Some(OsStr::new("copy.json")));
        assert!(path.parent().unwrap().starts_with(std::env::temp_dir()));
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let root = path.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversize_and_invalid_output_names_are_refused_without_truncation() {
        let error = CopyPanel::new("x".repeat(MAX_COPY_BYTES + 1)).unwrap_err();
        assert!(matches!(error, CopyPanelError::TooLarge { .. }));
        assert!(matches!(
            CopySelection::full_response("x".repeat(MAX_COPY_BYTES + 1)),
            Err(CopyPanelError::TooLarge { .. })
        ));
        let selection = CopySelection {
            text: "safe".to_owned(),
            filename: "../unsafe".to_owned(),
        };
        assert!(matches!(
            selection.write_recovery(),
            Err(CopyPanelError::InvalidFilename)
        ));
    }

    #[test]
    fn hostile_fence_metadata_is_safe_and_code_inventory_is_bounded() {
        let answer = "```../PyThOn\u{1b}[31m\n\u{1b}[2Jprint('safe')\n```".to_owned();
        let panel = CopyPanel::new(answer).unwrap().unwrap();
        let row = &panel.code_rows[0];
        assert_eq!(row.selection.filename(), "copy.python31m");
        assert!(!row.label.contains('\u{1b}'));
        assert!(!row.description.contains('\u{1b}'));
        assert!(row.selection.text().starts_with('\u{1b}'));

        let many = "```text\n```\n".repeat(MAX_CODE_BLOCKS + 1);
        assert!(matches!(
            CopyPanel::new(many),
            Err(CopyPanelError::TooManyCodeBlocks {
                limit: MAX_CODE_BLOCKS
            })
        ));
    }
}
