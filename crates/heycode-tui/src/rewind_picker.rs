//! Source-shaped prompt selection over native durable checkpoints. Selection
//! never mutates history; confirmed actions use the ordinary rewind dispatcher.

use std::cell::{Cell, RefCell};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use heycode_core::SessionId;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Clear, Paragraph},
};

use crate::terminal::Styles;

const MAX_CHECKPOINTS: usize = 50;
const MAX_PREVIEW_CHARS: usize = 360;
/// Separator, heading, blank, notice, blank and the cancel hint.
const EMPTY_HEIGHT: usize = 6;

/// Wall-clock milliseconds; a clock before the epoch reports zero rather than
/// panicking, and `compact_age` then reports an unknown age.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
}

/// Compact checkpoint age, matching the source's `(8s ago)` shape. A missing or
/// future durable timestamp is reported as unknown rather than as "0s ago".
#[must_use]
pub(crate) fn compact_age(now_ms: i64, at_ms: i64) -> String {
    let Some(seconds) = now_ms.checked_sub(at_ms).filter(|elapsed| *elapsed >= 0) else {
        return "unknown age".into();
    };
    let seconds = seconds / 1000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Checkpoint {
    turn: u64,
    preview: String,
    files: Option<usize>,
    at_ms: i64,
}

impl Checkpoint {
    fn code_label(&self) -> String {
        match self.files {
            Some(0) => "No code changes".into(),
            Some(count) => format!(
                "{count} native file{} can be restored",
                if count == 1 { "" } else { "s" }
            ),
            None => "⚠ No code restore".into(),
        }
    }
}

/// Bounded display snapshot tied to the originating session.
#[derive(Clone, PartialEq, Eq)]
pub struct RewindPickerRequest {
    session_id: SessionId,
    points: Vec<Checkpoint>,
}

impl RewindPickerRequest {
    /// Read prompt boundaries and native checkpoint ownership without mutation.
    #[must_use]
    pub fn from_agent(agent: &heycode_agent::Agent) -> Self {
        let session_id = agent
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .clone();
        let mut points = agent.rewind_points();
        points.drain(..points.len().saturating_sub(MAX_CHECKPOINTS));
        let files = agent.rewind_file_candidates(&points);
        Self::from_points(session_id, points.into_iter().zip(files))
    }

    /// Convert supplied boundaries without inventing native file eligibility.
    #[must_use]
    pub fn new(session_id: SessionId, points: Vec<heycode_agent::RewindPoint>) -> Self {
        Self::from_points(session_id, points.into_iter().map(|point| (point, None)))
    }

    fn from_points(
        session_id: SessionId,
        points: impl IntoIterator<Item = (heycode_agent::RewindPoint, Option<usize>)>,
    ) -> Self {
        let mut points = points.into_iter().collect::<Vec<_>>();
        points.drain(..points.len().saturating_sub(MAX_CHECKPOINTS));
        Self {
            session_id,
            points: points
                .into_iter()
                .map(|(point, files)| Checkpoint {
                    turn: point.turn,
                    at_ms: point.at_ms,
                    preview: point
                        .prompt
                        .chars()
                        .filter(|c| !c.is_control() || c.is_whitespace())
                        .take(MAX_PREVIEW_CHARS)
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    files,
                })
                .collect(),
        }
    }

    /// Session whose immutable prompt boundaries were displayed.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }
}

/// One explicit choice for the ordinary queued command dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindSelection {
    session_id: SessionId,
    turn: u64,
    files: bool,
}

impl RewindSelection {
    /// Refuse a picker that survived a switch to another conversation.
    #[must_use]
    pub fn belongs_to(&self, session_id: &SessionId) -> bool {
        self.session_id == *session_id
    }

    /// Closed command grammar, revalidated by the existing runtime owner.
    #[must_use]
    pub fn command(&self) -> String {
        format!(
            "/rewind {}{}",
            self.turn,
            if self.files { " files" } else { "" }
        )
    }
}

