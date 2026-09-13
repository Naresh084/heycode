//! Nonblocking optional questions projected from the originating durable session.
use super::{AppState, Item, LoopDeps};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

#[derive(Clone, Debug)]
pub struct OptionalQuestionView {
    pub mode: heycode_core::QuestionMode,
    pub header: Option<String>,
    pub choice_descriptions: Vec<Option<String>>,
    pub selected_choices: std::collections::BTreeSet<usize>,
    pub session_id: String,
    pub question_id: String,
    pub owner_label: String,
    pub child_id: Option<String>,
    pub requires_reopen: bool,
    pub requires_restore: bool,
    pub prompt: String,
    pub choices: Vec<String>,
    pub selection: usize,
    pub input: String,
}

#[derive(Default)]
pub struct OptionalQuestions {
    pub rows: Vec<OptionalQuestionView>,
    pub open: bool,
    pub selected: usize,
    pub badge_area: Option<Rect>,
    option_hits: Vec<(usize, Rect)>,
    response: Option<(
        String,
        String,
        Option<String>,
        Option<heycode_agent::QuestionAnswer>,
    )>,
    reopen: Option<String>,
    settled: std::collections::VecDeque<(String, String)>,
}

impl OptionalQuestions {
    fn reconcile(&mut self, mut rows: Vec<OptionalQuestionView>) {
        let selected = self
            .rows
            .get(self.selected)
            .map(|row| (row.session_id.clone(), row.question_id.clone()));
        for row in &mut rows {
            if let Some(old) = self
                .rows
                .iter()
                .find(|old| old.session_id == row.session_id && old.question_id == row.question_id)
            {
                row.selection = old.selection;
                row.input.clone_from(&old.input);
                row.selected_choices.clone_from(&old.selected_choices);
            }
        }
        self.selected = selected
            .and_then(|(session, question)| {
                rows.iter()
                    .position(|row| row.session_id == session && row.question_id == question)
            })
            .unwrap_or(0);
        self.rows = rows;
        if self.rows.is_empty() {
            self.open = false;
        }
    }

    pub fn current(&self) -> Option<&OptionalQuestionView> {
        self.rows.get(self.selected)
    }

    fn handle(&mut self, event: &Event) {
        let count = self.rows.len();
        let Some(row) = self.rows.get_mut(self.selected) else {
            self.open = false;
            return;
        };
        if let Event::Mouse(mouse) = event {
            if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) {
                if let Some((index, _)) = self
                    .option_hits
                    .iter()
                    .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                {
                    row.selection = *index;
                    if row.mode == heycode_core::QuestionMode::MultipleChoice
                        && *index < row.choices.len()
                    {
                        if !row.selected_choices.insert(*index) {
                            row.selected_choices.remove(index);
                        }
                    }
                }
            } else if mouse.kind == MouseEventKind::ScrollUp {
                row.selection = row.selection.saturating_sub(1);
            } else if mouse.kind == MouseEventKind::ScrollDown {
                row.selection = (row.selection + 1).min(row.choices.len() + 1);
            }
            return;
        }
        if row.requires_reopen {
            match event {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Enter =>
                {
                    self.reopen = Some(row.session_id.clone());
                    self.open = false;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc => {
                    self.open = false
                }
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && key.code == KeyCode::Tab && count > 1 =>
                {
                    self.selected = (self.selected + 1) % count
                }
                _ => {}
            }
            return;
        }
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => self.open = false,
                KeyCode::Tab | KeyCode::BackTab if count > 1 => {
                    self.selected = if key.code == KeyCode::BackTab {
                        (self.selected + count - 1) % count
                    } else {
                        (self.selected + 1) % count
                    };
                }
                KeyCode::Up => row.selection = row.selection.saturating_sub(1),
                KeyCode::Down => row.selection = (row.selection + 1).min(row.choices.len() + 1),
                KeyCode::Char(' ')
                    if row.mode == heycode_core::QuestionMode::MultipleChoice
                        && row.selection < row.choices.len() =>
                {
                    if !row.selected_choices.insert(row.selection) {
                        row.selected_choices.remove(&row.selection);
                    }
                }
                KeyCode::Enter => {
                    let answer = if row.selection == row.choices.len() + 1 {
                        None
                    } else if row.mode == heycode_core::QuestionMode::MultipleChoice
                        && row.selection < row.choices.len()
                    {
                        let labels = row
                            .selected_choices
                            .iter()
                            .filter_map(|index| row.choices.get(*index).cloned())
                            .collect::<Vec<_>>();
                        if labels.is_empty() {
                            return;
                        }
                        Some(heycode_agent::QuestionAnswer::Selected(labels))
                    } else {
                        let answer = row
                            .choices
                            .get(row.selection)
                            .cloned()
                            .unwrap_or_else(|| row.input.clone());
                        if answer.trim().is_empty() {
                            return;
                        }
                        Some(heycode_agent::QuestionAnswer::Answer(answer))
                    };
                    self.response = Some((
                        row.session_id.clone(),
                        row.question_id.clone(),
                        row.child_id.clone(),
                        answer,
                    ));
                    self.open = false;
                }
                KeyCode::Backspace => {
                    row.input.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control()
                        && row.input.len() + c.len_utf8() <= 16 * 1024 =>
                {
                    row.selection = row.choices.len();
                    row.input.push(c);
                }
                _ => {}
            },
            Event::Paste(text) => {
                row.selection = row.choices.len();
                for c in text.chars().filter(|c| !c.is_control()) {
                    if row.input.len() + c.len_utf8() > 16 * 1024 {
                        break;
                    }
                    row.input.push(c);
                }
            }
            _ => {}
        }
    }
}

