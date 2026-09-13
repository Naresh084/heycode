//! Dedicated, full-document plan review. No ordinary approval shortcut accepts a plan.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use heycode_agent::PlanReviewDecision;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Withdraw pending plan reviews on every shell return or failed startup path.
pub(crate) struct ReviewSurfaceGuard(
    pub Option<std::sync::Arc<heycode_agent::InteractiveApproval>>,
);
impl Drop for ReviewSurfaceGuard {
    fn drop(&mut self) {
        if let Some(policy) = &self.0 {
            policy.set_plan_review_available(false);
        }
    }
}

/// The three explicit implementation decisions, in display order.
pub const CHOICES: [&str; 3] = [
    "Yes, accept the plan and make changes — Accepted edits",
    "Yes, use Default permissions — ask as required",
    "No, stay in Plan mode — provide feedback and revise",
];

/// One complete proposal with independent document scrolling and choice focus.
#[derive(Debug, Clone)]
pub struct PlanReviewView {
    /// Exact pending review correlation.
    pub id: u64,
    /// Complete Markdown proposal. Never truncated to an argument preview.
    pub plan: String,
    /// Zero-based decision selection; starts on remain in Plan.
    pub selection: usize,
    /// Human revision feedback.
    pub feedback: String,
    /// Rendered document row offset.
    pub scroll: usize,
    max_scroll: usize,
}
impl PlanReviewView {
    /// Open a review without authorizing implementation.
    #[must_use]
    pub fn new(id: u64, plan: String) -> Self {
        let max_scroll = crate::markdown::render_markdown(&plan, 78)
            .len()
            .saturating_sub(12);
        Self {
            id,
            plan,
            selection: 2,
            feedback: String::new(),
            scroll: 0,
            max_scroll,
        }
    }

    /// The modal owns all input; only Enter selects an explicit decision.
    pub fn handle(&mut self, event: &Event) -> Option<PlanReviewDecision> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    return Some(PlanReviewDecision::StayInPlan {
                        feedback: self.feedback.clone(),
                    });
                }
                KeyCode::Enter => {
                    return Some(match self.selection {
                        0 => PlanReviewDecision::AcceptedEdits,
                        1 => PlanReviewDecision::DefaultPermissions,
                        _ => PlanReviewDecision::StayInPlan {
                            feedback: self.feedback.clone(),
                        },
                    });
                }
                KeyCode::Tab | KeyCode::Right => self.selection = (self.selection + 1) % 3,
                KeyCode::BackTab | KeyCode::Left => self.selection = (self.selection + 2) % 3,
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll = (self.scroll + 1).min(self.max_scroll),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(12),
                KeyCode::PageDown => self.scroll = (self.scroll + 12).min(self.max_scroll),
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = self.max_scroll,
                KeyCode::Backspace => {
                    self.feedback.pop();
                    self.selection = 2;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    if self.feedback.len() < 16 * 1024 {
                        self.feedback.push(c);
                    }
                    self.selection = 2;
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(3).min(self.max_scroll)
                }
                _ => {}
            },
            Event::Paste(text) => {
                let remaining = (16_usize * 1024).saturating_sub(self.feedback.len());
                self.feedback
                    .extend(text.chars().filter(|c| !c.is_control()).take(remaining));
                self.selection = 2;
            }
            _ => {}
        }
        None
    }

    /// Complete accessible document viewport, with no unreachable truncated tail.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec!["Plan review — read-only until explicitly accepted".into()];
        let document = crate::markdown::render_markdown(&self.plan, 78);
        let start = self.scroll.min(document.len().saturating_sub(12));
        lines.push(format!(
            "Document rows {}–{} of {}",
            start + 1,
            (start + 12).min(document.len()),
            document.len()
        ));
        lines.extend(
            document
                .into_iter()
                .skip(start)
                .take(12)
                .map(|line| line.to_string()),
        );
        lines.extend(
            CHOICES
                .iter()
                .enumerate()
                .map(|(i, text)| format!("{} {text}", if i == self.selection { ">" } else { " " })),
        );
        lines.push(format!("Feedback: {}", self.feedback));
        lines.push("Up/Down/PgUp/PgDn/Home/End scroll · Tab/Left/Right choose · Enter confirm · Esc remain in Plan".into());
        lines
    }
}

/// Draw the entire available screen as a dedicated Markdown review with fixed decisions.
pub fn draw(frame: &mut Frame<'_>, view: &mut PlanReviewView, area: Rect) {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Plan review · read-only until accepted ")
        .borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let footer_height = inner.height.min(9);
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(footer_height)]).areas(inner);
    let document = crate::markdown::render_markdown(&view.plan, usize::from(body.width.max(1)));
    view.max_scroll = document
        .len()
        .saturating_sub(usize::from(body.height.max(1)));
    view.scroll = view.scroll.min(view.max_scroll);
    let count = document.len();
    let visible = document
        .into_iter()
        .skip(view.scroll)
        .take(usize::from(body.height))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), body);
    let mut lines = vec![Line::from(format!(
        "Document row {} of {count} · ↑↓ PgUp/PgDn Home/End scroll",
        view.scroll + 1
    ))];
    for (i, choice) in CHOICES.iter().enumerate() {
        let style = if i == view.selection {
            Style::default().bold().fg(ratatui::style::Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            format!("{} {choice}", if i == view.selection { ">" } else { " " }),
            style,
        ));
    }
    lines.push(Line::from(format!("Feedback: {}", view.feedback)));
    lines.push(Line::from(
        "Tab/←/→ choose · Enter confirm · Esc stay · Type feedback to revise",
    ));
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        footer,
    );
}
