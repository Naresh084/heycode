//! Workflow-specific focus and navigation; the parent composer stays owned.
use crate::{app::AppState, workflow_console::*};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Position;
use std::sync::Arc;

impl AppState {
    pub(crate) fn strip_task_records(&self) -> Vec<&crate::task_console::TaskRecord> {
        self.task_console
            .records
            .iter()
            .filter(|record| {
                record.kind != crate::task_console::TaskKind::Child || record.active_child()
            })
            .filter(|record| {
                self.workflow_console.runs.is_empty()
                    || (!self.workflow_console.task_keys.contains(&record.key.0)
                        && !(record.kind == crate::task_console::TaskKind::Tool
                            && record
                                .telemetry
                                .current_tool
                                .as_deref()
                                .is_some_and(|name| {
                                    name.strip_prefix("mcp__heycode__").unwrap_or(name)
                                        == "workflow"
                                })))
            })
            .collect()
    }
    pub(crate) fn task_strip_visible(&self) -> bool {
        !self.workflow_console.expanded()
            && self.task_console.attached()
            && (self.task_console.strip_focused
                || self.strip_task_records().iter().any(|row| {
                    matches!(
                        row.kind,
                        crate::task_console::TaskKind::Child
                            | crate::task_console::TaskKind::Team
                            | crate::task_console::TaskKind::Work
                    )
                }))
    }
    pub(crate) fn task_strip_summary(&self) -> String {
        use crate::task_console::TaskCategory;
        let rows = self.strip_task_records();
        [
            TaskCategory::Agents,
            TaskCategory::Work,
            TaskCategory::Jobs,
            TaskCategory::Teams,
        ]
        .into_iter()
        .filter_map(|category| {
            let matching = rows
                .iter()
                .filter(|row| category.contains(row.kind))
                .collect::<Vec<_>>();
            (!matching.is_empty()).then(|| {
                format!(
                    "{} {} · {} active",
                    category.label(),
                    matching.len(),
                    matching.iter().filter(|row| row.status.active()).count()
                )
            })
        })
        .collect::<Vec<_>>()
        .join(" | ")
    }