impl AppState {
    /// Whether the deliberately opened optional panel currently owns focus.
    pub fn optional_question_panel_visible(&self) -> bool {
        self.optional_questions.open
            && !self.high_priority_modal_open()
            && self.pending_plan_review.is_none()
    }

    /// Open saved optional questions without modifying the composer or its cursor.
    pub fn open_optional_questions(&mut self) {
        if self.optional_questions.rows.is_empty() {
            self.items
                .push(Item::Info("No optional questions pending".into()));
        } else {
            self.optional_questions.open = true;
        }
    }

    pub(super) fn handle_optional_question_event(&mut self, event: &Event) -> bool {
        if self.high_priority_modal_open() || self.pending_plan_review.is_some() {
            return false;
        }
        if self.optional_question_panel_visible() {
            self.optional_questions.handle(event);
            return true;
        }
        let open = matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::ALT)
            || matches!(event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) && self.optional_questions.badge_area.is_some_and(|area| area.contains((mouse.column, mouse.row).into())));
        if open {
            self.open_optional_questions();
        }
        open
    }

    pub(super) fn apply_optional_question(
        &mut self,
        session_id: &str,
        question_id: &str,
        prompt: &str,
        choices: &[String],
    ) {
        if self
            .optional_questions
            .settled
            .iter()
            .any(|(session, question)| session == session_id && question == question_id)
        {
            return;
        }
        // Only the current session's bus is admitted here. Child cards are
        // recovered through the authorized registry, including their task ID.
        if self
            .current_session_id()
            .as_ref()
            .map(heycode_core::SessionId::as_str)
            != Some(session_id)
        {
            return;
        }
        if !self
            .optional_questions
            .rows
            .iter()
            .any(|row| row.session_id == session_id && row.question_id == question_id)
        {
            self.optional_questions.rows.push(OptionalQuestionView {
                mode: heycode_core::QuestionMode::SingleChoice,
                header: None,
                choice_descriptions: Vec::new(),
                selected_choices: Default::default(),
                session_id: session_id.into(),
                question_id: question_id.into(),
                owner_label: "Conversation".into(),
                child_id: None,
                requires_reopen: false,
                requires_restore: false,
                prompt: prompt.into(),
                choices: choices.to_vec(),
                selection: 0,
                input: String::new(),
            });
        }
    }

    pub(super) fn settle_optional_question(&mut self, session_id: &str, question_id: &str) {
        self.optional_questions
            .settled
            .push_back((session_id.into(), question_id.into()));
        if self.optional_questions.settled.len() > 1024 {
            self.optional_questions.settled.pop_front();
        }
        let rows = self
            .optional_questions
            .rows
            .iter()
            .filter(|row| row.session_id != session_id || row.question_id != question_id)
            .cloned()
            .collect();
        self.optional_questions.reconcile(rows);
    }
}

