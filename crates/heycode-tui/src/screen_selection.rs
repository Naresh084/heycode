//! Mouse selection of rendered cells, never hidden application text.

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, layout::Position, style::Modifier};
use unicode_width::UnicodeWidthStr;

pub(crate) enum SelectionAction {
    Copy(String),
    Click(Position),
}

#[derive(Clone, Copy)]
struct Range {
    start: Position,
    end: Position,
}

impl Range {
    fn ordered(self) -> (Position, Position) {
        if (self.start.y, self.start.x) <= (self.end.y, self.end.x) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    fn contains(self, position: Position) -> bool {
        let (start, end) = self.ordered();
        (position.y, position.x) >= (start.y, start.x) && (position.y, position.x) <= (end.y, end.x)
    }
}

#[derive(Default)]
pub(crate) struct ScreenSelection {
    frame: Option<Buffer>,
    frozen: Option<Buffer>,
    range: Option<Range>,
    dragging: bool,
    word_selected: bool,
    last_click: Option<(Position, Instant)>,
}

impl ScreenSelection {
    pub(crate) fn handle(&mut self, event: MouseEvent, now: Instant) -> Option<SelectionAction> {
        let frame = self.frame.as_ref()?;
        let area = frame.area;
        if area.is_empty() {
            return None;
        }
        let position = Position::new(
            event.column.clamp(area.x, area.right().saturating_sub(1)),
            event.row.clamp(area.y, area.bottom().saturating_sub(1)),
        );
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if area.contains(position) => {
                let double = self.last_click.is_some_and(|(previous, at)| {
                    previous == position
                        && now.saturating_duration_since(at) <= Duration::from_millis(350)
                });
                self.last_click = Some((position, now));
                self.frozen = self.frame.clone();
                self.dragging = true;
                self.word_selected = double;
                self.range = Some(if double {
                    word_range(frame, position)
                } else {
                    Range {
                        start: position,
                        end: position,
                    }
                });
                if double {
                    return self.copy();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                if let Some(range) = &mut self.range {
                    range.end = position;
                }
                self.word_selected = false;
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.dragging = false;
                if self.word_selected {
                    return None;
                }
                if let Some(range) = &mut self.range {
                    range.end = position;
                    if range.start != range.end {
                        return self.copy();
                    }
                }
                self.range = None;
                return Some(SelectionAction::Click(position));
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.clear(),
            _ => {}
        }
        None
    }

    fn copy(&self) -> Option<SelectionAction> {
        let text = selected_text(self.frozen.as_ref()?, self.range?);
        (!text.is_empty()).then_some(SelectionAction::Copy(text))
    }

    pub(crate) fn clear(&mut self) {
        self.range = None;
        self.frozen = None;
        self.dragging = false;
        self.word_selected = false;
    }

    pub(crate) fn paint(&mut self, buffer: &mut Buffer) {
        if self.dragging
            && let Some(frozen) = &self.frozen
        {
            if frozen.area == buffer.area {
                // Streaming continues in application state; selected cells stay
                // fixed until release so the copied text matches what was seen.
                *buffer = frozen.clone();
            } else {
                self.clear();
            }
        }
        self.frame = Some(buffer.clone());
        if let Some(range) = self.range {
            if !self.dragging
                && self.frozen.as_ref().is_some_and(|frozen| {
                    frozen.area != buffer.area
                        || selected_text(frozen, range) != selected_text(buffer, range)
                })
            {
                self.clear();
                return;
            }
            for y in buffer.area.y..buffer.area.bottom() {
                for x in buffer.area.x..buffer.area.right() {
                    if range.contains(Position::new(x, y)) {
                        buffer[(x, y)].set_style(
                            ratatui::style::Style::default().add_modifier(Modifier::REVERSED),
                        );
                    }
                }
            }
        }
    }
}

fn word_range(buffer: &Buffer, position: Position) -> Range {
    let mut start = position.x;
    let mut end = position.x;
    // Whitespace-delimited tokens retain paths, URLs and command punctuation.
    let whitespace = buffer[(position.x, position.y)].symbol().trim().is_empty();
    while start > buffer.area.x
        && buffer[(start - 1, position.y)].symbol().trim().is_empty() == whitespace
    {
        start -= 1;
    }
    while end + 1 < buffer.area.right()
        && buffer[(end + 1, position.y)].symbol().trim().is_empty() == whitespace
    {
        end += 1;
    }
    Range {
        start: Position::new(start, position.y),
        end: Position::new(end, position.y),
    }
}

fn selected_text(buffer: &Buffer, range: Range) -> String {
    let (start, end) = range.ordered();
    let mut rows = Vec::new();
    for y in start.y.max(buffer.area.y)..=end.y.min(buffer.area.bottom().saturating_sub(1)) {
        let left = if y == start.y { start.x } else { buffer.area.x };
        let right = if y == end.y {
            end.x
        } else {
            buffer.area.right().saturating_sub(1)
        };
        let mut text = String::new();
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let symbol = buffer[(x, y)].symbol();
            let width = u16::try_from(symbol.width().max(1)).unwrap_or(1);
            if x <= right && x.saturating_add(width) > left {
                text.push_str(symbol);
            }
            x = x.saturating_add(width);
        }
        rows.push(if right == buffer.area.right().saturating_sub(1) {
            text.trim_end().to_owned()
        } else {
            text
        });
    }
    rows.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{layout::Rect, style::Style};

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn fixture() -> ScreenSelection {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 24, 3));
        buffer.set_string(0, 0, "hello world", Style::default());
        buffer.set_string(0, 1, "漢🙂 text", Style::default());
        let mut selection = ScreenSelection::default();
        selection.paint(&mut buffer);
        selection
    }
    #[test]
    fn drag_copies_on_release_and_reverse_selection_matches() {
        for (from, to) in [(0, 4), (4, 0)] {
            let mut selection = fixture();
            let now = Instant::now();
            assert!(
                selection
                    .handle(mouse(MouseEventKind::Down(MouseButton::Left), from, 0), now)
                    .is_none()
            );
            assert!(
                selection
                    .handle(mouse(MouseEventKind::Drag(MouseButton::Left), to, 0), now)
                    .is_none()
            );
            let Some(SelectionAction::Copy(text)) =
                selection.handle(mouse(MouseEventKind::Up(MouseButton::Left), to, 0), now)
            else {
                panic!("missing copy")
            };
            assert_eq!(text, "hello");
        }
    }
    #[test]
    fn double_click_copies_word_and_single_click_does_not_copy() {
        let mut selection = fixture();
        let now = Instant::now();
        selection.handle(mouse(MouseEventKind::Down(MouseButton::Left), 7, 0), now);
        assert!(matches!(
            selection.handle(mouse(MouseEventKind::Up(MouseButton::Left), 7, 0), now),
            Some(SelectionAction::Click(_))
        ));
        let Some(SelectionAction::Copy(text)) = selection.handle(
            mouse(MouseEventKind::Down(MouseButton::Left), 7, 0),
            now + Duration::from_millis(100),
        ) else {
            panic!("missing double click")
        };
        assert_eq!(text, "world");
    }
    #[test]
    fn wide_graphemes_copy_once_and_selection_stays_fixed_during_streaming() {
        let mut selection = fixture();
        let now = Instant::now();
        selection.handle(mouse(MouseEventKind::Down(MouseButton::Left), 0, 1), now);
        let mut changed = Buffer::empty(Rect::new(0, 0, 24, 3));
        changed.set_string(0, 1, "new streaming content", Style::default());
        selection.paint(&mut changed);
        let Some(SelectionAction::Copy(text)) =
            selection.handle(mouse(MouseEventKind::Up(MouseButton::Left), 3, 1), now)
        else {
            panic!("missing wide copy")
        };
        assert_eq!(text, "漢🙂");
        assert_eq!(text.chars().count(), 2);
    }
}