/// Result of one input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindPickerOutcome {
    /// Keep the picker open.
    None,
    /// Close without mutation.
    Cancelled,
    /// An explicit confirmation action was activated by Enter or a click.
    Submit(RewindSelection),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Conversation,
    ConversationAndFiles,
    SummarizeFromHere,
    SummarizeUpToHere,
    Cancel,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::Conversation => "Restore conversation",
            Self::ConversationAndFiles => "Restore code and conversation",
            Self::SummarizeFromHere => "Summarize from here",
            Self::SummarizeUpToHere => "Summarize up to here",
            Self::Cancel => "Never mind",
        }
    }

    /// heycode restores a durable turn checkpoint; it has no rewind-with-summary
    /// path. Those rows stay visible and numbered like the source's so the
    /// absence is explicit, but they can never be chosen.
    const fn available(self) -> bool {
        !matches!(self, Self::SummarizeFromHere | Self::SummarizeUpToHere)
    }

    fn row(self, index: usize, chosen: bool) -> String {
        format!(
            "{} {}. {}{}",
            if chosen { "❯" } else { " " },
            index + 1,
            self.label(),
            if self.available() {
                ""
            } else {
                " (unavailable in heycode)"
            }
        )
    }
}

/// Chronological prompt list followed by explicit restoration choices.
/// The current position is the initial, non-mutating selection.
pub struct RewindPicker {
    request: RewindPickerRequest,
    selected: usize,
    choice: Option<usize>,
    hit_rows: RefCell<Vec<(Rect, usize)>>,
    body: Cell<Rect>,
    page_size: Cell<usize>,
}

impl RewindPicker {
    /// Construct a side-effect-free selection surface.
    #[must_use]
    pub fn new(request: RewindPickerRequest) -> Self {
        let selected = request.points.len();
        Self {
            request,
            selected,
            choice: None,
            hit_rows: RefCell::new(Vec::new()),
            body: Cell::new(Rect::default()),
            page_size: Cell::new(1),
        }
    }