fn rows_for(
    agent: &heycode_agent::Agent,
    label: String,
    child_id: Option<String>,
) -> anyhow::Result<Vec<OptionalQuestionView>> {
    let session_id = agent
        .session()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .id()
        .to_string();
    Ok(agent
        .async_questions()?
        .into_iter()
        .map(|question| OptionalQuestionView {
            mode: question.mode,
            header: question.header,
            choice_descriptions: question.descriptions,
            selected_choices: Default::default(),
            session_id: session_id.clone(),
            question_id: question.id.to_string(),
            owner_label: label.clone(),
            child_id: child_id.clone(),
            requires_reopen: false,
            requires_restore: false,
            prompt: question.question,
            choices: question.options,
            selection: 0,
            input: String::new(),
        })
        .collect())
}

pub(super) fn refresh(state: &mut AppState, deps: &LoopDeps) -> anyhow::Result<()> {
    let mut rows = rows_for(&deps.agent, "Conversation".into(), None)?;
    if let Some(registry) = deps.subagents.as_ref() {
        let owner = deps
            .agent
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .to_string();
        let authority = registry.root_authority(heycode_agent::SubagentId::new(owner)?);
        for task in registry.task_snapshots_for(&authority) {
            let id = heycode_agent::SubagentId::new(task.id.clone())?;
            if let Some(child) = registry.native_child_for(&authority, &id) {
                let mut child_rows = rows_for(&child, task.label, Some(task.id))?;
                if registry.child_for(&authority, &id).is_none()
                    && !matches!(
                        task.state,
                        heycode_agent::TaskState::Queued
                            | heycode_agent::TaskState::Running
                            | heycode_agent::TaskState::Cancelling
                    )
                {
                    // Archived handles still own their writer. They must be restored through /agents.
                    for row in &mut child_rows {
                        row.requires_reopen = true;
                        row.requires_restore = true;
                    }
                }
                rows.extend(child_rows);
            } else if let Some(session_id) = task.session_id {
                // Session::open is read-only and validates session storage. The task
                // record supplies provenance; never form paths from display labels.
                if session_id.is_empty()
                    || !session_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    anyhow::bail!("invalid optional question session identity");
                }
                let root = deps
                    .agent
                    .session()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .path()
                    .parent()
                    .and_then(std::path::Path::parent)
                    .map(std::path::Path::to_path_buf)
                    .ok_or_else(|| anyhow::anyhow!("session root unavailable"))?;
                let path = root.join(&session_id);
                if path.is_dir() {
                    let session = heycode_session::Session::open(path)?;
                    rows.extend(
                        heycode_agent::AsyncQuestion::pending_for_session(&session)?
                            .into_iter()
                            .map(|question| OptionalQuestionView {
                                mode: question.mode,
                                header: question.header,
                                choice_descriptions: question.descriptions,
                                selected_choices: Default::default(),
                                session_id: session_id.clone(),
                                question_id: question.id.to_string(),
                                owner_label: task.label.clone(),
                                child_id: Some(task.id.clone()),
                                requires_reopen: true,
                                requires_restore: false,
                                prompt: question.question,
                                choices: question.options,
                                selection: 0,
                                input: String::new(),
                            }),
                    );
                }
            }
        }
    }
    state.optional_questions.reconcile(rows);
    Ok(())
}

