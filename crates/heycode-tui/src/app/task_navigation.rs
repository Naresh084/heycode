//! Composer-preserving navigation for the authoritative task console.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;

use super::AppState;
use crate::task_console::{ConsoleView, TaskAction, TaskCategory, TaskHit, TaskKey, TaskSource};

/// Parent editor state stays separate while a task owns the visible composer.
pub(super) struct ParentTaskDraft {
    input: tui_textarea::TextArea<'static>,
    history_cursor: Option<(usize, Vec<String>)>,
}

impl AppState {
    /// One projection for strip rows, layout, navigation and footer counts.
    pub(crate) fn active_child_records(&self) -> Vec<&crate::task_console::TaskRecord> {
        self.strip_task_records()
            .into_iter()
            .filter(|row| row.active_child())
            .collect()
    }

    /// Pending human text belongs below the working indicator until admission.
    pub(crate) fn pending_message_texts(&self) -> Vec<String> {
        let child = self.task_console.active;
        let key = if child {
            self.task_console.selected.as_ref()
        } else {
            None
        };
        let mut messages = self
            .task_console
            .source
            .as_ref()
            .and_then(|source| source.pending_messages(key).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|message| message.text().to_owned())
            .collect::<Vec<_>>();
        if child {
            messages.extend(
                self.task_console
                    .actions
                    .iter()
                    .filter_map(|action| match action {
                        TaskAction::Steer { key: target, text } if Some(target) == key => {
                            Some(text.clone())
                        }
                        _ => None,
                    }),
            );
        } else {
            messages.extend(
                self.pending_inbox_submission
                    .iter()
                    .map(|(_, text)| text.clone()),
            );
        }
        messages
    }

    /// Recall through the owner before editing, so a racing claim wins exactly once.
    pub(super) fn recall_pending_messages(&mut self) -> bool {
        let child = self.task_console.active;
        let key = if child {
            self.task_console.selected.clone()
        } else {
            None
        };
        if self.pending_message_texts().is_empty() {
            return false;
        }
        let recalled = self.task_console.source.as_ref().map_or_else(
            || Ok(Vec::new()),
            |source| source.recall_messages(key.as_ref()),
        );
        let mut texts = match recalled {
            Ok(messages) => messages
                .into_iter()
                .map(|message| message.text().to_owned())
                .collect::<Vec<_>>(),
            Err(error) => {
                self.task_console.notice = Some(format!("Recall failed: {error}"));
                return true;
            }
        };
        if child {
            self.task_console.actions.retain(|action| {
                if let TaskAction::Steer { key: target, text } = action
                    && Some(target) == key.as_ref()
                {
                    texts.push(text.clone());
                    false
                } else {
                    true
                }
            });
        } else {
            texts.extend(
                self.pending_inbox_submission
                    .drain(..)
                    .map(|(_, text)| text),
            );
        }
        if texts.is_empty() {
            return true;
        }
        let draft = self.input.lines().join("\n");
        if !draft.is_empty() {
            texts.push(draft);
        }
        self.input = tui_textarea::TextArea::new(
            texts.join("\n\n").split('\n').map(str::to_owned).collect(),
        );
        self.input.move_cursor(tui_textarea::CursorMove::Bottom);
        self.input.move_cursor(tui_textarea::CursorMove::End);
        self.history_cursor = None;
        if let Some(key) = key {
            self.task_console
                .set_notice(key, "Queued messages moved to composer.".into());
        }
        true
    }

    /// Attach the live task owner. Custom compositions can provide the same
    /// contract without changing the renderer or constructing a second runtime.
    pub fn set_task_source(&mut self, source: std::sync::Arc<dyn TaskSource>) {
        self.task_console.attach(source);
    }

    /// Inspect the bounded task navigation state.
    #[must_use]
    pub const fn task_console(&self) -> &crate::task_console::TaskConsole {
        &self.task_console
    }