    /// Read the origin before opening a delayed picker request.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        self.request.session_id()
    }

    /// Space for the source-shaped heading, context and visible choices.
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        let height = if self.choice.is_some() {
            12 + self.modes().len()
        } else if self.request.points.is_empty() {
            EMPTY_HEIGHT
        } else {
            11 + self.request.points.len() * 3
        };
        u16::try_from(height.min(22)).unwrap_or(22)
    }

    fn modes(&self) -> Vec<Mode> {
        let mut modes = vec![Mode::Conversation];
        if self
            .request
            .points
            .get(self.selected)
            .is_some_and(|point| point.files.is_some_and(|files| files > 0))
        {
            modes.push(Mode::ConversationAndFiles);
        }
        modes.push(Mode::SummarizeFromHere);
        modes.push(Mode::SummarizeUpToHere);
        modes.push(Mode::Cancel);
        modes
    }

    fn first_available_choice(&self) -> usize {
        self.modes()
            .iter()
            .position(|mode| mode.available())
            .unwrap_or(0)
    }

    /// Complete bounded text for the screen-reader renderer.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec!["Rewind".into()];
        if let Some(choice) = self.choice {
            lines.push(
                "Confirm you want to restore to the point before you sent this message:".into(),
            );
            if let Some(point) = self.request.points.get(self.selected) {
                lines.push(format!("│ {}", point.preview));
                lines.push(format!("│ ({})", compact_age(now_ms(), point.at_ms)));
                lines.push("The conversation will be forked.".into());
                lines.push(
                    if self.modes().get(choice) == Some(&Mode::ConversationAndFiles) {
                        format!(
                            "{} Conflict checks run before restoration.",
                            point.code_label()
                        )
                    } else {
                        "The code will be unchanged.".into()
                    },
                );
            }
            lines.extend(
                self.modes()
                    .into_iter()
                    .enumerate()
                    .map(|(index, mode)| mode.row(index, index == choice)),
            );
        } else if self.request.points.is_empty() {
            lines.push("Nothing to rewind to yet.".into());
            lines.push("Esc to cancel".into());
            return lines;
        } else {
            lines.push("Restore the code and/or conversation to the point before…".into());
            for (index, point) in self.request.points.iter().enumerate() {
                lines.push(format!(
                    "{} {}",
                    if index == self.selected { "❯" } else { " " },
                    point.preview
                ));
                lines.push(point.code_label());
            }
            lines.push(format!(
                "{} (current)",
                if self.selected == self.request.points.len() {
                    "❯"
                } else {
                    " "
                }
            ));
        }
        lines.push("Enter to continue · Esc to cancel".into());
        lines
    }

    fn navigate(&mut self, down: bool, steps: usize) {
        if let Some(choice) = self.choice {
            let modes = self.modes();
            let count = modes.len();
            let mut next = choice;
            // Skip the rows heycode cannot honour; they stay visible but unusable.
            for _ in 0..count {
                next = if down {
                    (next + 1) % count
                } else {
                    (next + count - 1) % count
                };
                if modes[next].available() {
                    break;
                }
            }
            self.choice = Some(next);
        } else {
            self.selected = if down {
                self.selected
                    .saturating_add(steps)
                    .min(self.request.points.len())
            } else {
                self.selected.saturating_sub(steps)
            };
        }
        self.hit_rows.borrow_mut().clear();
    }

    fn activate(&mut self) -> RewindPickerOutcome {
        let Some(choice) = self.choice else {
            if self.selected == self.request.points.len() {
                return RewindPickerOutcome::Cancelled;
            }
            self.choice = Some(self.first_available_choice());
            self.hit_rows.borrow_mut().clear();
            return RewindPickerOutcome::None;
        };
        match self.modes().get(choice) {
            Some(Mode::Cancel) | None => RewindPickerOutcome::Cancelled,
            Some(mode) if !mode.available() => RewindPickerOutcome::None,
            Some(mode) => self.request.points.get(self.selected).map_or(
                RewindPickerOutcome::Cancelled,
                |point| {
                    RewindPickerOutcome::Submit(RewindSelection {
                        session_id: self.request.session_id.clone(),
                        turn: point.turn,
                        files: *mode == Mode::ConversationAndFiles,
                    })
                },
            ),
        }
    }

    /// Consume modal input. Paste and repeated Enter never activate restoration;
    /// clicks activate only a currently visible confirmation action.
    pub fn handle(&mut self, event: &Event) -> RewindPickerOutcome {
        match event {
            Event::Resize(_, _) => {
                self.hit_rows.borrow_mut().clear();
                self.body.set(Rect::default());
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Esc {
                    if self.choice.take().is_none() {
                        return RewindPickerOutcome::Cancelled;
                    }
                    self.hit_rows.borrow_mut().clear();
                    return RewindPickerOutcome::None;
                }
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    return RewindPickerOutcome::None;
                }
                match key.code {
                    KeyCode::Up => self.navigate(false, 1),
                    KeyCode::Down | KeyCode::Tab => self.navigate(true, 1),
                    KeyCode::PageUp => self.navigate(false, self.page_size.get()),
                    KeyCode::PageDown => self.navigate(true, self.page_size.get()),
                    KeyCode::Enter if key.kind == KeyEventKind::Press => return self.activate(),
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                let position = ratatui::layout::Position::new(mouse.column, mouse.row);
                if !self.body.get().contains(position) {
                    return RewindPickerOutcome::None;
                }
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) if self.choice.is_some() => {
                        let choice =
                            self.hit_rows.borrow().iter().find_map(|(area, index)| {
                                area.contains(position).then_some(*index)
                            });
                        if let Some(choice) = choice {
                            self.choice = Some(choice);
                            return self.activate();
                        }
                    }
                    MouseEventKind::ScrollDown => self.navigate(true, 1),
                    MouseEventKind::ScrollUp => self.navigate(false, 1),
                    _ => {}
                }
            }
            _ => {}
        }
        RewindPickerOutcome::None
    }

    /// Draw the same top separator, heading, prompt list and explicit choices
    /// used by the source interaction. Only visible action rows receive hits.
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, styles: &Styles) {
        self.hit_rows.borrow_mut().clear();
        self.body.set(area);
        if area.height == 0 || area.width == 0 {
            return;
        }
        frame.render_widget(Clear, area);
        let text_style = Style::default().fg(styles.text());
        let dim_style = Style::default().fg(styles.dim());
        let left = area.x.saturating_add(3.min(area.width.saturating_sub(1)));
        let width = area.right().saturating_sub(left);
        let line = |frame: &mut Frame<'_>, y: u16, text: String, style: Style| {
            if y < area.bottom() {
                frame.render_widget(
                    Paragraph::new(text).style(style),
                    Rect::new(left, y, width, 1),
                );
            }
        };
        // A sentence that overflows a narrow terminal reflows into the blank row
        // beneath it instead of being cut, as the source's does.
        let sentence = |frame: &mut Frame<'_>, y: u16, text: &str, style: Style| {
            if y < area.bottom() {
                let rows = 2.min(area.bottom() - y);
                frame.render_widget(
                    Paragraph::new(text.to_owned())
                        .style(style)
                        .wrap(ratatui::widgets::Wrap { trim: false }),
                    Rect::new(left, y, width, rows),
                );
            }
        };
        if area.height < 4 {
            line(frame, area.y, "Rewind · Esc to cancel".into(), text_style);
            return;
        }
        let accent_style = Style::default().fg(styles.accent());
        frame.render_widget(
            Paragraph::new("▔".repeat(usize::from(area.width))).style(accent_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let roomy = area.height
            >= if self.choice.is_some() {
                u16::try_from(12 + self.modes().len()).unwrap_or(u16::MAX)
            } else {
                11
            };
        line(
            frame,
            area.y + 1,
            "Rewind".into(),
            accent_style.add_modifier(Modifier::BOLD),
        );
        if self.choice.is_none() && self.request.points.is_empty() {
            line(
                frame,
                area.y + 3.min(area.height.saturating_sub(2)),
                "Nothing to rewind to yet.".into(),
                dim_style,
            );
            line(frame, area.bottom() - 1, "Esc to cancel".into(), dim_style);
            return;
        }
        let start = area.y
            + if roomy {
                if self.choice.is_some() { 11 } else { 7 }
            } else {
                2
            };
        let end = area.bottom().saturating_sub(2);
        if let Some(choice) = self.choice {
            if roomy {
                sentence(
                    frame,
                    area.y + 3,
                    "Confirm you want to restore to the point before you sent this message:",
                    text_style,
                );
                if let Some(point) = self.request.points.get(self.selected) {
                    line(
                        frame,
                        area.y + 5,
                        format!("│ {}", point.preview),
                        text_style,
                    );
                    line(
                        frame,
                        area.y + 6,
                        format!("│ ({})", compact_age(now_ms(), point.at_ms)),
                        dim_style,
                    );
                }
                line(
                    frame,
                    area.y + 8,
                    "The conversation will be forked.".into(),
                    text_style,
                );
                let notice = if self.modes().get(choice) == Some(&Mode::ConversationAndFiles) {
                    "Confirmed native files will be restored after conflict checks."
                } else {
                    "The code will be unchanged."
                };
                line(frame, area.y + 9, notice.into(), text_style);
            }
            let modes = self.modes();
            // Rows may occupy every line up to `end` inclusive; the line below
            // it stays blank and the hint owns the last one.
            let capacity = (usize::from(end.saturating_sub(start)) + 1).max(1);
            let skip = choice.saturating_sub(capacity - 1);
            for (offset, (index, mode)) in modes
                .into_iter()
                .enumerate()
                .skip(skip)
                .take(capacity)
                .enumerate()
            {
                let y = start + u16::try_from(offset).unwrap_or(0);
                line(
                    frame,
                    y,
                    mode.row(index, index == choice),
                    if !mode.available() {
                        dim_style
                    } else if index == choice {
                        Style::default().fg(styles.accent())
                    } else {
                        text_style
                    },
                );
                self.hit_rows
                    .borrow_mut()
                    .push((Rect::new(left, y, width, 1), index));
            }
        } else {
            if roomy {
                sentence(
                    frame,
                    area.y + 3,
                    "Restore the code and/or conversation to the point before…",
                    text_style,
                );
            }
            // Roomy rows carry a blank separator, as the source's list does.
            let row_height: u16 = if roomy {
                3
            } else if end.saturating_sub(start) >= 4 {
                2
            } else {
                1
            };
            let capacity =
                ((usize::from(end.saturating_sub(start)) + 1) / usize::from(row_height)).max(1);
            self.page_size.set(capacity);
            let first = self.selected.saturating_sub(capacity - 1);
            if roomy && first > 0 {
                line(
                    frame,
                    area.y + 5,
                    format!(" ↑ {first} more above"),
                    dim_style,
                );
            }
            for (offset, index) in (first..=self.request.points.len())
                .take(capacity)
                .enumerate()
            {
                let y = start + u16::try_from(offset).unwrap_or(0) * row_height;
                let selected = index == self.selected;
                let marker = if selected { "❯" } else { " " };
                let preview = self
                    .request
                    .points
                    .get(index)
                    .map_or("(current)", |point| point.preview.as_str());
                line(
                    frame,
                    y,
                    format!("{marker} {preview}"),
                    if selected {
                        Style::default().fg(styles.accent())
                    } else {
                        text_style
                    },
                );
                if row_height >= 2
                    && let Some(point) = self.request.points.get(index)
                {
                    // The ⚠ glyph, not a colour, marks a checkpoint whose code
                    // cannot be restored; the source keeps both labels dim.
                    line(frame, y + 1, format!("  {}", point.code_label()), dim_style);
                }
            }
        }
        line(
            frame,
            area.bottom() - 1,
            "Enter to continue · Esc to cancel".into(),
            dim_style,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crossterm::event::{KeyEvent, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn picker(count: u64, files: Option<usize>) -> RewindPicker {
        RewindPicker::new(RewindPickerRequest::from_points(
            SessionId::from_raw("source-session"),
            (0..count).map(|turn| {
                (
                    heycode_agent::RewindPoint {
                        turn,
                        event_count: turn * 3,
                        prompt: format!("Prompt {turn}\u{1b}\n{}", "long ".repeat(100)),
                        at_ms: now_ms() - 8_000,
                    },
                    files,
                )
            }),
        ))
    }

    fn rendered(picker: &RewindPicker, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area(), &Styles::default()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn empty_picker_states_that_there_is_nothing_to_rewind_to() {
        let picker = picker(0, None);
        assert_eq!(picker.desired_height(), 6);
        let rows = rendered(&picker, 80, 6);
        assert!(rows[0].starts_with('▔'), "{rows:#?}");
        assert_eq!(rows[1], "   Rewind");
        assert_eq!(rows[2], "");
        assert_eq!(rows[3], "   Nothing to rewind to yet.");
        assert_eq!(rows[5], "   Esc to cancel");
        assert!(
            !rows.iter().any(|row| row.contains("(current)")),
            "an empty conversation offers no checkpoint row: {rows:#?}"
        );
        assert_eq!(
            picker.accessible_lines(),
            vec![
                "Rewind".to_owned(),
                "Nothing to rewind to yet.".to_owned(),
                "Esc to cancel".to_owned()
            ]
        );
    }

    #[test]
    fn populated_list_orders_checkpoints_oldest_first_with_current_last() {
        let picker = picker(2, Some(0));
        let rows = rendered(&picker, 80, picker.desired_height());
        assert_eq!(rows[1], "   Rewind");
        assert_eq!(
            rows[3],
            "   Restore the code and/or conversation to the point before…"
        );
        assert!(rows[7].starts_with("     Prompt 0"), "{rows:#?}");
        assert_eq!(rows[8], "     No code changes");
        assert_eq!(rows[9], "");
        assert!(rows[10].starts_with("     Prompt 1"), "{rows:#?}");
        assert_eq!(rows[13], "   ❯ (current)");
        assert_eq!(rows[rows.len() - 1], "   Enter to continue · Esc to cancel");
    }

    #[test]
    fn selection_moves_the_marker_without_touching_the_stored_order() {
        let mut picker = picker(2, Some(0));
        picker.handle(&key(KeyCode::Up));
        let rows = rendered(&picker, 80, picker.desired_height());
        assert!(rows[10].starts_with("   ❯ Prompt 1"), "{rows:#?}");
        assert_eq!(rows[13], "     (current)");
        picker.handle(&key(KeyCode::Up));
        let rows = rendered(&picker, 80, picker.desired_height());
        assert!(rows[7].starts_with("   ❯ Prompt 0"), "{rows:#?}");
        assert_eq!(
            picker
                .request
                .points
                .iter()
                .map(|point| point.turn)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn confirmation_shows_the_prompt_its_age_and_every_numbered_source_choice() {
        let mut picker = picker(1, Some(0));
        picker.handle(&key(KeyCode::Up));
        picker.handle(&key(KeyCode::Enter));
        let rows = rendered(&picker, 90, picker.desired_height());
        assert_eq!(rows[1], "   Rewind");
        assert_eq!(
            rows[3],
            "   Confirm you want to restore to the point before you sent this message:"
        );
        assert!(rows[5].starts_with("   │ Prompt 0"), "{rows:#?}");
        assert!(
            rows[6].starts_with("   │ (") && rows[6].ends_with("ago)"),
            "the checkpoint states its own age: {:?}",
            rows[6]
        );
        assert_eq!(rows[8], "   The conversation will be forked.");
        assert_eq!(rows[9], "   The code will be unchanged.");
        assert_eq!(rows[11], "   ❯ 1. Restore conversation");
        assert_eq!(
            rows[12],
            "     2. Summarize from here (unavailable in heycode)"
        );
        assert_eq!(
            rows[13],
            "     3. Summarize up to here (unavailable in heycode)"
        );
        assert_eq!(rows[14], "     4. Never mind");
    }

    #[test]
    fn summarizing_choices_are_never_selectable_and_never_restore() {
        let mut picker = picker(1, Some(0));
        picker.handle(&key(KeyCode::Up));
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(picker.choice, Some(0));
        picker.handle(&key(KeyCode::Down));
        assert_eq!(
            picker.choice,
            Some(3),
            "navigation skips the rows heycode cannot honour"
        );
        picker.handle(&key(KeyCode::Up));
        assert_eq!(picker.choice, Some(0));
        picker.choice = Some(1);
        assert_eq!(
            picker.handle(&key(KeyCode::Enter)),
            RewindPickerOutcome::None,
            "a click landing on an unavailable row must not restore anything"
        );
    }

    #[test]
    fn code_labels_separate_no_change_from_an_unavailable_restore() {
        assert_eq!(
            picker(1, Some(0)).request.points[0].code_label(),
            "No code changes"
        );
        assert_eq!(
            picker(1, None).request.points[0].code_label(),
            "⚠ No code restore"
        );
        assert_eq!(
            picker(1, Some(1)).request.points[0].code_label(),
            "1 native file can be restored"
        );
        assert_eq!(
            picker(1, Some(3)).request.points[0].code_label(),
            "3 native files can be restored"
        );
    }

    #[test]
    fn compact_age_reports_units_and_refuses_to_guess_an_unknown_time() {
        let now = 1_800_000_000_000_i64;
        assert_eq!(compact_age(now, now), "0s ago");
        assert_eq!(compact_age(now, now - 8_000), "8s ago");
        assert_eq!(compact_age(now, now - 120_000), "2m ago");
        assert_eq!(compact_age(now, now - 7_200_000), "2h ago");
        assert_eq!(compact_age(now, now - 172_800_000), "2d ago");
        assert_eq!(compact_age(now, now + 1_000), "unknown age");
    }

    #[test]
    fn current_position_is_non_mutating_and_confirmation_defaults_to_conversation() {
        let mut picker = picker(2, Some(1));
        assert_eq!(
            picker.handle(&key(KeyCode::Enter)),
            RewindPickerOutcome::Cancelled
        );
        picker.handle(&key(KeyCode::Up));
        assert_eq!(
            picker.handle(&key(KeyCode::Enter)),
            RewindPickerOutcome::None
        );
        let RewindPickerOutcome::Submit(selection) = picker.handle(&key(KeyCode::Enter)) else {
            panic!("missing restore");
        };
        assert_eq!(selection.command(), "/rewind 1");
        assert!(selection.belongs_to(&SessionId::from_raw("source-session")));
        assert!(!selection.belongs_to(&SessionId::from_raw("other-session")));
        picker.handle(&key(KeyCode::Down));
        let RewindPickerOutcome::Submit(selection) = picker.handle(&key(KeyCode::Enter)) else {
            panic!("missing native file restore");
        };
        assert_eq!(selection.command(), "/rewind 1 files");
    }

    #[test]
    fn only_confirmed_native_file_candidates_enable_file_mode() {
        for files in [None, Some(0), Some(2)] {
            let mut picker = picker(1, files);
            picker.handle(&key(KeyCode::Up));
            picker.handle(&key(KeyCode::Enter));
            assert_eq!(
                picker.modes().contains(&Mode::ConversationAndFiles),
                files == Some(2)
            );
            assert_eq!(picker.request.points[0].files, files);
        }
    }

    #[test]
    fn empty_escape_paste_and_repeated_enter_never_restore() {
        let mut empty = picker(0, Some(0));
        assert_eq!(
            empty.handle(&key(KeyCode::Enter)),
            RewindPickerOutcome::Cancelled
        );
        let mut picker = picker(1, Some(0));
        picker.handle(&key(KeyCode::Up));
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.handle(&Event::Paste("/rewind 0 files\n".into())),
            RewindPickerOutcome::None
        );
        assert_eq!(
            picker.handle(&Event::Key(KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat
            ))),
            RewindPickerOutcome::None
        );
        assert_eq!(picker.handle(&key(KeyCode::Esc)), RewindPickerOutcome::None);
        assert_eq!(
            picker.handle(&key(KeyCode::Esc)),
            RewindPickerOutcome::Cancelled
        );
    }

    #[test]
    fn visible_action_click_activates_and_resize_invalidates_old_targets() {
        let mut picker = picker(60, Some(0));
        assert_eq!(picker.request.points.len(), 50);
        assert!(
            picker
                .accessible_lines()
                .iter()
                .all(|line| !line.contains('\u{1b}'))
        );
        picker.handle(&key(KeyCode::Up));
        picker.handle(&key(KeyCode::Enter));
        let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, frame.area(), &Styles::default()))
            .unwrap();
        let (row, _) = picker.hit_rows.borrow()[0];
        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 3,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            picker.handle(&mouse),
            RewindPickerOutcome::Submit(_)
        ));
        terminal
            .draw(|frame| picker.draw(frame, frame.area(), &Styles::default()))
            .unwrap();
        picker.handle(&Event::Resize(20, 4));
        assert_eq!(picker.handle(&mouse), RewindPickerOutcome::None);
    }

    #[test]
    fn short_frames_keep_selected_action_visible_without_hidden_hit_targets() {
        let mut picker = picker(1, Some(0));
        picker.handle(&key(KeyCode::Up));
        picker.handle(&key(KeyCode::Enter));
        picker.handle(&key(KeyCode::Down));
        for height in 1..14 {
            let mut terminal = Terminal::new(TestBackend::new(30, height)).unwrap();
            terminal
                .draw(|frame| picker.draw(frame, frame.area(), &Styles::default()))
                .unwrap();
            let hits = picker.hit_rows.borrow();
            let chosen = picker.choice.unwrap();
            if height < 4 {
                assert!(hits.is_empty());
            } else {
                assert!(hits.iter().any(|(_, index)| *index == chosen));
            }
            assert!(hits.iter().all(|(area, _)| area.y < height - 1));
        }
    }
}