pub(super) fn resolve_pending(state: &mut AppState, deps: &LoopDeps) {
    if let Some(session_id) = state.optional_questions.reopen.take() {
        let restore = state
            .optional_questions
            .rows
            .iter()
            .find(|row| row.session_id == session_id && row.requires_restore)
            .and_then(|row| row.child_id.clone());
        if let (Some(child_id), Some(registry)) = (restore, deps.subagents.as_ref()) {
            let result = (|| -> anyhow::Result<()> {
                let owner = deps
                    .agent
                    .session()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .id()
                    .to_string();
                let authority = registry.root_authority(heycode_agent::SubagentId::new(owner)?);
                if !registry
                    .restore_child_for(&authority, &heycode_agent::SubagentId::new(child_id)?)?
                {
                    anyhow::bail!("child conversation could not be restored");
                }
                refresh(state, deps)?;
                state.optional_questions.open = true;
                Ok(())
            })();
            if let Err(error) = result {
                state.items.push(Item::Error(error.to_string()));
            }
        } else {
            state.handle_session_command(crate::session_browser::SessionCommandRequest::Resume(
                heycode_core::SessionId::from_raw(session_id),
            ));
        }
    }
    let Some((session_id, question_id, child_id, answer)) =
        state.optional_questions.response.take()
    else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        let owner = deps
            .agent
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .to_string();
        if let Some(child_id) = child_id {
            let registry = deps
                .subagents
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("question owner unavailable"))?;
            let jobs = deps
                .jobs
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("child follow-up host unavailable"))?;
            let authority = registry.root_authority(heycode_agent::SubagentId::new(owner)?);
            let id = heycode_agent::SubagentId::new(child_id)?;
            let child = registry
                .native_child_for(&authority, &id)
                .ok_or_else(|| anyhow::anyhow!("question owner unavailable"))?;
            if child
                .session()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .id()
                .as_str()
                != session_id
            {
                anyhow::bail!("question owner changed");
            }
            registry.resolve_optional_question_value_for(
                &authority,
                &id,
                &question_id,
                answer.as_ref(),
                jobs,
            )
        } else {
            if owner != session_id {
                anyhow::bail!("question belongs to another session");
            }
            if answer.is_some() && !state.native_inbox_available() {
                anyhow::bail!("optional answers require the originating native session route");
            }
            match answer.as_ref() {
                Some(answer) => deps.agent.answer_async_value(&question_id, answer),
                None => deps.agent.cancel_async(&question_id),
            }
        }
    })();
    if let Err(error) = result {
        state.items.push(Item::Error(error.to_string()));
    } else {
        state.items.push(Item::Info(
            if answer.is_some() {
                "Optional answer saved"
            } else {
                "Optional question dismissed"
            }
            .into(),
        ));
    }
    if let Err(error) = refresh(state, deps) {
        state.items.push(Item::Error(error.to_string()));
    }
}