    /// Current transcript/composer owner, preserving the owner beneath a task
    /// inspector and never substituting parent data for an unavailable child.
    #[must_use]
    pub fn foreground_task(&self) -> Option<crate::task_console::ForegroundTask<'_>> {
        self.task_console.foreground()
    }

    /// The persisted task-console shortcut, formatted for terminal hints.
    #[must_use]
    pub fn task_toggle_key_label(&self) -> String {
        self.keymap
            .chord(heycode_ui::keymap::KeymapAction::ToggleTasks)
            .to_string()
            .split('+')
            .map(|word| match word {
                "ctrl" => "Ctrl".into(),
                "alt" => "Alt".into(),
                "shift" => "Shift".into(),
                other => other.to_uppercase(),
            })
            .collect::<Vec<String>>()
            .join("+")
    }

    /// Refresh authoritative state, including while the parent is idle.
    pub fn refresh_tasks(&mut self) {
        let selecting_list = self.task_console.view == ConsoleView::List;
        let previous = if selecting_list {
            self.task_console.visible_records()
        } else {
            self.active_child_records()
        }
        .into_iter()
        .map(|row| row.key.clone())
        .collect::<Vec<_>>();
        self.task_console.refresh();
        if (self.task_console.strip_focused || selecting_list) && !self.task_console.focus_main {
            let surviving = if selecting_list {
                self.task_console.visible_records()
            } else {
                self.active_child_records()
            };
            if !surviving
                .iter()
                .any(|row| Some(&row.key) == self.task_console.focused.as_ref())
            {
                let previous_index = previous
                    .iter()
                    .position(|key| Some(key) == self.task_console.focused.as_ref())
                    .unwrap_or(0);
                let next = surviving
                    .get(previous_index.min(surviving.len().saturating_sub(1)))
                    .map(|row| row.key.clone());
                self.task_console.focus_main = next.is_none();
                self.task_console.focused = next;
                // Main remains a navigation target until explicit activation or exit.
            }
        }
        let mut changed = false;
        for item in &mut self.items {
            if let super::Item::Tool { view, .. } = item
                && view.completed_agent_label.is_none()
                && let Some(job) = &view.completed_job
            {
                let mut owners = self.task_console.records.iter().filter(|row| {
                    row.kind == crate::task_console::TaskKind::Child
                        && row.job.as_ref() == Some(job)
                });
                let first = owners.next();
                let label = first
                    .filter(|_| owners.next().is_none())
                    .filter(|row| {
                        matches!(
                            row.status,
                            crate::task_console::TaskStatus::Completed
                                | crate::task_console::TaskStatus::Idle
                        )
                    })
                    .map(|row| row.label.clone());
                if view.completed_agent_label != label {
                    view.completed_agent_label = label;
                    changed = true;
                }
            }
            if let super::Item::Tool {
                call_id: Some(id),
                view,
                ..
            } = item
            {
                let children = self
                    .task_console
                    .records
                    .iter()
                    .filter(|row| {
                        row.kind == crate::task_console::TaskKind::Child
                            && row.telemetry.spawn_call_id.as_deref() == Some(id.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if view.spawn_children != children {
                    view.spawn_children = children;
                    changed = true;
                }
            }
        }
        if changed {
            self.tool_group_signature = None;
            self.refresh_tool_groups();
        }
    }

    /// Expand the task list through the shell's single panel claim boundary.
    pub fn open_tasks(&mut self) {
        if self.high_priority_modal_open() {
            return;
        }
        if self.task_console.active || self.claim_panel_surface() {
            self.side_panel = None;
            self.task_console.refresh();
            self.task_console.focused = self.task_console.selected.clone();
            self.task_console.focus_main = !self.task_console.active;
            self.task_console.strip_focused = false;
            self.task_console.view = ConsoleView::List;
        }
    }

    /// Open a domain-specific selector from the bottom navigation.
    pub fn open_task_category(&mut self, category: TaskCategory) {
        self.task_console.restore_preview();
        if category == TaskCategory::Jobs && self.task_console.active {
            self.return_from_task();
        }
        self.open_tasks();
        if self.task_console.view == ConsoleView::List {
            self.task_console.set_category(category);
        }
    }

    /// Open a specific task, preserving the entire parent TextArea (including
    /// undo, cursor and multiline content). Parent queued commands and inbox
    /// submissions keep their original owner and are never re-routed.
    pub fn open_task(&mut self, key: TaskKey) {
        if self.high_priority_modal_open() {
            return;
        }
        let Some(kind) = self
            .task_console
            .records
            .iter()
            .find(|record| record.key == key)
            .map(|record| record.kind)
        else {
            return;
        };
        self.return_from_task();
        if self.task_console.view == ConsoleView::Collapsed && !self.claim_panel_surface() {
            return;
        }
        if kind == crate::task_console::TaskKind::Work {
            self.task_console.inspect(key);
            self.ctrl_c_seen = false;
            self.screen_selection.clear();
            return;
        }
        self.task_console.preview = false;
        self.task_console.foregrounded.insert(key.clone());
        self.task_console.selected = Some(key.clone());
        self.parent_task_draft = Some(ParentTaskDraft {
            input: std::mem::take(&mut self.input),
            history_cursor: self.history_cursor.take(),
        });
        self.input = self.task_console.drafts.remove(&key).unwrap_or_default();
        self.task_console.open_selected();
        if let Some((scroll, follow, before, stack)) =
            self.task_console.positions.get(&key).cloned()
        {
            self.task_console.output_scroll = scroll;
            self.task_console.follow = follow;
            self.task_console.before = before;
            self.task_console.page_stack = stack;
            self.task_console.read_page();
        }
        self.ctrl_c_seen = false;
        self.screen_selection.clear();
    }

    /// Return to the parent without consuming or reconstructing either draft.
    pub fn return_from_task(&mut self) {
        self.task_console.restore_preview();
        self.workflow_console.opened_from_workflow = None;
        if let Some(parent) = self.parent_task_draft.take() {
            if let Some(key) = self.task_console.selected.clone() {
                self.task_console.positions.insert(
                    key.clone(),
                    (
                        self.task_console.output_scroll,
                        self.task_console.follow,
                        self.task_console.before,
                        self.task_console.page_stack.clone(),
                    ),
                );
                self.task_console
                    .drafts
                    .insert(key, std::mem::take(&mut self.input));
            }
            self.input = parent.input;
            self.history_cursor = parent.history_cursor;
        }
        self.task_console.view = ConsoleView::Collapsed;
        self.task_console.strip_focused = false;
        self.task_console.active = false;
        self.task_console.preview = false;
        self.task_console.focus_main = true;
        self.ctrl_c_seen = false;
    }

    /// Restore an unsent task message after an action failure, without touching
    /// the parent or overwriting a newer draft typed while the owner replied.
    pub fn restore_failed_task_message(&mut self, key: TaskKey, text: String) {
        let input = if self.task_console.active && self.task_console.selected.as_ref() == Some(&key)
        {
            &mut self.input
        } else {
            self.task_console.drafts.entry(key.clone()).or_default()
        };
        let newer = input.lines().join("\n");
        let restored = if newer.is_empty() {
            text
        } else {
            format!("{text}\n\n{newer}")
        };
        *input = tui_textarea::TextArea::new(restored.lines().map(str::to_owned).collect());
        self.task_console.set_notice(
            key,
            "Message failed. Unsent text restored to this task's draft.".into(),
        );
    }

    pub(super) fn close_tasks(&mut self) {
        self.return_from_task();
        self.task_console.view = ConsoleView::Collapsed;
        self.task_console.hits.clear();
    }

    fn dismiss_task_selector(&mut self) {
        self.task_console.view = if self.task_console.active {
            ConsoleView::Detail
        } else {
            ConsoleView::Collapsed
        };
        self.task_console.strip_focused = false;
    }

    fn task_action(&mut self, hit: TaskHit) {
        match hit {
            TaskHit::Category(category) => self.open_task_category(category),
            TaskHit::ReviewIssue => {
                if let Some(key) = self.task_console.first_unread_issue() {
                    self.task_console.inspect(key);
                }
            }
            TaskHit::DismissIssue => {
                if let Some(key) = self.task_console.selected.clone() {
                    self.task_console.dismiss_issue(&key);
                }
            }
            TaskHit::Main => {
                self.task_console.strip_focused = false;
                self.close_tasks();
            }
            TaskHit::ExpandOutput => {
                self.task_console.expanded_output = !self.task_console.expanded_output
            }
            TaskHit::Raw => {
                self.task_console.raw_output = !self.task_console.raw_output;
                self.task_console.output_scroll = 0;
            }
            TaskHit::ToolDetails(id) => {
                if let Some(key) = self.task_console.selected.clone() {
                    let token = (key, id);
                    if !self.task_console.expanded_tools.remove(&token) {
                        self.task_console.expanded_tools.insert(token);
                    }
                }
            }
            TaskHit::Toggle => {
                if self.task_console.view == ConsoleView::List
                    && self.task_console.category == TaskCategory::All
                {
                    self.dismiss_task_selector();
                } else {
                    self.open_task_category(TaskCategory::All);
                }
            }
            TaskHit::Open(key) => {
                if self.task_console.records.iter().any(|row| {
                    row.key == key
                        && matches!(
                            row.kind,
                            crate::task_console::TaskKind::Child
                                | crate::task_console::TaskKind::Job
                                | crate::task_console::TaskKind::Work
                        )
                }) {
                    self.task_console.inspect(key);
                } else {
                    self.open_task(key);
                }
            }
            TaskHit::Switch(key) => {
                self.task_console.strip_focused = false;
                self.open_task(key);
            }
            TaskHit::Foreground => {
                let key = self.task_console.selected.clone();
                self.task_console.restore_preview();
                if let Some(key) = key {
                    self.open_task(key);
                }
            }
            TaskHit::Back => {
                if self.task_console.preview {
                    self.task_console.restore_preview();
                } else {
                    self.close_tasks();
                }
            }
            TaskHit::Older => self.task_console.older_page(),
            TaskHit::Follow => self.task_console.follow_latest(),
            TaskHit::Metadata => {
                self.task_console.show_metadata = !self.task_console.show_metadata;
                self.task_console.output_scroll = 0;
            }
            TaskHit::Channel => {
                self.task_console.channel = self.task_console.channel.next();
                self.task_console.follow_latest();
            }
            action => {
                if let Some(key) = self.task_console.selected.clone() {
                    let action = match action {
                        TaskHit::Interrupt => TaskAction::Interrupt(key),
                        TaskHit::Retry => TaskAction::Retry(key),
                        TaskHit::Close => TaskAction::Close(key),
                        TaskHit::Background => TaskAction::Background(key),
                        _ => return,
                    };
                    self.task_console.queue(action);
                }
            }
        }
    }

    /// True only when a task action consumes the event. High-priority approval,
    /// trust, question and secret surfaces retain exclusive input ownership.
    pub(super) fn handle_task_terminal_event(&mut self, event: &Event) -> bool {
        if self.high_priority_modal_open() {
            return false;
        }
        if (self.task_console.preview || self.task_console.view == ConsoleView::List)
            && matches!(event, Event::Paste(_))
        {
            return true;
        }
        if let Event::Key(key) = event
            && self.task_console.preview
            && key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('c')
        {
            self.task_console.restore_preview();
            return true;
        }
        if let Event::Mouse(mouse) = event {
            let point = Position::new(mouse.column, mouse.row);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, hit)) = self
                    .task_console
                    .hits
                    .iter()
                    .rev()
                    .find(|(area, _)| area.contains(point))
            {
                let hit = hit.clone();
                self.task_action(hit);
                return true;
            }
            if (self.task_console.active || self.task_console.preview)
                && self.transcript_area.contains(point)
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.task_console.follow = false;
                        self.task_console.output_scroll = if self.task_console.show_metadata {
                            self.task_console.output_scroll.saturating_sub(3)
                        } else {
                            self.task_console.output_scroll.saturating_add(3)
                        };
                    }
                    MouseEventKind::ScrollDown => {
                        self.task_console.output_scroll = if self.task_console.show_metadata {
                            self.task_console.output_scroll.saturating_add(3)
                        } else {
                            self.task_console.output_scroll.saturating_sub(3)
                        }
                    }
                    _ => return false,
                }
                return true;
            }
            return false;
        }
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && crate::terminal::chord(*key).and_then(|chord| self.keymap.action(chord))
                == Some(heycode_ui::keymap::KeymapAction::ToggleTasks)
        {
            self.task_action(TaskHit::Toggle);
            return true;
        }
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && key.modifiers.is_empty()
            && !self.task_console.preview
            && self.task_console.view != ConsoleView::List
        {
            let children = self
                .active_child_records()
                .into_iter()
                .map(|row| row.key.clone())
                .collect::<Vec<_>>();
            if !children.is_empty() || self.task_console.strip_focused {
                if self.task_console.strip_focused {
                    match key.code {
                        KeyCode::Up => {
                            if self.task_console.focus_main {
                                self.task_console.strip_focused = false;
                            } else {
                                let index = self
                                    .task_console
                                    .focused
                                    .as_ref()
                                    .and_then(|focused| {
                                        children.iter().position(|key| key == focused)
                                    })
                                    .unwrap_or(0);
                                if index == 0 {
                                    self.task_console.focus_main = true;
                                    self.task_console.focused = None;
                                } else {
                                    self.task_console.focused = Some(children[index - 1].clone());
                                }
                            }
                            return true;
                        }
                        KeyCode::Down => {
                            if children.is_empty() {
                                return true;
                            }
                            if self.task_console.focus_main {
                                self.task_console.focus_main = false;
                                self.task_console.focused = children.first().cloned();
                            } else {
                                let index = self
                                    .task_console
                                    .focused
                                    .as_ref()
                                    .and_then(|focused| {
                                        children.iter().position(|key| key == focused)
                                    })
                                    .unwrap_or(0);
                                self.task_console.focused =
                                    Some(children[(index + 1).min(children.len() - 1)].clone());
                            }
                            return true;
                        }
                        KeyCode::Enter => {
                            self.task_console.strip_focused = false;
                            if self.task_console.focus_main {
                                self.task_action(TaskHit::Main);
                            } else if let Some(key) = self.task_console.focused.clone() {
                                self.task_action(TaskHit::Switch(key));
                            }
                            return true;
                        }
                        KeyCode::Esc => {
                            self.task_console.strip_focused = false;
                            return true;
                        }
                        _ => self.task_console.strip_focused = false,
                    }
                } else if key.code == KeyCode::Down
                    && self.input.lines().iter().all(String::is_empty)
                {
                    self.task_console.strip_focused = true;
                    self.task_console.focused =
                        self.task_console.selected.clone().filter(|selected| {
                            self.task_console.active && children.contains(selected)
                        });
                    self.task_console.focus_main = self.task_console.focused.is_none();
                    return true;
                }
            }
        }
        if self.task_console.view == ConsoleView::Collapsed {
            return false;
        }
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if self.task_console.preview && key.modifiers == KeyModifiers::NONE {
            match key.code {
                KeyCode::Left => self.task_action(TaskHit::Back),
                KeyCode::Char('d') => self.task_action(TaskHit::DismissIssue),
                KeyCode::Char('r') => self.task_action(TaskHit::Retry),
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') => {
                    self.task_console.restore_preview()
                }
                KeyCode::Char('f')
                    if self.task_console.selected_record().is_some_and(|row| {
                        row.kind == crate::task_console::TaskKind::Child
                            || row.capabilities.terminal_input
                    }) =>
                {
                    self.task_action(TaskHit::Foreground)
                }
                KeyCode::Char('x') => self.task_action(TaskHit::Interrupt),
                KeyCode::Up | KeyCode::PageUp => {
                    if self
                        .task_console
                        .selected_record()
                        .is_some_and(|row| row.kind == crate::task_console::TaskKind::Child)
                    {
                        self.task_console.output_scroll =
                            self.task_console.output_scroll.saturating_sub(8);
                    } else {
                        self.task_console.follow = false;
                        self.task_console.output_scroll =
                            self.task_console.output_scroll.saturating_add(8);
                    }
                }
                KeyCode::Down | KeyCode::PageDown => {
                    if self
                        .task_console
                        .selected_record()
                        .is_some_and(|row| row.kind == crate::task_console::TaskKind::Child)
                    {
                        let maximum = self
                            .task_console
                            .output_rows
                            .saturating_sub(self.task_console.output_height);
                        self.task_console.output_scroll =
                            (self.task_console.output_scroll + 8).min(maximum);
                    } else {
                        self.task_console.output_scroll =
                            self.task_console.output_scroll.saturating_sub(8);
                        self.task_console.follow = self.task_console.output_scroll == 0;
                    }
                }
                _ => {}
            }
            return true;
        }
        if key.modifiers == KeyModifiers::ALT {
            let hit = match key.code {
                KeyCode::Char('i') => Some(TaskHit::Interrupt),
                KeyCode::Char('t') => Some(TaskHit::Retry),
                KeyCode::Char('x') => Some(TaskHit::Close),
                KeyCode::Char('b') => Some(TaskHit::Background),
                KeyCode::Char('o') => Some(TaskHit::Channel),
                KeyCode::Char('r') => Some(TaskHit::Raw),
                KeyCode::Char('f') => Some(TaskHit::ExpandOutput),
                KeyCode::Up | KeyCode::Down => {
                    let ids = &self.task_console.rendered_tools;
                    let current = self
                        .task_console
                        .tool_focus
                        .as_ref()
                        .and_then(|id| ids.iter().position(|other| other == id));
                    let next = match (key.code, current) {
                        (KeyCode::Up, Some(index)) => index.saturating_sub(1),
                        (KeyCode::Down, Some(index)) => {
                            (index + 1).min(ids.len().saturating_sub(1))
                        }
                        _ => 0,
                    };
                    self.task_console.tool_focus = ids.get(next).cloned();
                    return true;
                }
                KeyCode::Char('m') => Some(TaskHit::Metadata),
                KeyCode::Left => Some(TaskHit::Back),
                _ => None,
            };
            if let Some(hit) = hit {
                self.task_action(hit);
                return true;
            }
        }
        if key.code == KeyCode::Esc {
            if self.task_console.view == ConsoleView::Detail
                && self
                    .task_console
                    .selected_record()
                    .is_some_and(|row| row.kind == crate::task_console::TaskKind::Job)
            {
                self.task_action(TaskHit::Back);
            } else if self.task_console.view == ConsoleView::Detail
                && self
                    .task_console
                    .selected_record()
                    .is_some_and(|row| row.capabilities.interrupt)
            {
                self.task_action(TaskHit::Interrupt);
            } else if self.task_console.view == ConsoleView::Detail {
                self.return_from_task();
            } else {
                self.dismiss_task_selector();
            }
            return true;
        }
        if self.task_console.view == ConsoleView::List {
            match key.code {
                KeyCode::Left | KeyCode::Right => {
                    let categories = [
                        TaskCategory::Agents,
                        TaskCategory::Work,
                        TaskCategory::Jobs,
                        TaskCategory::Teams,
                        TaskCategory::All,
                    ];
                    let index = categories
                        .iter()
                        .position(|category| *category == self.task_console.category)
                        .unwrap_or(0);
                    let next = if key.code == KeyCode::Right {
                        (index + 1) % categories.len()
                    } else {
                        (index + categories.len() - 1) % categories.len()
                    };
                    self.task_console.set_category(categories[next]);
                }
                KeyCode::Up => {
                    if self.task_console.focus_main {
                        self.dismiss_task_selector();
                    } else if self
                        .task_console
                        .visible_records()
                        .iter()
                        .any(|row| row.kind == crate::task_console::TaskKind::Child)
                        && self
                            .task_console
                            .visible_records()
                            .first()
                            .is_some_and(|r| Some(&r.key) == self.task_console.focused.as_ref())
                    {
                        self.task_console.focus_main = true;
                    } else {
                        self.task_console.move_selection(false);
                    }
                }
                KeyCode::Down => {
                    if self.task_console.focus_main {
                        self.task_console.focus_main = false;
                        self.task_console.focused = self
                            .task_console
                            .visible_records()
                            .first()
                            .map(|r| r.key.clone());
                    } else {
                        self.task_console.move_selection(true);
                    }
                }
                KeyCode::Enter => {
                    if self.task_console.focus_main {
                        self.close_tasks();
                        return true;
                    }
                    if let Some(key) = self.task_console.focused.clone() {
                        self.task_action(TaskHit::Open(key));
                    }
                }
                // Ordinary editing returns focus to the parent composer.
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete => {
                    self.dismiss_task_selector();
                    return if self.task_console.active {
                        self.handle_task_detail_key(key)
                    } else {
                        false
                    };
                }
                _ => return false,
            }
            return true;
        }
        if self.task_console.show_metadata {
            let page = self.task_console.output_height.max(1);
            match key.code {
                KeyCode::PageUp => {
                    self.task_console.output_scroll =
                        self.task_console.output_scroll.saturating_sub(page)
                }
                KeyCode::PageDown => {
                    self.task_console.output_scroll =
                        self.task_console.output_scroll.saturating_add(page)
                }
                KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.task_console.output_scroll = 0
                }
                KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.task_console.output_scroll =
                        self.task_console.output_rows.saturating_sub(page)
                }
                _ => return self.handle_task_detail_key(key),
            }
            return true;
        }
        self.handle_task_detail_key(key)
    }

    fn handle_task_detail_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        if key.code == KeyCode::Up && key.modifiers.is_empty() && self.recall_pending_messages() {
            return true;
        }
        match key.code {
            KeyCode::PageUp => {
                let max = self
                    .task_console
                    .output_rows
                    .saturating_sub(self.task_console.output_height);
                if self.task_console.output_scroll >= max {
                    self.task_console.older_page();
                } else {
                    self.task_console.follow = false;
                    self.task_console.output_scroll = (self.task_console.output_scroll
                        + self.task_console.output_height.max(1))
                    .min(max);
                }
                return true;
            }
            KeyCode::PageDown => {
                if self.task_console.output_scroll == 0 {
                    self.task_console.newer_page();
                } else {
                    self.task_console.output_scroll = self
                        .task_console
                        .output_scroll
                        .saturating_sub(self.task_console.output_height.max(1));
                }
                return true;
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.task_console.follow_latest();
                return true;
            }
            KeyCode::Enter
                if key.modifiers.is_empty() && self.task_console.tool_focus.is_some() =>
            {
                if let Some(id) = self.task_console.tool_focus.clone() {
                    self.task_action(TaskHit::ToolDetails(id));
                }
                return true;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let text = self.input.lines().join("\n");
                if let Some(key) = self.task_console.selected.clone() {
                    let queued = self.task_console.actions.len();
                    let action = if self
                        .task_console
                        .selected_record()
                        .is_some_and(|row| row.capabilities.terminal_input)
                    {
                        TaskAction::TerminalInput { key, text }
                    } else {
                        TaskAction::Steer { key, text }
                    };
                    self.task_console.queue(action);
                    if self.task_console.actions.len() > queued {
                        self.input = tui_textarea::TextArea::default();
                        if let Some(key) = self.task_console.selected.clone() {
                            self.task_console.set_notice(key, "Sending…".into());
                        }
                    }
                }
                return true;
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                if self
                    .task_console
                    .selected_record()
                    .is_some_and(|row| row.capabilities.interrupt)
                {
                    self.task_action(TaskHit::Interrupt);
                } else if !self.input.lines().iter().all(String::is_empty) {
                    self.input = tui_textarea::TextArea::default();
                }
                return true;
            }
            _ => {}
        }
        self.task_console.tool_focus = None;
        // The child editor is intentionally independent from parent prompt
        // history, slash commands, approval-mode cycling and inbox shortcuts.
        if key.code == KeyCode::Enter {
            self.input.insert_newline();
        } else {
            let _ = self.input.input(tui_textarea::Input::from(*key));
        }
        true
    }
}