    /// Attach the actual owner-backed workflow projection.
    pub fn set_workflow_source(&mut self, source: Arc<dyn WorkflowSource>) {
        self.workflow_console.attach(source);
        self.panel_commands
            .attach(crate::panel_commands::CapabilityPanel::Workflows);
    }
    /// Read the current native workflow workspace state.
    #[must_use]
    pub const fn workflow_console(&self) -> &WorkflowConsole {
        &self.workflow_console
    }
    /// Open the workflow workspace while preserving the conversation editor.
    pub fn open_workflows(&mut self) {
        if self.claim_panel_surface() {
            self.workflow_console.refresh();
            if let Some(phase) = self.workflow_console.phase().map(|phase| phase.id.clone()) {
                self.workflow_console.select_phase(phase);
            }
            self.workflow_console.view = WorkflowView::Workspace;
        }
    }
    pub(super) fn close_workflows(&mut self) {
        self.workflow_console.view = WorkflowView::Collapsed;
        self.workflow_console.rail_focused = false;
        self.workflow_console.hits.clear();
        self.workflow_console.opened_from_workflow = None;
    }
    /// Poll committed workflow state and owner acknowledgements.
    pub fn refresh_workflows(&mut self) {
        self.workflow_console.refresh();
    }
    pub(crate) fn workflow_toggle_label(&self) -> String {
        self.keymap
            .chord(heycode_ui::keymap::KeymapAction::ToggleWorkflows)
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
    fn workflow_back(&mut self) {
        self.workflow_console.rail_focused = false;
        self.workflow_console.view = match self.workflow_console.view {
            WorkflowView::Agent | WorkflowView::Picker => WorkflowView::Workspace,
            _ => WorkflowView::Collapsed,
        };
    }
    fn return_to_workflow(&mut self) {
        let Some((run, agent)) = self.workflow_console.opened_from_workflow.take() else {
            return;
        };
        self.return_from_task();
        self.close_tasks();
        self.workflow_console.select_run(run);
        self.workflow_console.select_agent(agent, true);
    }
    fn workflow_hit(&mut self, hit: WorkflowHit) {
        match hit {
            WorkflowHit::Toggle => {
                if self.workflow_console.view == WorkflowView::Picker {
                    self.workflow_console.view = WorkflowView::Workspace;
                } else if self.workflow_console.expanded() {
                    self.close_workflows();
                } else {
                    self.open_workflows();
                }
            }
            WorkflowHit::Picker => self.workflow_console.view = WorkflowView::Picker,
            WorkflowHit::PickRun(id) => self.workflow_console.select_run(id),
            WorkflowHit::Phase(id) => self.workflow_console.select_phase(id),
            WorkflowHit::Agent(id) => self.workflow_console.select_agent(id, true),
            WorkflowHit::Back => self.workflow_back(),
            WorkflowHit::Action(kind) => self.workflow_console.request(kind),
            WorkflowHit::Conversation => {
                if let (Some(run), Some(agent)) = (
                    self.workflow_console.selected_run.clone(),
                    self.workflow_console
                        .agent()
                        .map(|agent| agent.task_id.clone()),
                ) {
                    let key = crate::task_console::TaskKey(format!("child:{agent}"));
                    self.refresh_tasks();
                    if self
                        .task_console
                        .records
                        .iter()
                        .any(|record| record.key == key)
                    {
                        self.close_workflows();
                        self.open_task(key);
                        self.workflow_console.opened_from_workflow = Some((run, agent));
                    } else {
                        self.workflow_console.notice = Some("This historical conversation is no longer retained; its summary remains here.".into());
                    }
                }
            }
            WorkflowHit::ScrollUp => self
                .workflow_console
                .set_scroll(self.workflow_console.scroll().saturating_sub(3)),
            WorkflowHit::ScrollDown => self
                .workflow_console
                .set_scroll(self.workflow_console.scroll().saturating_add(3)),
        }
    }
    pub(super) fn handle_workflow_event(&mut self, event: &Event) -> bool {
        if self.high_priority_modal_open()
            || self.pending_plan_review.is_some()
            || self.pending_mcp_elicitation.is_some()
        {
            return false;
        }
        if matches!(event, Event::Paste(_))
            && (self.workflow_console.expanded() || self.workflow_console.rail_focused)
        {
            return true;
        }
        if self.workflow_console.opened_from_workflow.is_some()
            && self.task_console.view == crate::task_console::ConsoleView::Detail
        {
            let back = match event {
                Event::Key(key) => key.kind == KeyEventKind::Press && key.code == KeyCode::Esc,
                Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                    self.task_console.hits.iter().any(|(area, hit)| {
                        matches!(hit, crate::task_console::TaskHit::Back)
                            && area.contains(Position::new(mouse.column, mouse.row))
                    })
                }
                _ => false,
            };
            if back {
                self.return_to_workflow();
                return true;
            }
        }
        if let Event::Mouse(mouse) = event {
            let point = Position::new(mouse.column, mouse.row);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, hit)) = self
                    .workflow_console
                    .hits
                    .iter()
                    .rev()
                    .find(|(area, _)| area.contains(point))
            {
                self.workflow_hit(hit.clone());
                return true;
            }
            if self.workflow_console.expanded() && self.workflow_console.body.contains(point) {
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let forward = mouse.kind == MouseEventKind::ScrollDown;
                        if self.workflow_console.view == WorkflowView::Agent {
                            self.workflow_hit(if forward {
                                WorkflowHit::ScrollDown
                            } else {
                                WorkflowHit::ScrollUp
                            });
                        } else if self.workflow_console.focus == WorkflowFocus::Phases {
                            self.workflow_console.move_phase(forward);
                        } else {
                            self.workflow_console.move_agent(forward);
                        }
                        return true;
                    }
                    _ => {}
                }
            }
        }
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind != KeyEventKind::Press {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return false;
        }
        if crate::terminal::chord(*key).and_then(|chord| self.keymap.action(chord))
            == Some(heycode_ui::keymap::KeymapAction::ToggleWorkflows)
        {
            self.workflow_hit(WorkflowHit::Toggle);
            return true;
        }
        if !self.workflow_console.expanded() {
            if self.workflow_console.rail_focused {
                match key.code {
                    KeyCode::Enter => self.open_workflows(),
                    KeyCode::Esc | KeyCode::Up => self.workflow_console.rail_focused = false,
                    KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => return false,
                    _ => {}
                }
                return true;
            }
            if key.code == KeyCode::Down
                && key.modifiers.is_empty()
                && !self.workflow_console.runs.is_empty()
                && self.task_console.view == crate::task_console::ConsoleView::Collapsed
                && self.input.cursor().0 + 1 == self.input.lines().len()
                && self.command_palette.is_none()
                && self.settings_panel().is_none()
                && self.theme_picker.is_none()
                && self.keymap_picker.is_none()
                && self.profile_picker.is_none()
                && self.session_browser.is_none()
                && self.permission_picker().is_none()
                && self.model_picker.is_none()
                && self.effort_picker.is_none()
                && self.route_picker.is_none()
                && self.capability_catalog().is_none()
                && self.plugin_panel().is_none()
                && self.mcp_panel().is_none()
            {
                self.workflow_console.rail_focused = true;
                return true;
            }
            return false;
        }
        // The task console retains its independent configured shortcut.
        if crate::terminal::chord(*key).and_then(|chord| self.keymap.action(chord))
            == Some(heycode_ui::keymap::KeymapAction::ToggleTasks)
        {
            return false;
        }
        if key.code == KeyCode::Esc {
            self.workflow_back();
            return true;
        }
        if self.workflow_console.view == WorkflowView::Picker {
            match key.code {
                KeyCode::Up | KeyCode::Down => {
                    let index = self
                        .workflow_console
                        .runs
                        .iter()
                        .position(|run| {
                            Some(&run.id) == self.workflow_console.selected_run.as_ref()
                        })
                        .unwrap_or(0);
                    let next = if key.code == KeyCode::Down {
                        (index + 1).min(self.workflow_console.runs.len().saturating_sub(1))
                    } else {
                        index.saturating_sub(1)
                    };
                    if let Some(run) = self.workflow_console.runs.get(next) {
                        self.workflow_console.selected_run = Some(run.id.clone());
                    }
                }
                KeyCode::Enter => self.workflow_console.view = WorkflowView::Workspace,
                _ => {}
            }
            return true;
        }
        if self.workflow_console.view == WorkflowView::Agent {
            let page = self.workflow_console.detail_height.max(1);
            match key.code {
                KeyCode::Char('c') if key.modifiers.is_empty() => {
                    self.workflow_hit(WorkflowHit::Conversation)
                }
                KeyCode::Up => self
                    .workflow_console
                    .set_scroll(self.workflow_console.scroll().saturating_sub(1)),
                KeyCode::Down => self
                    .workflow_console
                    .set_scroll(self.workflow_console.scroll().saturating_add(1)),
                KeyCode::PageUp => self
                    .workflow_console
                    .set_scroll(self.workflow_console.scroll().saturating_sub(page)),
                KeyCode::PageDown => self
                    .workflow_console
                    .set_scroll(self.workflow_console.scroll().saturating_add(page)),
                KeyCode::Home => self.workflow_console.set_scroll(0),
                KeyCode::End => self.workflow_console.set_scroll(usize::MAX),
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.workflow_console.focus =
                    if self.workflow_console.focus == WorkflowFocus::Phases {
                        WorkflowFocus::Agents
                    } else {
                        WorkflowFocus::Phases
                    }
            }
            KeyCode::Left => self.workflow_console.move_phase(false),
            KeyCode::Right => self.workflow_console.move_phase(true),
            KeyCode::Up | KeyCode::Down => {
                let forward = key.code == KeyCode::Down;
                if self.workflow_console.focus == WorkflowFocus::Phases {
                    self.workflow_console.move_phase(forward);
                } else {
                    self.workflow_console.move_agent(forward);
                }
            }
            KeyCode::Enter => {
                if self.workflow_console.focus == WorkflowFocus::Phases {
                    self.workflow_console.focus = WorkflowFocus::Agents;
                } else if let Some(agent) = self.workflow_console.agent() {
                    self.workflow_console
                        .select_agent(agent.task_id.clone(), true);
                }
            }
            KeyCode::Char('p') if key.modifiers.is_empty() => {
                self.workflow_console.request(WorkflowActionKind::Pause)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.workflow_console.request(WorkflowActionKind::Resume)
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                self.workflow_console.request(WorkflowActionKind::Stop)
            }
            KeyCode::Char('w') if key.modifiers.is_empty() => {
                self.workflow_console.view = WorkflowView::Picker
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => return false,
            _ => {}
        }
        true
    }
}