pub fn panel_lines(state: &AppState, width: u16, available_rows: usize) -> Vec<Line<'static>> {
    let Some(row) = state.optional_questions.current() else {
        return Vec::new();
    };
    let styles = state.styles();
    let safe = |text: &str| crate::markdown::terminal_safe_span(text).into_owned();
    if row.requires_reopen {
        return vec![
            Line::styled(
                format!("Optional question · {}", safe(&row.owner_label)),
                Style::default().fg(styles.accent()),
            ),
            Line::from(safe(&row.prompt)),
            Line::from(if row.requires_restore {
                "This child conversation is archived. Restore it to answer."
            } else {
                "This saved child session needs to be reopened to answer."
            }),
            Line::styled(
                if row.requires_restore {
                    "❯ Restore child conversation"
                } else {
                    "❯ Open original session"
                },
                Style::default().fg(styles.accent()),
            ),
            Line::from("Enter open · Tab next · Esc close"),
        ];
    }
    let header = vec![
        Line::styled(
            format!(
                "Optional question {} of {} · {}",
                state.optional_questions.selected + 1,
                state.optional_questions.rows.len(),
                safe(&row.owner_label)
            ),
            Style::default().fg(styles.accent()),
        ),
        Line::from("Work continues while you decide."),
        Line::default(),
    ]
    .into_iter()
    .chain(crate::markdown::wrap_styled(
        &[Span::styled(
            safe(&row.prompt),
            Style::default().fg(styles.text()).bold(),
        )],
        usize::from(width.max(1)),
        0,
    ))
    .chain([Line::default()])
    .collect();
    let choices = row
        .choices
        .iter()
        .map(|choice| safe(choice))
        .chain(["Custom answer".into(), "Dismiss question".into()]);
    let options = choices
        .enumerate()
        .map(|(index, choice)| {
            let style = Style::default().fg(if row.selection == index {
                styles.accent()
            } else {
                styles.text()
            });
            let mark = if row.mode == heycode_core::QuestionMode::MultipleChoice
                && index < row.choices.len()
            {
                if row.selected_choices.contains(&index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            let mut lines = crate::markdown::wrap_styled(
                &[
                    Span::styled(if row.selection == index { "❯ " } else { "  " }, style),
                    Span::styled(format!("{}. {mark}{choice}", index + 1), style),
                ],
                usize::from(width.max(1)),
                4,
            );
            if let Some(Some(description)) = row.choice_descriptions.get(index) {
                lines.extend(crate::markdown::wrap_styled(
                    &[Span::styled(
                        format!("    {}", safe(description)),
                        Style::default().fg(styles.dim()),
                    )],
                    usize::from(width.max(1)),
                    4,
                ));
            }
            lines
        })
        .collect();
    let mut footer = Vec::new();
    if row.selection == row.choices.len() {
        footer.push(Line::from(format!(
            "Answer  {}",
            visible_answer(&row.input, width)
        )));
    }
    footer.push(Line::styled(
        if row.mode == heycode_core::QuestionMode::MultipleChoice {
            "Space toggle · Enter submit · Tab next · Esc close"
        } else {
            "↑/↓ choose · Enter confirm · Tab next · Esc close"
        },
        Style::default().fg(styles.dim()),
    ));
    crate::panel_frame::option_window(header, options, footer, row.selection, available_rows)
}

fn visible_answer(input: &str, width: u16) -> String {
    let safe = crate::markdown::terminal_safe_span(input);
    let mut remaining = usize::from(width).saturating_sub(9);
    let mut tail = Vec::new();
    for grapheme in safe.graphemes(true).rev() {
        if grapheme.width() > remaining {
            break;
        }
        remaining = remaining.saturating_sub(grapheme.width());
        tail.push(grapheme);
    }
    tail.into_iter().rev().collect()
}

pub fn draw(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let lines = panel_lines(
        state,
        area.width,
        usize::from(area.height.saturating_sub(1)),
    );
    let mut hits = Vec::new();
    if let Some(question) = state.optional_questions.current() {
        for (index, choice) in question
            .choices
            .iter()
            .map(String::as_str)
            .chain(["Custom answer", "Dismiss question"])
            .enumerate()
        {
            let mark = if question.mode == heycode_core::QuestionMode::MultipleChoice
                && index < question.choices.len()
            {
                if question.selected_choices.contains(&index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            let caption = format!(
                "{}. {mark}{}",
                index + 1,
                crate::markdown::terminal_safe_span(choice)
            );
            if let Some(row) = lines.iter().position(|line| {
                let text = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                let text = text
                    .trim_start()
                    .strip_prefix("❯ ")
                    .unwrap_or(text.trim_start());
                // Styled wrapping may merge spans or split a long caption. Match
                // its visible first line rather than a particular span index.
                text.starts_with(&format!("{}. ", index + 1)) && caption.starts_with(text)
            }) {
                let y = area
                    .y
                    .saturating_add(u16::try_from(row + 1).unwrap_or(u16::MAX));
                if y < area.bottom() {
                    hits.push((index, Rect::new(area.x, y, area.width, 1)));
                }
            }
        }
    }
    state.optional_questions.option_hits = hits;
    let cursor = state
        .optional_questions
        .current()
        .filter(|question| {
            !question.requires_reopen && question.selection == question.choices.len()
        })
        .and_then(|question| {
            let visible = visible_answer(&question.input, area.width);
            let expected = Line::from(format!("Answer  {visible}"));
            lines.iter().position(|line| *line == expected).map(|row| {
                (
                    area.x
                        .saturating_add(u16::try_from(8 + visible.width()).unwrap_or(u16::MAX))
                        .min(area.right().saturating_sub(1)),
                    area.y
                        .saturating_add(u16::try_from(row + 1).unwrap_or(u16::MAX)),
                )
            })
        });
    crate::panel_frame::render(frame, area, state.styles(), lines);
    if let Some((x, y)) = cursor
        && y < area.bottom()
    {
        frame.set_cursor_position((x, y));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn row(id: &str) -> OptionalQuestionView {
        OptionalQuestionView {
            mode: heycode_core::QuestionMode::SingleChoice,
            header: None,
            choice_descriptions: Vec::new(),
            selected_choices: Default::default(),
            session_id: "session-owner".into(),
            question_id: id.into(),
            owner_label: "Conversation".into(),
            child_id: None,
            requires_reopen: false,
            requires_restore: false,
            prompt: "Which format?".into(),
            choices: vec!["Markdown".into(), "Text".into()],
            selection: 0,
            input: String::new(),
        }
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn arrival_close_and_reopen_preserve_composer_and_custom_answer() {
        let mut state = AppState::new("test", "/workspace".into());
        state.replace_input("existing draft".into());
        state.optional_questions.reconcile(vec![row("first")]);
        assert!(!state.optional_question_panel_visible());
        state.handle_terminal_event(&key(KeyCode::Char('!')));
        let composer = state.input.lines().to_vec();
        let cursor = state.input.cursor();
        assert!(
            state.handle_optional_question_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::ALT
            )))
        );
        state.handle_terminal_event(&Event::Paste("custom answer".into()));
        state.handle_terminal_event(&key(KeyCode::Esc));
        assert!(!state.optional_question_panel_visible());
        assert_eq!(state.optional_questions.rows.len(), 1);
        assert_eq!(state.optional_questions.rows[0].input, "custom answer");
        assert_eq!(state.input.lines(), composer.as_slice());
        assert_eq!(state.input.cursor(), cursor);
        state
            .optional_questions
            .reconcile(vec![row("first"), row("second")]);
        state.open_optional_questions();
        assert_eq!(
            state.optional_questions.current().unwrap().input,
            "custom answer"
        );
        state.handle_terminal_event(&key(KeyCode::Enter));
        let response = state.optional_questions.response.take().unwrap();
        assert_eq!(response.0, "session-owner");
        assert_eq!(response.1, "first");
        assert_eq!(
            response.3,
            Some(heycode_agent::QuestionAnswer::Answer(
                "custom answer".into()
            ))
        );
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert!(state.optional_questions.response.is_none());
    }

    #[test]
    fn explicit_dismiss_is_distinct_from_close_and_preserves_other_owner() {
        let mut questions = OptionalQuestions::default();
        let mut child = row("same-question-id");
        child.session_id = "child-owner".into();
        child.child_id = Some("task-handle".into());
        questions.reconcile(vec![row("same-question-id"), child]);
        questions.open = true;
        questions.handle(&key(KeyCode::Tab));
        for _ in 0..3 {
            questions.handle(&key(KeyCode::Down));
        }
        questions.handle(&key(KeyCode::Enter));
        assert_eq!(
            questions.response.take(),
            Some((
                "child-owner".into(),
                "same-question-id".into(),
                Some("task-handle".into()),
                None
            ))
        );
        questions.reconcile(vec![row("same-question-id")]);
        assert_eq!(questions.current().unwrap().session_id, "session-owner");
    }

    #[test]
    fn required_question_preempts_optional_without_losing_optional_edit() {
        let mut state = AppState::new("test", "/workspace".into());
        state.optional_questions.reconcile(vec![row("optional")]);
        state.open_optional_questions();
        state.handle_terminal_event(&Event::Paste("retained".into()));
        state.pending_runtime_question = Some(super::super::PendingRuntimeQuestionView {
            mode: heycode_core::QuestionMode::SingleChoice,
            progress: (1, 1),
            selected_choices: Default::default(),
            request_id: "required".into(),
            header: None,
            prompt: "Required?".into(),
            choices: vec!["Yes".into()],
            choice_descriptions: vec![None],
            selection: 0,
            input: String::new(),
        });
        assert!(!state.optional_question_panel_visible());
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert!(state.pending_runtime_question.is_none());
        assert!(state.optional_question_panel_visible());
        assert_eq!(
            state.optional_questions.current().unwrap().input,
            "retained"
        );
        assert!(state.optional_questions.response.is_none());
    }

    #[test]
    fn saved_child_reopens_exact_session_instead_of_admitting_answer() {
        let mut questions = OptionalQuestions::default();
        let mut child = row("question");
        child.requires_reopen = true;
        child.session_id = "original-child".into();
        questions.reconcile(vec![child]);
        questions.open = true;
        questions.handle(&key(KeyCode::Enter));
        assert_eq!(questions.reopen.as_deref(), Some("original-child"));
        assert!(questions.response.is_none());
    }

    #[test]
    fn rendered_mouse_choices_match_the_narrow_panel_and_custom_cursor() {
        let mut state = AppState::new("test", "/workspace".into());
        state.optional_questions.reconcile(vec![row("question")]);
        state.open_optional_questions();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 17)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, &mut state, area);
            })
            .unwrap();
        let area = state
            .optional_questions
            .option_hits
            .iter()
            .find(|(index, _)| *index == 2)
            .unwrap()
            .1;
        state.handle_terminal_event(&Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(state.optional_questions.current().unwrap().selection, 2);
        assert!(state.optional_questions.response.is_none());
        state.handle_terminal_event(&Event::Paste("界e\u{301}".repeat(60)));
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, &mut state, area);
            })
            .unwrap();
        assert!(
            visible_answer(&state.optional_questions.current().unwrap().input, 60).width() <= 51
        );
        state.handle_terminal_event(&key(KeyCode::Esc));
        assert_eq!(
            state.optional_questions.current().unwrap().input,
            "界e\u{301}".repeat(60)
        );
    }

    #[test]
    fn narrow_question_panel_has_choices_dismiss_and_no_internal_identifiers() {
        let mut state = AppState::new("test", "/workspace".into());
        state
            .optional_questions
            .reconcile(vec![row("secret-question-id")]);
        state.open_optional_questions();
        let text = panel_lines(&state, 60, 17)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Markdown"));
        assert!(text.contains("Dismiss question"));
        assert!(text.contains("Esc close"));
        assert!(!text.contains("secret-question-id"));
        assert!(!text.contains("session-owner"));
    }
    #[test]
    fn multi_select_requires_explicit_toggles_and_preserves_draft_on_close() {
        let mut questions = OptionalQuestions::default();
        let mut multiple = row("multiple");
        multiple.mode = heycode_core::QuestionMode::MultipleChoice;
        questions.reconcile(vec![multiple]);
        questions.open = true;
        questions.handle(&key(KeyCode::Enter));
        assert!(questions.response.is_none());
        questions.handle(&key(KeyCode::Char(' ')));
        questions.handle(&key(KeyCode::Down));
        questions.handle(&key(KeyCode::Char(' ')));
        questions.handle(&key(KeyCode::Esc));
        assert!(!questions.open);
        assert_eq!(questions.current().unwrap().selected_choices.len(), 2);
        questions.open = true;
        questions.handle(&key(KeyCode::Enter));
        assert_eq!(
            questions.response.unwrap().3,
            Some(heycode_agent::QuestionAnswer::Selected(vec![
                "Markdown".into(),
                "Text".into()
            ]))
        );
    }

    #[test]
    fn required_multi_select_and_custom_answer_use_distinct_typed_responses() {
        let mut state = AppState::new("test", "/workspace".into());
        let event = heycode_agent::UiEvent::RuntimeQuestionRequested {
            request_id: "required".into(),
            header: Some("Scope".into()),
            prompt: "Which scope?".into(),
            mode: heycode_core::QuestionMode::MultipleChoice,
            progress: (1, 2),
            choices: vec!["A".into(), "B".into()],
            choice_descriptions: vec![Some("First".into()), Some("Second".into())],
        };
        state.apply(&event);
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert!(state.pending_runtime_question.is_some());
        state.handle_terminal_event(&key(KeyCode::Char(' ')));
        state.handle_terminal_event(&key(KeyCode::Down));
        state.handle_terminal_event(&key(KeyCode::Char(' ')));
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert_eq!(
            state.take_runtime_question_response().unwrap().1,
            Some(heycode_agent::QuestionAnswer::Selected(vec![
                "A".into(),
                "B".into()
            ]))
        );
        state.apply(&event);
        state.handle_terminal_event(&Event::Paste("/custom, text".into()));
        state.handle_terminal_event(&key(KeyCode::Enter));
        assert_eq!(
            state.take_runtime_question_response().unwrap().1,
            Some(heycode_agent::QuestionAnswer::Answer(
                "/custom, text".into()
            ))
        );
        state.apply(&event);
        state.handle_terminal_event(&key(KeyCode::Esc));
        assert_eq!(state.take_runtime_question_response().unwrap().1, None);
    }
}
