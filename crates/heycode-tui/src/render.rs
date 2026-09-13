//! Frame rendering: transcript with per-tool views, rounded input, status.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use heycode_ui::preferences::HeaderDensity;
use heycode_ui::theme::ThemeRole;

use crate::app::{AppState, Item, format_scroll_speed};
use crate::markdown;
use crate::panel_frame;

/// Draw one frame. Pure function of `state` (+ terminal size).
pub fn draw(frame: &mut Frame<'_>, state: &mut AppState) {
    state.refresh_tool_groups();
    state.reasoning_hit_rows.clear();
    state.mascot_hitbox = Rect::default();
    state.task_console.hits.clear();
    state.workflow_console.hits.clear();
    state.transcript_area = Rect::default();
    draw_shell(frame, state);
    state.screen_selection.paint(frame.buffer_mut());
}

fn draw_shell(frame: &mut Frame<'_>, state: &mut AppState) {
    let area = frame.area();
    if state.workspace_trust_is_foreground() {
        draw_workspace_trust(frame, state, area);
        return;
    }
    if state.pending_secret.is_some() {
        draw_secret_prompt(frame, state, area);
        return;
    }
    // Mandatory first-run setup owns the viewport; `/login` and `/connect`
    // reopen the same wizard as a bottom panel over the live shell.
    if state.onboarding_is_fullscreen() {
        draw_onboarding(frame, state, area);
        return;
    }
    if let Some(view) = state.pending_plan_review.as_mut() {
        crate::plan_review::draw(frame, view, area);
        return;
    }
    if crate::task_render::background_open(state)
        && state.pending_ask.is_none()
        && state.pending_runtime_question.is_none()
        && state.pending_mcp_elicitation.is_none()
        && state.pending_command_confirmation.is_none()
    {
        let height = crate::task_render::background_height(state, area.height);
        let header_height = if area.height >= 16 { 5 } else { 1 };
        let [header, transcript, panel] = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(height),
        ])
        .areas(area);
        draw_header(frame, state, header);
        let inspected = state.task_console.preview_underlay.clone().map(|underlay| {
            let inspected = state.task_console.capture_view();
            state.task_console.apply_view(underlay);
            inspected
        });
        if state.task_console.active {
            crate::task_render::draw_output(frame, state, transcript);
        } else {
            draw_transcript_with_side_panel(frame, state, transcript);
        }
        if let Some(inspected) = inspected {
            state.task_console.apply_view(inspected);
        }
        state.task_console.hits.clear();
        crate::task_render::draw_background(frame, state, panel);
        return;
    }
    if state.settings_panel().is_some()
        && state.pending_ask.is_none()
        && state.pending_runtime_question.is_none()
        && state.pending_mcp_elicitation.is_none()
        && state.pending_command_confirmation.is_none()
    {
        let header_height = if state.focus_view() {
            0
        } else if area.height >= 16 {
            5
        } else {
            1
        };
        let [header, panel] =
            Layout::vertical([Constraint::Length(header_height), Constraint::Min(1)]).areas(area);
        draw_header(frame, state, header);
        draw_settings_panel(frame, state, panel);
        return;
    }
    if state.detailed_transcript() && inline_surface_height(state, area.width) == 0 {
        let caption = format!(
            " Showing detailed transcript · {} to toggle · ↑↓ scroll · ? for shortcuts",
            state.transcript_toggle_hint()
        );
        let mut footer = markdown::wrap_styled(
            &[Span::styled(
                caption,
                Style::default().fg(state.styles().dim()),
            )],
            usize::from(area.width.max(1)),
            0,
        );
        if footer.len() == 1 && footer[0].width().saturating_add(10) <= usize::from(area.width) {
            let padding = usize::from(area.width).saturating_sub(footer[0].width() + 8);
            footer[0].spans.push(Span::styled(
                format!("{}verbose ", " ".repeat(padding)),
                Style::default().fg(state.styles().dim()),
            ));
        }
        if state.shortcut_list_visible() {
            use heycode_ui::keymap::KeymapAction;
            let key = |action| state.keymap().chord(action).to_string();
            let shortcuts = format!(
                " ↑/↓ scroll · {}/{} page · {}/{} oldest/newest · esc close · ? hide shortcuts",
                key(KeymapAction::ScrollPageUp),
                key(KeymapAction::ScrollPageDown),
                key(KeymapAction::ScrollToOldest),
                key(KeymapAction::ScrollToNewest),
            );
            footer.extend(markdown::wrap_styled(
                &[Span::styled(
                    shortcuts,
                    Style::default().fg(state.styles().dim()),
                )],
                usize::from(area.width.max(1)),
                0,
            ));
        }
        let height = u16::try_from(footer.len() + 1)
            .unwrap_or(u16::MAX)
            .min(area.height);
        let [transcript, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(height)]).areas(area);
        draw_transcript_with_side_panel(frame, state, transcript);
        footer.insert(
            0,
            Line::styled(
                "─".repeat(usize::from(area.width)),
                Style::default().fg(state.styles().border()),
            ),
        );
        frame.render_widget(Paragraph::new(footer), controls);
        return;
    }
    let preferences = state.shell_preferences();
    let requested_surface_height = inline_surface_height(state, area.width);
    let workflow_open = state.workflow_console.expanded();
    let header_height = if state.focus_view() {
        0
    } else if workflow_open {
        u16::from(area.height >= 8)
    } else {
        match preferences.header_density() {
            HeaderDensity::Full
                if area.height >= 16
                    && (requested_surface_height == 0
                        || area.height >= requested_surface_height.saturating_add(18)) =>
            {
                5
            }
            HeaderDensity::Full | HeaderDensity::Compact if area.height >= 5 => 1,
            HeaderDensity::Full | HeaderDensity::Compact => 0,
        }
    };
    let status_height = u16::from(
        !workflow_open
            && !panel_frame::takes_over_composer(state)
            && state.pending_ask.is_none()
            && state.help_panel.is_none()
            && state.skills_panel().is_none()
            && state.effort_picker.is_none()
            && state.model_picker.is_none()
            && state.rewind_picker.is_none()
            && state.plugin_panel().is_none()
            && state.advisor_panel.is_none()
            && state.memory_panel.is_none()
            && state.skill_doctor_panel.is_none()
            && state.copy_panel.is_none()
            && state.add_directory_dialog.is_none()
            && preferences.footer_status()
            && area.height >= 7,
    ) * if area.width < 100
        && area.height >= 12
        && status_fields(state)
            .iter()
            .any(|field| field.priority == 90)
    {
        2
    } else {
        1
    };
    let controls_height = u16::from(
        !workflow_open
            && !panel_frame::takes_over_composer(state)
            && state.pending_ask.is_none()
            && state.help_panel.is_none()
            && state.skills_panel().is_none()
            && state.effort_picker.is_none()
            && state.model_picker.is_none()
            && state.rewind_picker.is_none()
            && state.plugin_panel().is_none()
            && state.advisor_panel.is_none()
            && state.memory_panel.is_none()
            && state.skill_doctor_panel.is_none()
            && state.copy_panel.is_none()
            && state.add_directory_dialog.is_none()
            && preferences.footer_hints()
            && area.height >= 9,
    );
    // A standalone `?` swaps the one footer line for the shortcut list, so the
    // list is sized here and the composer moves up by exactly its extra rows.
    let controls_height = if controls_height > 0 && state.shortcut_list_visible() {
        state
            .shortcut_list()
            .height(area.width)
            .min(area.height.saturating_sub(7).max(1))
    } else {
        controls_height
    };
    let workflow_rail_height = crate::workflow_render::rail_height(state, area.height);
    let task_strip_height = crate::task_render::foreground_strip_height(state, area.height);
    let copy_height = u16::from(state.copy_notice_text().is_some() && area.height >= 8);
    let activity_height = activity_panel_height(state, area);
    let queued_lines = queued_message_lines(state, area.width);
    let queued_height = if workflow_open || area.height < 12 {
        0
    } else {
        u16::try_from(queued_lines.len()).unwrap_or(u16::MAX).min(6)
    };
    let body_height = area.height.saturating_sub(
        header_height
            + activity_height
            + queued_height
            + copy_height
            + status_height
            + controls_height
            + task_strip_height
            + workflow_rail_height,
    );
    let transcript_min = if body_height >= 6 {
        3
    } else {
        u16::from(body_height > 1)
    };
    let input_height = if panel_frame::takes_over_composer(state)
        || state.pending_ask.is_some()
        || state.skills_panel().is_some()
        || state.plugin_panel().is_some()
        || state.effort_picker.is_some()
        || state.model_picker.is_some()
        || state.rewind_picker.is_some()
        || state.pending_runtime_question.is_some()
        || state.optional_question_panel_visible()
        || state.help_panel.is_some()
        || state.memory_panel.is_some()
        || state.skill_doctor_panel.is_some()
        || state.copy_panel.is_some()
        || state.add_directory_dialog.is_some()
        || workflow_open
    {
        0
    } else {
        input_height(state, area)
            .min(body_height.saturating_sub(transcript_min))
            .max(u16::from(body_height > 0))
    };
    let surface_height = requested_surface_height.min(
        body_height
            .saturating_sub(input_height)
            .saturating_sub(transcript_min),
    );
    let palette_above_input = state.command_palette().is_some();
    let agent_surface = !palette_above_input
        && state.task_console.view != crate::task_console::ConsoleView::Collapsed
        && state.task_console.category != crate::task_console::TaskCategory::Jobs;
    let [
        header,
        transcript,
        activity,
        queued,
        copy_notice,
        palette_surface,
        input,
        status,
        controls,
        control_surface,
        task_strip,
        agent_surface_area,
        workflow_rail,
    ] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(transcript_min),
        Constraint::Length(activity_height),
        Constraint::Length(queued_height),
        Constraint::Length(copy_height),
        Constraint::Length(if palette_above_input {
            surface_height
        } else {
            0
        }),
        Constraint::Length(input_height),
        Constraint::Length(status_height),
        Constraint::Length(controls_height),
        Constraint::Length(if agent_surface || palette_above_input {
            0
        } else {
            surface_height
        }),
        Constraint::Length(task_strip_height),
        Constraint::Length(if agent_surface { surface_height } else { 0 }),
        Constraint::Length(workflow_rail_height),
    ])
    .areas(area);
    let surface = if palette_above_input {
        palette_surface
    } else if agent_surface {
        agent_surface_area
    } else {
        control_surface
    };
    draw_header(frame, state, header);
    let effort = state
        .reasoning_effort
        .as_deref()
        .filter(|_| requested_surface_height == 0);
    let effort_width = effort.map_or(0, |value| {
        u16::try_from(value.width() + 15).unwrap_or(u16::MAX)
    });
    let show_effort = effort_width > 0 && activity.width >= effort_width + 32;
    let mut activity_body = activity;
    if show_effort {
        activity_body.width = activity_body.width.saturating_sub(effort_width);
    }
    if state.has_active_turn()
        || state.side_command_activity().is_some()
        || latest_todos(state).is_some()
        || state.task_console.active
        || !queued_lines.is_empty()
    {
        draw_activity_panel(frame, state, activity_body);
    }
    state.optional_questions.badge_area =
        if queued.height > 0 && !state.optional_questions.rows.is_empty() {
            Some(Rect::new(queued.x, queued.y, queued.width, 1))
        } else {
            None
        };
    if queued.height > 0 {
        frame.render_widget(
            Paragraph::new(
                queued_lines
                    .into_iter()
                    .take(usize::from(queued.height))
                    .collect::<Vec<_>>(),
            ),
            queued,
        );
    }
    if show_effort && activity.height > 0 {
        frame.render_widget(
            Paragraph::new(format!("● {} · /effort ", effort.unwrap_or_default()))
                .style(Style::default().fg(state.styles().dim()))
                .alignment(Alignment::Right),
            Rect::new(activity.right() - effort_width, activity.y, effort_width, 1),
        );
    }
    if workflow_open {
        crate::workflow_render::draw_workspace(frame, state, transcript);
    } else if state.task_console.view == crate::task_console::ConsoleView::List
        && state.task_console.category == crate::task_console::TaskCategory::Jobs
    {
        crate::task_render::draw_jobs_list(frame, state, transcript);
    } else if state.task_console.active
        && state.task_console.selected_record().is_some_and(|row| {
            matches!(
                row.kind,
                crate::task_console::TaskKind::Child | crate::task_console::TaskKind::Job
            ) || state.task_console.expanded_output
        })
    {
        crate::task_render::draw_output(frame, state, transcript);
    } else {
        draw_transcript_with_side_panel(frame, state, transcript);
    }
    if let Some(message) = state.copy_notice_text() {
        frame.render_widget(
            Paragraph::new(format!("{message} "))
                .style(Style::default().fg(state.styles().dim()))
                .alignment(Alignment::Right),
            copy_notice,
        );
    }
    if state.pending_runtime_question.is_none() && !state.optional_question_panel_visible() {
        draw_input(frame, state, input);
    }
    crate::workflow_render::draw_rail(frame, state, workflow_rail);
    crate::task_render::draw_strip(frame, state, task_strip);
    draw_status(frame, state, status);
    draw_controls(frame, state, controls);
    if state.onboarding_is_panel() {
        draw_onboarding(frame, state, surface);
    } else if state.pending_mcp_elicitation.is_some() {
        draw_mcp_elicitation(frame, state, surface);
    } else if state.pending_runtime_question.is_some() {
        draw_runtime_question(frame, state, surface);
    } else if state.pending_ask.is_some() {
        draw_ask_card(frame, state, surface);
    } else if state.optional_question_panel_visible() {
        crate::app::optional_questions::draw(frame, state, surface);
    } else if state.pending_command_confirmation.is_some() {
        draw_command_confirmation(frame, state, surface);
    } else if let Some(dialog) = state.add_directory_dialog.as_ref() {
        crate::add_directory::draw(frame, surface, dialog);
    } else if let Some(picker) = state.rewind_picker.as_ref() {
        picker.draw(frame, surface, &state.styles());
    } else if let Some(panel) = state.advisor_panel.as_ref() {
        crate::advisor_panel::draw(frame, panel, surface);
    } else if state.theme_picker().is_some() {
        draw_theme_picker(frame, state, surface);
    } else if state.scroll_speed_picker().is_some() {
        draw_scroll_speed_picker(frame, state, surface);
    } else if state.help_panel.is_some() {
        let styles = state.styles();
        if let Some(help) = state.help_panel.as_mut() {
            help.draw(frame, surface, styles);
        }
    } else if state.copy_panel.is_some() {
        let styles = state.styles();
        if let Some(panel) = state.copy_panel.as_mut() {
            panel.draw(frame, surface, styles);
        }
    } else if let Some(progress) = state.export_progress.as_ref() {
        let styles = state.styles();
        progress.draw(
            frame,
            surface,
            crate::export_panel::ExportPanelStyles::new(
                styles.text(),
                styles.dim(),
                styles.accent(),
            ),
        );
    } else if state.export_panel.is_some() {
        let styles = state.styles();
        if let Some(panel) = state.export_panel.as_mut() {
            panel.draw(
                frame,
                surface,
                crate::export_panel::ExportPanelStyles::new(
                    styles.text(),
                    styles.dim(),
                    styles.accent(),
                ),
            );
        }
    } else if state.skill_doctor_panel.is_some() {
        let styles = state.styles();
        if let Some(panel) = state.skill_doctor_panel.as_mut() {
            panel.draw(frame, surface, styles);
        }
    } else if state.memory_panel.is_some() {
        let styles = state.styles();
        if let Some(panel) = state.memory_panel.as_mut() {
            panel.draw(frame, surface, styles);
        }
    } else if state.keymap_picker().is_some() {
        draw_keymap_picker(frame, state, surface);
    } else if state.profile_picker().is_some() {
        draw_profile_picker(frame, state, surface);
    } else if state.session_browser().is_some() {
        draw_session_browser(frame, state, surface);
    } else if let Some(panel) = state.sandbox_panel() {
        panel_frame::render(
            frame,
            surface,
            state.styles(),
            panel.lines(surface.width, state.styles()),
        );
    } else if let Some(panel) = state.autocompact_panel() {
        panel_frame::render(
            frame,
            surface,
            state.styles(),
            panel.lines(surface.width, state.styles()),
        );
    } else if state.permission_picker().is_some() {
        draw_permission_picker(frame, state, surface);
    } else if state.route_picker.is_some() {
        draw_route_picker(frame, state, surface);
    } else if state.skills_panel().is_some() {
        draw_skills_panel(frame, state, surface);
    } else if state.capability_catalog().is_some() {
        draw_capability_catalog(frame, state, surface);
    } else if state.settings_panel().is_some() {
        draw_settings_panel(frame, state, surface);
    } else if state.plugin_panel().is_some() {
        draw_plugin_panel(frame, state, surface);
    } else if state.mcp_panel().is_some() {
        draw_mcp_panel(frame, state, surface);
    } else if state.effort_picker.is_some() {
        draw_effort_picker(frame, state, surface);
    } else if state.model_picker.is_some() {
        draw_model_picker(frame, state, surface);
    } else if state.command_palette().is_some() {
        draw_command_palette(frame, state, surface);
    } else if state.task_console.view != crate::task_console::ConsoleView::Collapsed {
        crate::task_render::draw_surface(frame, state, surface);
    }
}

fn queued_message_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    let messages = state.pending_message_texts();
    let mut lines = Vec::new();
    if let Some(question) = state.optional_questions.current() {
        let label = format!(
            "  ◇ {} optional {} · Alt+Q to answer · {}",
            state.optional_questions.rows.len(),
            if state.optional_questions.rows.len() == 1 {
                "question"
            } else {
                "questions"
            },
            markdown::terminal_safe_span(&question.prompt)
        );
        lines.push(Line::styled(
            crate::terminal::truncate_to_width(&label, usize::from(width)),
            Style::default().fg(state.styles().accent()),
        ));
    }
    if messages.is_empty() {
        return lines;
    }
    for text in messages {
        let preview = text
            .lines()
            .map(crate::task_console::safe)
            .collect::<Vec<_>>()
            .join(" ");
        let mut line = String::from("  ❯ ");
        let mut used = 4;
        for ch in preview.chars() {
            let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + size + 1 > usize::from(width) {
                line.push('…');
                break;
            }
            line.push(ch);
            used += size;
        }
        lines.push(Line::styled(
            line,
            Style::default().fg(state.styles().text()),
        ));
    }
    lines.push(Line::default());
    lines
}

fn activity_panel_height(state: &AppState, area: Rect) -> u16 {
    if state.workflow_console.expanded() {
        return 0;
    }
    if area.height < 8 {
        return 0;
    }
    if state.task_console.active {
        return u16::from(
            state
                .task_console
                .selected_record()
                .is_some_and(|row| row.status.active())
                || !state.pending_message_texts().is_empty(),
        );
    }
    if !state.pending_message_texts().is_empty() {
        return 1;
    }
    if area.height >= 16
        && let Some(todos) = latest_todos(state)
        && !todos.is_empty()
    {
        return u16::try_from(todos.len().min(3) + 2).unwrap_or(5);
    }
    u16::from(
        state.has_active_turn()
            || state.side_command_activity().is_some()
            || state.is_compacting()
            || state.pending_runtime_question.is_some()
            || state.pending_mcp_elicitation.is_some()
            || state.pending_ask.is_some(),
    )
}

fn latest_todos(state: &AppState) -> Option<&[serde_json::Value]> {
    // The authoritative Work view owns migrated legacy state in interactive sessions.
    if state.task_console.attached() {
        return None;
    }
    state.items.iter().rev().find_map(|item| {
        let Item::Tool {
            name,
            result: Some((true, value)),
            ..
        } = item
        else {
            return None;
        };
        (canonical_tool_name(name) == "todo_write")
            .then(|| value.as_array())
            .flatten()
            .map(Vec::as_slice)
    })
}

pub(crate) fn current_activity(state: &AppState) -> String {
    if state.pending_runtime_question.is_some() || state.pending_mcp_elicitation.is_some() {
        return "Waiting for your answer".to_owned();
    }
    if state.pending_ask.is_some() {
        return "Waiting for approval".to_owned();
    }
    if state.task_console.active
        && let Some(row) = state.task_console.selected_record()
    {
        return row
            .telemetry
            .current_tool
            .as_ref()
            .filter(|_| row.status == crate::task_console::TaskStatus::Running)
            .map_or_else(
                || {
                    format!(
                        "{} · {}",
                        crate::task_console::safe(&row.label),
                        row.status_label()
                    )
                },
                |tool| {
                    format!(
                        "{} · {}",
                        crate::task_console::safe(&row.label),
                        crate::task_console::safe(tool)
                    )
                },
            );
    }
    if state.cancellation_requested && state.has_active_turn() {
        return "Cancelling…".to_owned();
    }
    if let Some(activity) = state.side_command_activity() {
        return activity.to_owned();
    }
    if state.is_compacting()
        || (state.has_active_turn()
            && state
                .context_budget
                .as_ref()
                .is_some_and(|budget| budget.activity == heycode_llm::ContextActivity::Compacting))
    {
        return "Compacting conversation… (Esc to cancel)".to_owned();
    }
    if !state.has_active_turn() {
        return "Ready".to_owned();
    }
    let latest = state.items[state.activity_item_start.min(state.items.len())..]
        .iter()
        .rev()
        .take_while(|item| !matches!(item, Item::User(_)));
    for item in latest.clone() {
        match item {
            Item::Tool {
                name,
                args,
                result: None,
                ..
            } => {
                return match canonical_tool_name(name) {
                    "bash" => args
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map_or_else(
                            || "Running a command…".to_owned(),
                            |command| {
                                format!(
                                    "Running: {}",
                                    crate::terminal::truncate_to_width(
                                        &markdown::terminal_safe_span(
                                            &command
                                                .split_whitespace()
                                                .collect::<Vec<_>>()
                                                .join(" ")
                                        ),
                                        usize::from(state.transcript_area.width.max(40))
                                            .saturating_sub(18)
                                    )
                                )
                            },
                        ),
                    "read" => "Reading a file…".to_owned(),
                    "grep" | "glob" => "Searching files…".to_owned(),
                    "todo_write" => "Updating tasks…".to_owned(),
                    "wait_agents" | "wait_agent" => "Waiting for agents…".to_owned(),
                    "agent_control" | "interrupt_task"
                        if args.get("action").and_then(serde_json::Value::as_str)
                            == Some("wait") =>
                    {
                        "Waiting for agents…".to_owned()
                    }
                    "agent_control" | "interrupt_task"
                        if args.get("action").and_then(serde_json::Value::as_str)
                            == Some("interrupt") =>
                    {
                        "Requesting agent cancellation…".to_owned()
                    }
                    name => format!("Using {name}…"),
                };
            }
            Item::ServerTool {
                logical,
                result: None,
                ..
            } => return format!("Using {logical}…"),
            _ => {}
        }
    }
    for item in latest {
        match item {
            Item::Tool { .. } | Item::ServerTool { .. } => return "Responding…".to_owned(),
            Item::Reasoning { done: false, .. } => return model_activity(state, true),
            Item::Assistant(text) if !text.is_empty() => return "Responding…".to_owned(),
            _ => {}
        }
    }
    model_activity(state, false)
}

fn model_activity(_state: &AppState, thinking: bool) -> String {
    if thinking {
        "Thinking…".to_owned()
    } else {
        "Working…".to_owned()
    }
}

fn observed_duration(seconds: u64) -> String {
    if seconds == 0 {
        "<1s".to_owned()
    } else {
        format!("{seconds}s")
    }
}

fn activity_label(state: &AppState) -> String {
    let label = current_activity(state);
    match state.turn_started_at.filter(|_| state.has_active_turn()) {
        Some(started) => format!(
            "{label}  ·  {}",
            observed_duration(started.elapsed().as_secs())
        ),
        None => label,
    }
}

fn draw_activity_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    if area.height == 0 {
        return;
    }
    let styles = state.styles();
    let todos = if state.task_console.active {
        None
    } else {
        latest_todos(state)
    };
    let Some(todos) = todos else {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{} ", state.spinner_glyph()),
                    Style::default().fg(styles.accent()),
                ),
                Span::styled(
                    current_activity(state),
                    Style::default().fg(styles.accent()),
                ),
                Span::styled(
                    (!state.task_console.active && state.side_command_activity().is_none())
                        .then_some(state.turn_started_at)
                        .flatten()
                        .map(|started| {
                            let effort = state
                                .reasoning_effort
                                .as_deref()
                                .map(|value| format!(" · thinking with {value} effort"))
                                .unwrap_or_default();
                            format!(
                                " ({}{effort})",
                                observed_duration(started.elapsed().as_secs())
                            )
                        })
                        .unwrap_or_default(),
                    Style::default().fg(styles.dim()),
                ),
            ])),
            area,
        );
        return;
    };
    let completed = todos
        .iter()
        .filter(|todo| todo.get("status").and_then(serde_json::Value::as_str) == Some("completed"))
        .count();
    let mut lines = vec![Line::from(vec![
        Span::styled(" Plan ", Style::default().fg(styles.accent()).bold()),
        Span::styled(
            format!("{completed}/{} complete", todos.len()),
            Style::default().fg(styles.dim()),
        ),
        Span::styled(
            format!(" · {}", activity_label(state)),
            Style::default().fg(styles.text()),
        ),
    ])];
    for todo in todos
        .iter()
        .filter(|todo| todo.get("status").and_then(serde_json::Value::as_str) != Some("completed"))
        .chain(todos.iter().filter(|todo| {
            todo.get("status").and_then(serde_json::Value::as_str) == Some("completed")
        }))
        .take(usize::from(area.height.saturating_sub(1)))
    {
        let status = todo
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending");
        let (glyph, color) = match status {
            "completed" => ("✓", styles.success()),
            "in_progress" => ("●", styles.accent()),
            _ => ("○", styles.dim()),
        };
        let content = todo
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Unnamed task");
        lines.push(Line::from(vec![
            Span::styled(format!("  {glyph} "), Style::default().fg(color)),
            Span::styled(content.to_owned(), Style::default().fg(styles.text())),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn inline_surface_height(state: &AppState, width: u16) -> u16 {
    if state.onboarding_is_panel() {
        return onboarding_panel_height(state, width);
    }
    if state.optional_question_panel_visible() {
        return u16::try_from(
            crate::app::optional_questions::panel_lines(state, width, 17).len() + 1,
        )
        .unwrap_or(18);
    }
    let task_height = crate::task_render::surface_height(state, width);
    if task_height > 0
        && !state.pending_ask.is_some()
        && state.pending_runtime_question.is_none()
        && state.pending_mcp_elicitation.is_none()
    {
        return task_height;
    }
    if let Some(palette) = state.command_palette() {
        return u16::try_from(palette.matches().len().clamp(1, 8) + 1).unwrap_or(9);
    }
    if state.pending_ask.is_some() {
        return ask_card_height(state, width);
    }
    if let Some(question) = state.pending_runtime_question.as_ref() {
        return runtime_question_height(state, question, width);
    }
    if let Some(panel) = state.plugin_panel() {
        return panel.desired_height();
    }
    if state.effort_picker.is_some() {
        return 9;
    }
    if let Some(panel) = state.sandbox_panel() {
        return panel_frame::height(panel.lines(width, state.styles()).len());
    }
    if let Some(panel) = state.autocompact_panel() {
        return panel_frame::height(panel.lines(width, state.styles()).len());
    }
    if state.permission_picker().is_some() {
        return panel_frame::height(permission_picker_lines(state, width).len());
    }
    if let Some(dialog) = state.add_directory_dialog.as_ref() {
        return dialog.desired_height();
    }
    if let Some(picker) = state.model_picker.as_ref() {
        return u16::try_from(11 + picker.matches().len().min(8)).unwrap_or(19);
    }
    if let Some(picker) = state.route_picker.as_ref() {
        return u16::try_from(5 + picker.matches().len().min(8) * 2).unwrap_or(21);
    }
    if let Some(picker) = state.rewind_picker.as_ref() {
        return picker.desired_height();
    }
    if let Some(panel) = state.advisor_panel.as_ref() {
        return panel.desired_height();
    }
    if state.theme_picker().is_some() {
        return panel_frame::height(theme_picker_lines(state, width).len());
    }
    if state.scroll_speed_picker().is_some() {
        // Title, blank, gauge, blank, preview note, blank and hint.
        return panel_frame::height(7);
    }
    if let Some(help) = state.help_panel.as_ref() {
        return help.desired_height();
    }
    if let Some(progress) = state.export_progress.as_ref() {
        return progress.desired_height();
    }
    if let Some(panel) = state.export_panel.as_ref() {
        return panel.desired_height();
    }
    if let Some(panel) = state.copy_panel.as_ref() {
        return panel.desired_height();
    }
    if let Some(panel) = state.memory_panel.as_ref() {
        return panel.desired_height();
    }
    if let Some(picker) = state.profile_picker() {
        return u16::try_from(picker.rows().len().saturating_add(2)).unwrap_or(u16::MAX);
    }
    if let Some(panel) = state.skills_panel() {
        return panel.desired_height(width);
    }
    if state.keymap_picker().is_some() {
        return panel_frame::height(keymap_picker_lines(state, width, 17).len());
    }
    if let Some(panel) = state.capability_catalog() {
        match panel.panel() {
            crate::panel_commands::CapabilityPanel::Hooks => {
                return panel_frame::height(
                    hooks_panel_lines(panel, width, state.styles(), 17).len(),
                );
            }
            crate::panel_commands::CapabilityPanel::Agents => {
                return panel_frame::height(
                    agent_catalog_lines(panel, width, state.styles(), 17).len(),
                );
            }
            _ => {}
        }
    }
    if state.pending_mcp_elicitation.is_some()
        || state.pending_command_confirmation.is_some()
        || state.keymap_picker().is_some()
        || state.session_browser().is_some()
        || state.skill_doctor_panel.is_some()
        || state.skills_panel().is_some()
        || state.capability_catalog().is_some()
        || state.settings_panel().is_some()
        || state.plugin_panel().is_some()
        || state.mcp_panel().is_some()
        || state.effort_picker.is_some()
    {
        18
    } else {
        0
    }
}

fn runtime_question_height(
    state: &AppState,
    question: &crate::app::PendingRuntimeQuestionView,
    width: u16,
) -> u16 {
    u16::try_from(runtime_question_lines(state, question, width, 17).len() + 1).unwrap_or(18)
}

fn draw_header(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let show_pet = state.shell_preferences().header_pet() && area.height >= 5 && area.width >= 60;
    if show_pet {
        state.mascot_hitbox = Rect::new(area.x, area.y.saturating_add(1), crate::mascot::WIDTH, 4);
    }
    let styles = state.styles();
    let model = if state.model.is_empty() {
        "No model selected"
    } else {
        state.active_model_label()
    };
    let runtime = human_runtime_label(&state.runtime);
    let cwd = compact_cwd(&state.cwd);

    if area.height == 1 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" HeyCode", Style::default().fg(styles.accent()).bold()),
                Span::styled(
                    format!(" {}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(styles.dim()),
                ),
                Span::styled("  ·  ", Style::default().fg(styles.border())),
                Span::styled(model.to_owned(), Style::default().fg(styles.text()).bold()),
                Span::styled(format!("  ·  {cwd}"), Style::default().fg(styles.dim())),
            ])),
            area,
        );
        return;
    }

    let pet = show_pet;
    let rows = state.companion.rows(
        if state.has_active_turn() {
            state.spinner
        } else {
            state.pet_frame
        },
        state.has_active_turn(),
        state.pending_ask.is_some()
            || state.pending_runtime_question.is_some()
            || state.pending_mcp_elicitation.is_some()
            || state.pending_command_confirmation.is_some(),
        state.chrome().animation(),
    );
    let mascot = |row: usize| -> Vec<Span<'static>> {
        if !pet {
            return vec![Span::raw(" ")];
        }
        vec![Span::styled(
            rows[row].clone(),
            Style::default().fg(styles.companion()),
        )]
    };
    let gap = if pet { "   " } else { " " };
    let mut title = mascot(0);
    title.extend([
        Span::raw(gap),
        Span::styled("HeyCode", Style::default().fg(styles.text()).bold()),
        Span::styled(
            format!(" {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(styles.dim()),
        ),
    ]);
    let mut identity = mascot(1);
    identity.extend([
        Span::raw(gap),
        Span::styled(model.to_owned(), Style::default().fg(styles.text()).bold()),
        Span::styled(format!("  ·  {runtime}"), Style::default().fg(styles.dim())),
    ]);
    let mut workspace = mascot(2);
    workspace.extend([
        Span::raw(gap),
        Span::styled(cwd, Style::default().fg(styles.dim())),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            blank(),
            Line::from(title),
            Line::from(identity),
            Line::from(workspace),
            Line::from(mascot(3)),
        ]),
        area,
    );
}

fn human_runtime_label(runtime: &str) -> String {
    match runtime {
        "" => "Unconfigured",
        "native" => "heycode native",
        "claude" => "Claude",
        "codex" => "Codex",
        "gemini" => "Gemini",
        other => other,
    }
    .to_owned()
}

/// The footer's separator, carried by the entry that follows it so a mouse
/// hit region covers exactly the text it belongs to.
fn separated(label: impl std::fmt::Display) -> String {
    format!(" · {label}")
}

fn draw_controls(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    if area.height == 0 {
        return;
    }
    use crate::task_console::{TaskCategory, TaskHit, TaskKind};
    let styles = state.styles();
    if state.shortcut_list_visible() {
        crate::composer_shortcuts::draw(
            frame,
            area,
            &state.shortcut_list(),
            Style::default().fg(styles.dim()),
        );
        return;
    }
    let permission = crate::permission_picker::permission_footer_label(&state.permission);
    let permission_color = match crate::permission_picker::permission_role(&state.permission) {
        ThemeRole::Accent => styles.accent(),
        ThemeRole::PanelTitle => styles.panel_title(),
        ThemeRole::Success => styles.success(),
        ThemeRole::Error => styles.error(),
        ThemeRole::Warn => styles.warn(),
        ThemeRole::Code => styles.code(),
        ThemeRole::Text => styles.text(),
        ThemeRole::Border => styles.border(),
        ThemeRole::PromptBackground => styles.prompt_background(),
        ThemeRole::PromptGlyph => styles.prompt_glyph(),
        ThemeRole::Dim => styles.dim(),
    };
    let mut entries: Vec<(String, Option<TaskHit>)> = Vec::new();
    if !permission.is_empty() {
        entries.push((permission.clone(), None));
        // One way forward sits next to the mode, never two: the cycle chord
        // when the session is in a mode it can step out of, and the shortcut
        // list when it is in the mode everything else cycles back to. A draft
        // in progress hides the discovery hint but keeps the cycle chord.
        let suffix = if matches!(state.permission.as_str(), "ask" | "default") {
            state
                .input
                .lines()
                .iter()
                .all(String::is_empty)
                .then_some(" · ? for shortcuts")
        } else if state.can_cycle_approval_mode() {
            Some(" (shift+tab to cycle)")
        } else {
            None
        };
        // Truncating an affordance into nonsense is worse than dropping it, so
        // it appears only when the whole line still fits the terminal.
        if let Some(suffix) = suffix
            && 2 + permission.width()
                + suffix.width()
                + usize::from(state.task_console.pending_issue_count() > 0) * 12
                <= usize::from(area.width)
        {
            entries.push((suffix.to_owned(), None));
        }
    }
    let issues = state.task_console.pending_issue_count();
    if issues > 0 {
        entries.push((
            separated(format!(
                "{issues} issue{}",
                if issues == 1 { "" } else { "s" }
            )),
            Some(TaskHit::ReviewIssue),
        ));
    }
    if state.task_console.inventory_error.is_some() {
        entries.push((
            separated("Agents unavailable"),
            Some(TaskHit::Category(TaskCategory::Agents)),
        ));
    }
    if let Some((label, _)) = crate::task_render::jobs_badge(state, area.width) {
        entries.push((separated(label), Some(TaskHit::Category(TaskCategory::All))));
    }
    let records = state.strip_task_records();
    let agents = state.active_child_records().len();
    if agents > 0 && !state.task_console.active {
        entries.push((
            separated(format!(
                "{agents} agent{}",
                if agents == 1 { "" } else { "s" }
            )),
            Some(TaskHit::Category(TaskCategory::Agents)),
        ));
    }
    for (kind, category, name) in [
        (TaskKind::Team, TaskCategory::Teams, "team"),
        (TaskKind::Work, TaskCategory::Work, "task"),
    ] {
        let count = records.iter().filter(|row| row.kind == kind).count();
        if count > 0 {
            entries.push((
                separated(format!(
                    "{count} {name}{}",
                    if count == 1 { "" } else { "s" }
                )),
                Some(TaskHit::Category(category)),
            ));
        }
    }
    let mut x = area.x.saturating_add(2);
    for (index, (label, hit)) in entries.into_iter().enumerate() {
        let width = (label.width() as u16).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(
                if index == 0 && !permission.is_empty() {
                    permission_color
                } else if hit.is_some() {
                    styles.accent()
                } else {
                    styles.dim()
                },
            )),
            rect,
        );
        if let Some(hit) = hit {
            state.task_console.hits.push((rect, hit));
        }
        x += width;
    }
}

fn draw_transcript_with_side_panel(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let Some(panel) = state.side_panel_snapshot() else {
        draw_transcript(frame, state, area);
        return;
    };
    if area.width >= 80 {
        let [transcript, side] =
            Layout::horizontal([Constraint::Percentage(68), Constraint::Percentage(32)])
                .areas(area);
        draw_transcript(frame, state, transcript);
        draw_side_panel(frame, state, &panel, side);
    } else {
        let panel_height = area.height.saturating_div(3).clamp(3, 8);
        let [transcript, side] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(panel_height)]).areas(area);
        draw_transcript(frame, state, transcript);
        draw_side_panel(frame, state, &panel, side);
    }
}

fn draw_side_panel(
    frame: &mut Frame<'_>,
    state: &AppState,
    panel: &crate::side_panel::SidePanelSnapshot,
    area: Rect,
) {
    use crate::side_panel::SidePanelTone;

    let mut lines = vec![Line::from(Span::styled(
        panel.summary().to_owned(),
        Style::default().fg(state.styles().dim()),
    ))];
    lines.extend(panel.rows().iter().map(|row| {
        let color = match row.tone() {
            SidePanelTone::Normal => state.styles().text(),
            SidePanelTone::Positive => state.styles().success(),
            SidePanelTone::Negative => state.styles().error(),
            SidePanelTone::Warning => state.styles().warn(),
        };
        Line::from(Span::styled(
            row.text().to_owned(),
            Style::default().fg(color),
        ))
    }));
    let block = if state.chrome().borders() {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(state.styles().border()))
            .title(format!(" {} · Ctrl+B cycle ", panel.kind().title()))
    } else {
        Block::default().title(format!("{} · Ctrl+B cycle", panel.kind().title()))
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_workspace_trust(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(view) = state.workspace_trust() else {
        return;
    };
    let dialog = view.state();
    let workspace_id = dialog
        .workspace_id()
        .as_str()
        .chars()
        .take(12)
        .collect::<String>();
    let access = |label: &str, value: heycode_trust::ProjectAccess| {
        Line::from(vec![
            Span::styled(
                format!("{label:<28}"),
                Style::default().fg(state.styles().dim()),
            ),
            Span::styled(
                if value.is_allowed() {
                    "allowed"
                } else {
                    "blocked"
                },
                Style::default()
                    .fg(if value.is_allowed() {
                        state.styles().success()
                    } else {
                        state.styles().warn()
                    })
                    .bold(),
            ),
        ])
    };
    let mut lines = vec![
        Line::from(Span::styled(
            "Trust this workspace?",
            Style::default().fg(state.styles().warn()).bold(),
        )),
        blank(),
        Line::from(Span::styled(
            dialog.canonical_root().display().to_string(),
            Style::default().fg(state.styles().text()).bold(),
        )),
        Line::from(vec![
            Span::styled("workspace id  ", Style::default().fg(state.styles().dim())),
            Span::styled(workspace_id, Style::default().fg(state.styles().text())),
        ]),
        blank(),
        Line::from(Span::styled(
            "Project files can define instructions and executable authority.",
            Style::default().fg(state.styles().warn()),
        )),
        access("project instructions", dialog.instructions()),
        access("project settings", dialog.settings()),
        access(
            "project plugins / MCP / hooks",
            dialog.project_executables(),
        ),
        blank(),
    ];
    for (index, action) in dialog.actions().iter().copied().enumerate() {
        let selected = index == view.selected();
        let (label, detail) = match action {
            heycode_trust::WorkspaceTrustAction::TrustOnce => (
                "Trust once",
                "Enable project executable contributions for this process",
            ),
            heycode_trust::WorkspaceTrustAction::TrustWorkspace => (
                "Trust this workspace",
                "Save trust for this canonical workspace identity",
            ),
            heycode_trust::WorkspaceTrustAction::OpenRestricted => (
                "Open read-only",
                "Keep project executable contributions disabled",
            ),
            heycode_trust::WorkspaceTrustAction::Exit => {
                ("Exit", "Leave without changing workspace trust")
            }
        };
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "● " } else { "  " },
                Style::default().fg(state.styles().accent()),
            ),
            Span::styled(
                format!("{label:<23}"),
                Style::default()
                    .fg(if selected {
                        state.styles().accent()
                    } else {
                        state.styles().text()
                    })
                    .add_modifier(if selected {
                        ratatui::style::Modifier::BOLD
                    } else {
                        ratatui::style::Modifier::empty()
                    }),
            ),
            Span::styled(detail, Style::default().fg(state.styles().dim())),
        ]));
    }
    if let Some(error) = view.error() {
        lines.extend([
            blank(),
            Line::from(Span::styled(
                error.to_owned(),
                Style::default().fg(state.styles().error()),
            )),
        ]);
    }
    lines.extend([
        blank(),
        Line::from(Span::styled(
            "↑↓ choose · enter confirm · esc exit · ctrl+c twice exit",
            Style::default().fg(state.styles().border()),
        )),
        Line::from(Span::styled(
            "Trust changes restart composition before project content can load.",
            Style::default().fg(state.styles().dim()),
        )),
    ]);
    let height = u16::try_from(lines.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let card = centered_dialog_rect(area, area.width.saturating_sub(4).clamp(72, 108), height);
    let block = Block::default()
        .title(Span::styled(
            " Workspace trust ",
            Style::default().fg(state.styles().warn()).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().warn()));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

fn draw_command_confirmation(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(confirm) = state.pending_command_confirmation.as_ref() else {
        return;
    };
    let card = area;
    let option = |index: usize, label: &str| {
        let selected = confirm.selection == index;
        Span::styled(
            format!("{} {label}", if selected { "●" } else { "○" }),
            Style::default()
                .fg(if selected {
                    state.styles().accent()
                } else {
                    state.styles().dim()
                })
                .add_modifier(if selected {
                    ratatui::style::Modifier::BOLD
                } else {
                    ratatui::style::Modifier::empty()
                }),
        )
    };
    let lines = vec![
        Line::from(Span::styled(
            "Interrupt active work?",
            Style::default().fg(state.styles().warn()).bold(),
        )),
        blank(),
        Line::from(vec![
            Span::styled(
                confirm.synopsis.clone(),
                Style::default().fg(state.styles().text()),
            ),
            Span::styled(
                " is marked interrupting",
                Style::default().fg(state.styles().dim()),
            ),
        ]),
        blank(),
        Line::from(vec![
            option(0, "Cancel"),
            Span::raw("      "),
            option(1, "Interrupt & run"),
        ]),
        Line::from(Span::styled(
            "←→ choose · enter confirm · esc cancel",
            Style::default().fg(state.styles().border()),
        )),
    ];
    let block = Block::default()
        .title(Span::styled(
            " command ",
            Style::default().fg(state.styles().warn()).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().warn()));
    frame.render_widget(Clear, card);
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

fn draw_profile_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(picker) = state.profile_picker() else {
        return;
    };
    let card = area;
    frame.render_widget(Clear, card);
    let lines = picker
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = index == picker.selected();
            Line::from(vec![
                Span::styled(
                    if selected { "❯ " } else { "  " },
                    Style::default().fg(if selected {
                        state.styles().accent()
                    } else {
                        state.styles().dim()
                    }),
                ),
                Span::styled(
                    row.label().to_owned(),
                    Style::default()
                        .fg(if selected {
                            state.styles().text()
                        } else {
                            state.styles().dim()
                        })
                        .bold(),
                ),
                Span::styled(
                    if row.current { "  (current)" } else { "" },
                    Style::default().fg(state.styles().dim()),
                ),
            ])
        })
        .chain(std::iter::once(Line::from(Span::styled(
            "↑↓ choose · Enter recompose · Esc close",
            Style::default().fg(state.styles().dim()),
        ))))
        .collect::<Vec<_>>();
    let block = Block::default()
        .title(Span::styled(
            " named profiles ",
            Style::default().fg(state.styles().accent()).bold(),
        ))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(state.styles().border()));
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

fn draw_session_browser(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    use crate::session_browser::{SessionDeleteChoice, SessionStorageView};
    use heycode_session::{SessionActivityStatus, SessionSource};

    let Some(browser) = state.session_browser() else {
        return;
    };
    let mut lines = vec![Line::from(Span::styled(
        match browser.storage() {
            SessionStorageView::Active => "Resume session",
            SessionStorageView::Archived => "Resume archived session",
            SessionStorageView::All => "Resume any saved session",
        },
        Style::default().fg(state.styles().accent()).bold(),
    ))];
    lines.extend(session_search_box(state, browser, area.width));
    if browser.filters_active() {
        lines.push(Line::from(vec![
            Span::styled("  filter  ", Style::default().fg(state.styles().dim())),
            Span::styled(
                browser.storage().label(),
                Style::default().fg(state.styles().text()),
            ),
            Span::styled(" · ", Style::default().fg(state.styles().dim())),
            Span::styled(
                browser.lineage().label(),
                Style::default().fg(state.styles().text()),
            ),
            Span::styled(" · ", Style::default().fg(state.styles().dim())),
            Span::styled(
                browser
                    .status()
                    .map_or("all status", |status| match status {
                        SessionActivityStatus::Empty => "empty",
                        SessionActivityStatus::Idle => "idle",
                        SessionActivityStatus::OpenTurn => "open turn",
                    }),
                Style::default().fg(state.styles().text()),
            ),
            Span::styled(
                browser
                    .source()
                    .map_or(" · all sources", |source| match source {
                        SessionSource::Interactive => " · interactive",
                        SessionSource::Headless => " · headless",
                        SessionSource::Acp => " · acp",
                        SessionSource::Subagent => " · subagent",
                        SessionSource::Scheduled => " · scheduled",
                        SessionSource::Delegated => " · delegated",
                        SessionSource::Fork => " · fork source",
                    }),
                Style::default().fg(state.styles().text()),
            ),
            Span::styled(
                if browser.current_cwd_only() {
                    " · this folder"
                } else {
                    ""
                },
                Style::default().fg(state.styles().accent()),
            ),
            Span::styled(
                if browser.current_runtime_only() {
                    " · current runtime"
                } else {
                    ""
                },
                Style::default().fg(state.styles().accent()),
            ),
        ]));
        lines.push(blank());
    }
    if let Some(error) = browser.error() {
        lines.push(Line::from(Span::styled(
            format!("  session store error: {error}"),
            Style::default().fg(state.styles().error()),
        )));
    }
    if browser.rows().is_empty() && browser.error().is_none() {
        lines.push(Line::from(Span::styled(
            if browser.search().is_empty() {
                "  No saved conversations in this view."
            } else {
                "  No conversations match your search."
            },
            Style::default().fg(state.styles().dim()),
        )));
    }
    let visible = usize::from((area.height.saturating_sub(10) / 5).max(1));
    let selected = browser.selected().and_then(|selected| {
        browser
            .rows()
            .iter()
            .position(|row| std::ptr::eq(row, selected))
    });
    let start = selected
        .unwrap_or(0)
        .saturating_sub(visible.saturating_sub(1).min(visible / 2));
    for (index, row) in browser.rows().iter().enumerate().skip(start).take(visible) {
        if let Some(group) = browser.workspace_group(index).or_else(|| {
            (index == start).then(|| {
                row.summary().cwd().map_or_else(
                    || "Unknown workspace".to_owned(),
                    |path| path.display().to_string(),
                )
            })
        }) {
            lines.push(Line::styled(
                format!("  {}", safe_setting_value(&group)),
                Style::default().fg(state.styles().dim()),
            ));
            lines.push(blank());
        }
        let selected = selected == Some(index);
        let summary = row.summary();
        let unreadable = !summary.is_readable();
        let title = summary.title().unwrap_or(summary.id().as_str());
        let markers = format!(
            "{}{}{}{}{}",
            if row.is_current() { " · current" } else { "" },
            "",
            if summary.storage() == heycode_session::SessionStorageState::Archived {
                " · archived"
            } else {
                ""
            },
            if unreadable { " · unreadable" } else { "" },
            summary
                .lineage()
                .map_or_else(String::new, |lineage| format!(
                    " · branch of {}@{}",
                    &lineage.parent_session_id().as_str()
                        [..8.min(lineage.parent_session_id().as_str().len())],
                    lineage.seed_event_count()
                )),
        );
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "❯ " } else { "  " },
                Style::default().fg(state.styles().accent()),
            ),
            Span::styled(
                safe_setting_value(title),
                Style::default()
                    .fg(if selected {
                        state.styles().accent()
                    } else {
                        state.styles().text()
                    })
                    .bold(),
            ),
            Span::styled(
                markers,
                Style::default().fg(if unreadable {
                    state.styles().error()
                } else {
                    state.styles().dim()
                }),
            ),
        ]));
        let source = summary
            .source()
            .map_or("unknown source", |source| match source {
                SessionSource::Interactive => "interactive",
                SessionSource::Headless => "headless",
                SessionSource::Acp => "acp",
                SessionSource::Subagent => "subagent",
                SessionSource::Scheduled => "scheduled",
                SessionSource::Delegated => "delegated",
                SessionSource::Fork => "fork",
            });
        let status = if unreadable {
            "unreadable"
        } else {
            match summary.status() {
                SessionActivityStatus::Empty => "empty",
                SessionActivityStatus::Idle => "idle",
                SessionActivityStatus::OpenTurn => "open turn",
            }
        };
        lines.push(Line::from(Span::styled(
            format!(
                "  {} · {} events · {} · {} · {}{}",
                crate::session_browser::relative_age(summary.last_activity_ms()),
                summary.event_count(),
                source,
                status,
                summary.runtime().unwrap_or("unknown runtime"),
                // Once other folders are listed the row must say where it lives.
                if browser.current_cwd_only() {
                    String::new()
                } else {
                    summary.cwd().map_or_else(
                        || " · unknown cwd".to_owned(),
                        |cwd| format!(" · {}", cwd.display()),
                    )
                },
            ),
            Style::default().fg(state.styles().dim()),
        )));
        lines.push(blank());
    }
    lines.push(Line::from(Span::styled(
        format!(
            "  page {} · {} matches{}",
            browser.page_index() + 1,
            browser.total_matches(),
            if browser.has_next_page() {
                " · more"
            } else {
                ""
            }
        ),
        Style::default().fg(state.styles().dim()),
    )));
    if let Some(input) = browser.rename_input() {
        lines.push(Line::from(vec![
            Span::styled(
                "  rename  ",
                Style::default().fg(state.styles().warn()).bold(),
            ),
            Span::styled(
                safe_setting_value(input),
                Style::default().fg(state.styles().text()),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "  type · Enter commit · Esc cancel",
            Style::default().fg(state.styles().dim()),
        )));
    } else if let Some(confirmation) = browser.delete_confirmation() {
        let cancel = confirmation.choice() == SessionDeleteChoice::Cancel;
        lines.push(Line::from(Span::styled(
            format!(
                "  Delete {}? Descendants/current/open sessions are refused by the store.",
                confirmation.session_id().as_str()
            ),
            Style::default().fg(state.styles().warn()).bold(),
        )));
        lines.push(Line::from(vec![
            Span::styled(
                if cancel { "❯ Cancel" } else { "  Cancel" },
                Style::default().fg(if cancel {
                    state.styles().success()
                } else {
                    state.styles().dim()
                }),
            ),
            Span::raw("    "),
            Span::styled(
                if cancel {
                    "  Move to trash"
                } else {
                    "❯ Move to trash"
                },
                Style::default().fg(if cancel {
                    state.styles().dim()
                } else {
                    state.styles().error()
                }),
            ),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            browser.hint(),
            Style::default().fg(state.styles().dim()),
        )));
        if !browser.search_active() {
            lines.push(Line::from(Span::styled(
            "  PgUp/PgDn page · n/f/a/d/e actions · s storage · l lineage · t status · c source · w cwd · v runtime · Esc close",
            Style::default().fg(state.styles().border()),
        )));
        }
    }
    if let Some(notice) = browser.notice() {
        lines.push(Line::from(Span::styled(
            format!("  {}", safe_setting_value(notice)),
            Style::default().fg(state.styles().warn()),
        )));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new("▔".repeat(usize::from(area.width)))
            .style(Style::default().fg(state.styles().accent())),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let body = Rect::new(
        area.x.saturating_add(SESSION_PANEL_INDENT),
        area.y.saturating_add(1),
        area.width.saturating_sub(SESSION_PANEL_INDENT),
        area.height.saturating_sub(1),
    );
    // Narrow terminals reflow the long rows, as the source does, rather than
    // silently cutting a hint or a conversation title at the right edge.
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        body,
    );
    if browser.search_active() && area.height > 4 && area.width > 0 {
        let width = u16::try_from(browser.search().width()).unwrap_or(u16::MAX);
        frame.set_cursor_position((
            body.x
                .saturating_add(4)
                .saturating_add(width)
                .min(area.right().saturating_sub(1)),
            body.y + 2,
        ));
    }
}

/// Indent shared by every session-panel row, matching the source's frame.
const SESSION_PANEL_INDENT: u16 = 3;

/// The source's rounded single-line search field, with a placeholder when the
/// query is empty. Content wider than the field is truncated, never wrapped.
fn session_search_box(
    state: &AppState,
    browser: &crate::session_browser::SessionBrowserView,
    width: u16,
) -> Vec<Line<'static>> {
    let outer = usize::from(width.saturating_sub(SESSION_PANEL_INDENT * 2)).max(4);
    // Field body: the two borders plus the leading `` ⌕ `` marker.
    let inner = outer.saturating_sub(5);
    let query = safe_setting_value(browser.search());
    let (text, style) = if query.is_empty() {
        (
            "Search…".to_owned(),
            Style::default().fg(state.styles().dim()),
        )
    } else {
        (
            query,
            Style::default().fg(if browser.search_active() {
                state.styles().accent()
            } else {
                state.styles().text()
            }),
        )
    };
    let shown: String = text.chars().take(inner).collect();
    let pad = inner.saturating_sub(shown.width());
    let border = Style::default().fg(state.styles().border());
    vec![
        Line::from(Span::styled(
            format!("╭{}╮", "─".repeat(outer.saturating_sub(2))),
            border,
        )),
        Line::from(vec![
            Span::styled("│ ⌕ ", border),
            Span::styled(shown, style),
            Span::styled(format!("{}│", " ".repeat(pad)), border),
        ]),
        Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(outer.saturating_sub(2))),
            border,
        )),
    ]
}

fn draw_settings_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    use heycode_ui::settings_ui::FieldOrigin;

    use crate::settings_panel::SettingsShellFocus;
    use heycode_agent::ui::{SettingsShellSection, SettingsShellTab};
    let Some(shell) = state.settings_shell() else {
        return;
    };
    if shell.tab() == SettingsShellTab::Stats && shell.stats_view().is_some() {
        crate::stats_render::draw(frame, shell, area, state.styles());
        return;
    }
    if shell.tab() == SettingsShellTab::Stats && shell.stats_view().is_some() {
        crate::stats_render::draw(frame, shell, area, state.styles());
        return;
    }
    let panel = shell.config();
    let visible = usize::from(area.height.saturating_sub(10).max(1));
    let filtered = shell.filtered_rows();
    let selected_position = filtered
        .iter()
        .position(|(index, _)| *index == panel.selected())
        .unwrap_or(0);
    let start = selected_position.saturating_sub(visible.saturating_sub(1).min(visible / 2));
    let mut lines = Vec::new();
    let mut mouse_rows = Vec::new();
    let mut tab_hits = Vec::new();
    let heading = format!("{}Settings", panel_frame::INDENT);
    let mut tab_x = area
        .x
        .saturating_add(u16::try_from(heading.len()).unwrap_or(u16::MAX));
    let mut tabs = vec![Span::styled(
        heading,
        Style::default().fg(state.styles().accent()).bold(),
    )];
    for tab in SettingsShellTab::ALL {
        let label = format!("  {} ", tab.label());
        let width = u16::try_from(label.len()).unwrap_or(u16::MAX);
        tab_hits.push((Rect::new(tab_x, area.y.saturating_add(1), width, 1), tab));
        tab_x = tab_x.saturating_add(width);
        tabs.push(Span::styled(
            label,
            if tab == shell.tab() {
                Style::default()
                    .fg(state.styles().text())
                    .bg(state.styles().prompt_background())
                    .bold()
            } else {
                Style::default().fg(state.styles().dim())
            },
        ));
    }
    lines.push(Line::from(tabs));
    lines.push(panel_frame::blank());
    let search_hit = if shell.tab() == SettingsShellTab::Config {
        lines.extend(settings_search_box(
            shell.query(),
            area.width,
            state.styles(),
        ));
        lines.push(panel_frame::blank());
        Some(Rect::new(area.x, area.y.saturating_add(4), area.width, 1))
    } else {
        None
    };
    shell.set_shell_mouse_regions(tab_hits, search_hit);
    if let Some(section) = shell.section() {
        let color = match section {
            SettingsShellSection::Failed { .. } => state.styles().error(),
            SettingsShellSection::Unavailable { .. } => state.styles().warn(),
            _ => state.styles().text(),
        };
        let available = usize::from(area.height.saturating_sub(6));
        let rendered = wrap_plain_dim(
            section.plain_text(),
            usize::from(area.width),
            state.styles(),
        );
        shell.set_content_layout_rows(available, rendered.len());
        lines.extend(
            rendered
                .into_iter()
                .skip(shell.content_offset())
                .take(available)
                .map(|line| line.style(Style::default().fg(color))),
        );
    }
    let window = if shell.tab() == SettingsShellTab::Config {
        visible
    } else {
        0
    };
    // One value column for the whole window, as the reference does; a
    // per-row separator would make the values saw-tooth down the panel.
    let path_width = filtered
        .iter()
        .skip(start)
        .take(window)
        .map(|(_, row)| {
            UnicodeWidthStr::width(row.namespace())
                + if row.path().is_empty() {
                    0
                } else {
                    UnicodeWidthStr::width(row.path()) + 1
                }
        })
        .max()
        .unwrap_or(0)
        .min(46)
        .saturating_add(1);
    for (index, row) in filtered.iter().copied().skip(start).take(window) {
        let selected = index == panel.selected();
        let origin = match row.origin() {
            Some(FieldOrigin::Default) => "default",
            Some(FieldOrigin::User) => "user",
            Some(FieldOrigin::Project) => "project",
            Some(FieldOrigin::Managed) => "managed",
            Some(_) => "other",
            None => "custom",
        };
        let value = match row.value() {
            crate::settings_panel::SettingsPanelValue::Toggle(value) => {
                if *value {
                    "on".to_owned()
                } else {
                    "off".to_owned()
                }
            }
            crate::settings_panel::SettingsPanelValue::Text(value)
            | crate::settings_panel::SettingsPanelValue::Number(value) => safe_setting_value(value),
            crate::settings_panel::SettingsPanelValue::Choice { selected, .. } => selected
                .as_deref()
                .map_or_else(|| "<invalid choice>".to_owned(), safe_setting_value),
            crate::settings_panel::SettingsPanelValue::Secret { configured } => {
                if *configured {
                    "configured".to_owned()
                } else {
                    "not configured".to_owned()
                }
            }
            crate::settings_panel::SettingsPanelValue::Unrenderable { reason } => {
                format!("unrenderable: {}", safe_setting_value(reason))
            }
            crate::settings_panel::SettingsPanelValue::Custom { panel } => {
                format!("custom panel: {}", safe_setting_value(panel))
            }
        };
        let path = if row.path().is_empty() {
            row.namespace().to_owned()
        } else {
            format!("{}.{}", row.namespace(), row.path())
        };
        let row_y = area
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
        if row_y < area.bottom() {
            mouse_rows.push((Rect::new(area.x, row_y, area.width, 1), index));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{}{}",
                    panel_frame::INDENT,
                    if selected { "❯ " } else { "  " }
                ),
                Style::default().fg(state.styles().accent()),
            ),
            Span::styled(
                format!("{path:<path_width$}"),
                Style::default().fg(if row.editable() {
                    state.styles().text()
                } else {
                    state.styles().dim()
                }),
            ),
            Span::styled(" ", Style::default()),
            Span::styled(
                value,
                Style::default().fg(if row.editable() {
                    state.styles().success()
                } else {
                    state.styles().warn()
                }),
            ),
            Span::styled(
                format!(
                    "  [{origin} · {}]",
                    match row.applies() {
                        heycode_settings::SettingsApplies::Live => "live",
                        heycode_settings::SettingsApplies::Restart => "restart",
                    }
                ),
                Style::default().fg(state.styles().dim()),
            ),
        ]));
        if selected && let Some(explanation) = row.explanation() {
            lines.push(Line::from(Span::styled(
                format!(
                    "{}  {}",
                    panel_frame::INDENT,
                    safe_setting_value(explanation)
                ),
                Style::default().fg(state.styles().warn()),
            )));
        }
    }
    if shell.tab() == SettingsShellTab::Config {
        if filtered.is_empty() {
            lines.push(panel_frame::note("No matching settings", state.styles()));
        }
        let below = filtered
            .len()
            .saturating_sub(start + window.min(filtered.len()));
        lines.push(Line::from(Span::styled(
            if below > 0 {
                format!("{}↓ {below} more below", panel_frame::INDENT)
            } else {
                format!("{}{} settings", panel_frame::INDENT, filtered.len())
            },
            Style::default().fg(state.styles().dim()),
        )));
    }
    lines.push(panel_frame::blank());
    if let Some(buffer) = panel.edit_buffer() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}edit  ", panel_frame::INDENT),
                Style::default().fg(state.styles().accent()).bold(),
            ),
            Span::styled(
                safe_setting_value(buffer),
                Style::default().fg(state.styles().text()),
            ),
        ]));
        lines.push(panel_frame::hint(
            "Enter to save · Esc to cancel",
            state.styles(),
        ));
    } else {
        lines.push(panel_frame::hint(shell.footer(), state.styles()));
    }
    if let Some(notice) = panel.notice() {
        lines.push(Line::from(Span::styled(
            format!("{}{}", panel_frame::INDENT, safe_setting_value(notice)),
            Style::default().fg(if notice.starts_with("saved") {
                state.styles().success()
            } else {
                state.styles().warn()
            }),
        )));
    }
    panel.set_mouse_rows(mouse_rows);
    panel_frame::render(frame, area, state.styles(), lines);
    if shell.tab() == SettingsShellTab::Config
        && shell.focus() == SettingsShellFocus::Search
        && area.height > 4
        && area.width > 8
    {
        let width =
            u16::try_from(unicode_width::UnicodeWidthStr::width(shell.query())).unwrap_or(u16::MAX);
        frame.set_cursor_position((
            area.x
                .saturating_add(7)
                .saturating_add(width)
                .min(area.right().saturating_sub(1)),
            area.y.saturating_add(4),
        ));
    }
}

/// The `⌕ Search settings…` box that opens the settings list.
fn settings_search_box(
    query: &str,
    width: u16,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let inner = usize::from(width)
        .saturating_sub(panel_frame::INDENT.len() * 2 + 2)
        .max(8);
    let body = if query.is_empty() {
        "Search settings…".to_owned()
    } else {
        safe_setting_value(query)
    };
    let body_style = Style::default().fg(if query.is_empty() {
        styles.dim()
    } else {
        styles.text()
    });
    let filled = crate::terminal::truncate_to_width(&format!("⌕ {body}"), inner.saturating_sub(1));
    let pad = inner.saturating_sub(UnicodeWidthStr::width(filled.as_str()) + 1);
    vec![
        Line::from(Span::styled(
            format!("{}╭{}╮", panel_frame::INDENT, "─".repeat(inner)),
            Style::default().fg(styles.border()),
        )),
        Line::from(vec![
            Span::styled(
                format!("{}│ ", panel_frame::INDENT),
                Style::default().fg(styles.border()),
            ),
            Span::styled(filled, body_style),
            Span::styled(
                format!("{}│", " ".repeat(pad)),
                Style::default().fg(styles.border()),
            ),
        ]),
        Line::from(Span::styled(
            format!("{}╰{}╯", panel_frame::INDENT, "─".repeat(inner)),
            Style::default().fg(styles.border()),
        )),
    ]
}

fn safe_setting_value(value: &str) -> String {
    value
        .chars()
        .flat_map(char::escape_default)
        .take(160)
        .collect()
}

fn draw_permission_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    panel_frame::render(
        frame,
        area,
        state.styles(),
        permission_picker_lines_bounded(
            state,
            area.width,
            usize::from(area.height.saturating_sub(1)),
        ),
    );
}

fn permission_picker_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    permission_picker_lines_bounded(state, width, usize::MAX)
}

fn permission_picker_lines_bounded(
    state: &AppState,
    width: u16,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let Some(picker) = state.permission_picker() else {
        return Vec::new();
    };
    let styles = state.styles();
    let header = vec![
        panel_frame::title("Permissions", styles),
        panel_frame::blank(),
        panel_frame::description("Applies to this conversation.", styles),
        panel_frame::blank(),
    ];
    let mut options = Vec::new();
    for (index, row) in picker.rows().iter().enumerate() {
        let selected = picker.selected() == index;
        let mut lines = panel_frame::wrap_option(
            panel_frame::option_spans(
                if selected {
                    panel_frame::Marker::Cursor
                } else {
                    panel_frame::Marker::None
                },
                index + 1,
                vec![Span::styled(
                    row.label.to_owned(),
                    Style::default().fg(if !row.selectable {
                        styles.dim()
                    } else if row.current {
                        styles.success()
                    } else {
                        styles.text()
                    }),
                )],
                row.current,
                styles,
            ),
            width,
        );
        lines.extend(permission_detail_rows(row.description, width, styles));
        if let Some(reason) = row.unavailable_reason {
            lines.extend(permission_detail_rows(reason, width, styles));
        }
        options.push(lines);
    }
    let mut footer = vec![panel_frame::blank()];
    footer.extend(panel_frame::wrap_note(
        "↑/↓ to navigate · Enter to select · Esc to cancel",
        width,
        styles,
    ));
    panel_frame::option_window(header, options, footer, picker.selected(), available_rows)
}

/// Wrap one permission row's explanation under its option label.
fn permission_detail_rows(
    value: &str,
    width: u16,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    panel_frame::wrap_note(value, width.saturating_sub(2), styles)
        .into_iter()
        .map(|line| {
            Line::from(
                std::iter::once(Span::raw("  "))
                    .chain(line.spans)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn draw_route_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    use crate::route_picker::RoutePickerClass;

    let Some(picker) = state.route_picker() else {
        return;
    };
    let card = area;
    let max_rows = usize::from(card.height.saturating_sub(7) / 2).max(1);
    let start = picker.selected().saturating_add(1).saturating_sub(max_rows);
    let end = (start + max_rows).min(picker.matches().len());
    let mut lines = vec![
        Line::from(vec![
            Span::styled("/", Style::default().fg(state.styles().accent()).bold()),
            Span::styled(
                picker.query().to_owned(),
                Style::default().fg(state.styles().text()).bold(),
            ),
            Span::styled(
                format!("  filter: {}", picker.filter().as_str()),
                Style::default().fg(state.styles().dim()),
            ),
        ]),
        Line::from(Span::styled(
            format!(
                "current runtime {} · inference {}",
                picker.current_runtime(),
                picker.current_provider()
            ),
            Style::default().fg(state.styles().dim()),
        )),
        blank(),
    ];
    if picker.is_loading() {
        lines.push(Line::from(Span::styled(
            "Loading live provider/runtime registries…",
            Style::default().fg(state.styles().warn()),
        )));
    } else if let Some(error) = picker.error() {
        lines.push(Line::from(Span::styled(
            format!("Registry unavailable: {error}"),
            Style::default().fg(state.styles().error()),
        )));
    } else if picker.matches().is_empty() {
        lines.push(Line::from(Span::styled(
            "No routes match this search/filter.",
            Style::default().fg(state.styles().warn()),
        )));
    } else {
        for (index, matched) in picker.matches()[start..end].iter().enumerate() {
            let absolute = start + index;
            let selected = absolute == picker.selected();
            let available = matched.row.selection.is_some();
            let class_color = match matched.row.class {
                RoutePickerClass::InferenceApi => ratatui::style::Color::Blue,
                RoutePickerClass::NativeAgent => state.styles().success(),
                RoutePickerClass::DelegatedAgent => state.styles().accent(),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "● " } else { "  " },
                    Style::default().fg(state.styles().accent()),
                ),
                Span::styled(
                    format!("[{}] ", matched.row.class.badge()),
                    Style::default().fg(class_color).bold(),
                ),
                Span::styled(
                    matched.row.display_name.clone(),
                    Style::default()
                        .fg(if available {
                            state.styles().text()
                        } else {
                            state.styles().dim()
                        })
                        .add_modifier(if selected {
                            ratatui::style::Modifier::BOLD
                        } else {
                            ratatui::style::Modifier::empty()
                        }),
                ),
                Span::styled(
                    format!("  {}", matched.row.id),
                    Style::default().fg(state.styles().dim()),
                ),
                Span::styled(
                    if matched.row.current { "  current" } else { "" },
                    Style::default().fg(state.styles().success()),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                format!(
                    "    {}",
                    matched
                        .row
                        .unavailable_reason
                        .as_deref()
                        .unwrap_or(&matched.row.detail)
                ),
                Style::default().fg(if available {
                    state.styles().dim()
                } else {
                    state.styles().warn()
                }),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        "type search · tab filter · ↑↓ choose · enter select · esc close",
        Style::default().fg(state.styles().border()),
    )));
    let block = Block::default()
        .title(Span::styled(
            " Provider & runtime ",
            Style::default().fg(state.styles().accent()).bold(),
        ))
        .borders(Borders::TOP)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().accent()));
    frame.render_widget(Clear, card);
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

/// Paint the U13 panel. The model already produced every line and its tone, so
/// this only maps tones onto the §8 palette.
fn draw_plugin_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    if let Some(panel) = state.plugin_panel() {
        crate::plugin_panel::render::draw(frame, panel, area, state.styles());
    }
}

/// Paint the read-only CMD04 skills/agents/hooks catalog.
fn draw_skills_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(panel) = state.skills_panel() else {
        return;
    };
    panel.draw(frame, area, state.styles());
}

fn draw_capability_catalog(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(panel) = state.capability_catalog() else {
        return;
    };
    if panel.panel() == crate::panel_commands::CapabilityPanel::Agents {
        draw_agent_catalog(frame, panel, area, state.styles());
        return;
    }
    if panel.panel() == crate::panel_commands::CapabilityPanel::Hooks {
        draw_hooks_panel(frame, panel, area, state.styles());
        return;
    }
    let mut lines = vec![Line::from(Span::styled(
        panel.summary().to_owned(),
        Style::default().fg(state.styles().dim()),
    ))];
    if panel.rows().is_empty() {
        lines.push(Line::from(Span::styled(
            "No contributions are registered",
            Style::default().fg(state.styles().dim()),
        )));
    } else {
        // Keep the selected row and the footer visible in short terminals.
        // The flat accessibility projection remains complete; only the
        // visual viewport is windowed.
        let visible_rows = usize::from(area.height.saturating_sub(3)).max(1);
        let start = panel
            .selected()
            .saturating_add(1)
            .saturating_sub(visible_rows)
            .min(panel.rows().len().saturating_sub(visible_rows));
        lines.extend(
            panel
                .rows()
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, row)| {
                    let marker = if index == panel.selected() {
                        "›"
                    } else {
                        " "
                    };
                    let style = if index == panel.selected() {
                        Style::default().fg(state.styles().text()).bold()
                    } else {
                        Style::default().fg(state.styles().text())
                    };
                    Line::from(vec![
                        Span::styled(format!("{marker} {}", row.name()), style),
                        Span::styled(
                            format!("  {}", row.detail()),
                            Style::default().fg(state.styles().dim()),
                        ),
                    ])
                }),
        );
    }
    lines.push(Line::from(Span::styled(
        "↑/↓ or wheel select · Esc close",
        Style::default().fg(state.styles().dim()),
    )));
    let card = area;
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", panel.title()),
            Style::default().fg(state.styles().accent()).bold(),
        ))
        .borders(Borders::TOP)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().accent()));
    frame.render_widget(Clear, card);
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

/// Paint `/hooks`: the read-only registry of lifecycle hook contributions.
///
/// The panel is a viewer, not an editor — heycode registers hooks from plugins
/// and effects, never from this surface — so it carries the reference's
/// read-only remark and a navigate/close hint rather than a confirm hint.
fn draw_hooks_panel(
    frame: &mut Frame<'_>,
    panel: &crate::panel_commands::CapabilityCatalogView,
    area: Rect,
    styles: crate::terminal::Styles,
) {
    let lines = hooks_panel_lines(
        panel,
        area.width,
        styles,
        usize::from(area.height.saturating_sub(1)),
    );
    let seat = panel_frame::anchored(area, lines.len());
    panel_frame::render(frame, seat, styles, lines);
}

fn hooks_panel_lines(
    panel: &crate::panel_commands::CapabilityCatalogView,
    width: u16,
    styles: crate::terminal::Styles,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let total = panel.rows().len();
    let digits = total.max(1).to_string().len();
    let mut header = vec![panel_frame::title(panel.title(), styles)];
    header.extend(panel_frame::wrap_note(panel.summary(), width, styles));
    header.push(panel_frame::blank());
    header.extend(panel_frame::wrap_note(
        "ℹ This panel is read-only. Hooks are contributed by plugins and effects.",
        width,
        styles,
    ));
    header.push(panel_frame::blank());
    if total == 0 {
        header.push(panel_frame::note("No contributions are registered", styles));
    }
    let name_width = panel
        .rows()
        .iter()
        .map(|row| UnicodeWidthStr::width(row.name()))
        .max()
        .unwrap_or(0)
        .min(28);
    let options = panel
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            panel_frame::wrap_option(
                panel_frame::option_spans_padded(
                    if index == panel.selected() {
                        panel_frame::Marker::Cursor
                    } else {
                        panel_frame::Marker::None
                    },
                    index + 1,
                    digits,
                    vec![
                        Span::styled(
                            format!("{:<name_width$}", row.name()),
                            Style::default().fg(styles.accent()),
                        ),
                        Span::styled(
                            format!("   {}", row.detail()),
                            Style::default().fg(styles.dim()),
                        ),
                    ],
                    false,
                    styles,
                ),
                width,
            )
        })
        .collect();
    let mut footer = vec![panel_frame::blank()];
    footer.extend(panel_frame::wrap_note(
        "↑/↓ to navigate · Esc to close",
        width,
        styles,
    ));
    panel_frame::option_window(header, options, footer, panel.selected(), available_rows)
}

/// Paint the delegation catalog inside the shared command-panel frame.
///
/// Claude 2.1.269 answers `/agents` with a receipt saying its subagent wizard
/// was removed. heycode's registered providers and presets are a live capability
/// rather than a wizard, so the panel stays; only its chrome is aligned.
fn draw_agent_catalog(
    frame: &mut Frame<'_>,
    panel: &crate::panel_commands::CapabilityCatalogView,
    area: Rect,
    styles: crate::terminal::Styles,
) {
    let lines = agent_catalog_lines(
        panel,
        area.width,
        styles,
        usize::from(area.height.saturating_sub(1)),
    );
    crate::command_panel_frame::render_anchored(frame, area, styles, lines);
}

fn agent_catalog_lines(
    panel: &crate::panel_commands::CapabilityCatalogView,
    width: u16,
    styles: crate::terminal::Styles,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let mut header = vec![panel_frame::title("Agents", styles), panel_frame::blank()];
    header.extend(panel_frame::wrap_note(panel.summary(), width, styles));
    header.push(panel_frame::blank());
    if panel.rows().is_empty() {
        header.push(panel_frame::note(
            "No agent providers are registered.",
            styles,
        ));
    }
    let options = panel
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let selected = index == panel.selected();
            let readiness = row.detail().split(" — ").next().unwrap_or("Unknown");
            panel_frame::wrap_option(
                panel_frame::option_spans(
                    if selected {
                        panel_frame::Marker::Cursor
                    } else {
                        panel_frame::Marker::None
                    },
                    index + 1,
                    vec![
                        Span::styled(
                            row.name().to_owned(),
                            Style::default().fg(if selected {
                                styles.accent()
                            } else {
                                styles.text()
                            }),
                        ),
                        Span::styled(format!(" · {readiness}"), Style::default().fg(styles.dim())),
                    ],
                    false,
                    styles,
                ),
                width,
            )
        })
        .collect();
    let mut footer = Vec::new();
    if let Some(row) = panel.rows().get(panel.selected()) {
        let setup = if row.detail().starts_with("NeedsAuthentication") {
            " Use this provider's setup to authenticate."
        } else {
            ""
        };
        footer.push(panel_frame::blank());
        footer.extend(panel_frame::wrap_note(
            &format!("{}: {}{setup}", row.name(), row.detail()),
            width,
            styles,
        ));
    }
    footer.push(panel_frame::blank());
    footer.extend(panel_frame::wrap_note(
        "↑/↓ to select · r to recheck · c to cancel probes · Esc to close",
        width,
        styles,
    ));
    panel_frame::option_window(header, options, footer, panel.selected(), available_rows)
}

/// Paint the U12 panel. The model already produced every line and its tone, so
/// this only maps tones onto the §8 palette.
fn draw_mcp_panel(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    use crate::mcp_panel::McpPanelTone;

    let Some(panel) = state.mcp_panel() else {
        return;
    };
    let card = area;
    let styles = state.styles();
    // Rule, title and a spacer sit above the model's own rows.
    let body = Rect::new(
        card.x,
        card.y.saturating_add(3),
        card.width,
        card.height.saturating_sub(3),
    );
    panel.set_mouse_layout(Rect::new(
        body.x
            .saturating_add(u16::try_from(crate::panel_frame::INDENT.len()).unwrap_or(3)),
        body.y,
        body.width
            .saturating_sub(u16::try_from(crate::panel_frame::INDENT.len()).unwrap_or(3)),
        body.height,
    ));
    let mut lines = vec![
        crate::panel_frame::title("MCP servers", styles),
        crate::panel_frame::blank(),
    ];
    lines.extend(panel.lines_for_height(body.height).into_iter().map(|line| {
        let style = match line.tone {
            McpPanelTone::Heading => Style::default().fg(state.styles().accent()).bold(),
            McpPanelTone::Body => Style::default().fg(state.styles().text()),
            McpPanelTone::Dim => Style::default().fg(state.styles().dim()),
            McpPanelTone::Good => Style::default().fg(state.styles().success()),
            McpPanelTone::Warn => Style::default().fg(state.styles().warn()),
            McpPanelTone::Bad => Style::default().fg(state.styles().error()),
            McpPanelTone::Selected => Style::default().fg(state.styles().text()).bold(),
        };
        Line::from(Span::styled(
            format!("{}{}", crate::panel_frame::INDENT, line.text),
            style,
        ))
    }));
    crate::panel_frame::render(frame, card, styles, lines);
}

fn draw_model_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    use heycode_llm::{CapabilitySupport, CatalogFreshness, ModelLifecycleStatus};

    let Some(picker) = state.model_picker.as_ref() else {
        return;
    };
    let styles = state.styles();
    let width = usize::from(area.width.saturating_sub(6)).max(1);
    let mut lines = vec![
        Line::from(Span::styled(
            "   Select model",
            Style::default().fg(styles.accent()).bold(),
        )),
        Line::from(Span::styled(
            format!(
                "   Switch between {} models. Enter saves your choice for new sessions.",
                crate::task_console::safe(picker.provider())
            ),
            Style::default().fg(styles.dim()),
        )),
        blank(),
    ];
    let max_rows = usize::from(area.height.saturating_sub(11)).max(1);
    let start = picker.selected().saturating_add(1).saturating_sub(max_rows);
    let end = (start + max_rows).min(picker.matches().len());
    let model_width = picker.matches()[start..end]
        .iter()
        .map(|row| row.model.id.width())
        .max()
        .unwrap_or(1)
        .min(width.saturating_sub(16).max(1));
    if picker.matches().is_empty() {
        lines.push(Line::from(Span::styled(
            if matches!(picker.state(), crate::app::ModelPickerLoadState::Loading) {
                "   Loading model catalog…"
            } else {
                "   No matching selectable models"
            },
            Style::default().fg(styles.dim()),
        )));
    } else {
        for (index, row) in picker.matches()[start..end].iter().enumerate() {
            let absolute_index = start + index;
            let current = row.model.id == picker.current_model();
            let id = crate::terminal::truncate_to_width(&row.model.id, model_width);
            let padding = model_width.saturating_sub(id.width());
            let description = if row.model.display_name == row.model.id {
                String::new()
            } else {
                row.model.display_name.clone()
            };
            let suffix = if current { " ✓ current" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(
                    if absolute_index == picker.selected() {
                        "   ❯ "
                    } else {
                        "     "
                    },
                    Style::default().fg(styles.accent()),
                ),
                Span::styled(
                    format!("{}. ", absolute_index + 1),
                    Style::default().fg(styles.dim()),
                ),
                Span::styled(
                    format!("{id}{suffix}"),
                    Style::default().fg(if current {
                        styles.success()
                    } else {
                        styles.text()
                    }),
                ),
                Span::styled(
                    format!("{}  {description}", " ".repeat(padding)),
                    Style::default().fg(styles.dim()),
                ),
            ]));
        }
    }
    lines.push(blank());
    if let Some(effort) = picker.effort_picker() {
        let choice = effort
            .choices()
            .get(effort.selected())
            .map_or("unavailable", String::as_str);
        lines.push(Line::from(vec![
            Span::styled("   ● ", Style::default().fg(styles.code())),
            Span::styled(
                format!(
                    "{choice} effort{}",
                    if Some(choice) == effort.default_effort() {
                        " (default)"
                    } else {
                        ""
                    }
                ),
                Style::default().fg(styles.dim()),
            ),
            Span::styled(" ←/→ to adjust", Style::default().fg(styles.dim())),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            if picker.effort_error().is_some() {
                "   Effort unavailable for this model"
            } else {
                "   Loading effort options…"
            },
            Style::default().fg(styles.dim()),
        )));
    }
    if let Some(row) = picker.matches().get(picker.selected()) {
        let lifecycle = match row.model.lifecycle.effective_status(current_unix_ms()) {
            ModelLifecycleStatus::Unknown => "unknown lifecycle",
            ModelLifecycleStatus::Stable => "stable",
            ModelLifecycleStatus::Preview => "preview",
            ModelLifecycleStatus::Deprecated => "deprecated",
            ModelLifecycleStatus::Retired => "retired",
        };
        let capability = |label: &str, support: CapabilitySupport| match support {
            CapabilitySupport::Supported => format!("{label}✓"),
            CapabilitySupport::Unsupported => format!("{label}–"),
            CapabilitySupport::Unknown => format!("{label}?"),
        };
        lines.push(Line::from(Span::styled(
            format!(
                "   {lifecycle} · {} · {}{}",
                capability("tools", row.model.capabilities.tools),
                capability("reason", row.model.capabilities.reasoning),
                model_override_suffix(row)
            ),
            Style::default().fg(if row.has_contradiction {
                styles.warn()
            } else {
                styles.dim()
            }),
        )));
    }
    let source = match picker.state() {
        crate::app::ModelPickerLoadState::Loading => "refreshing".to_owned(),
        crate::app::ModelPickerLoadState::Ready {
            freshness, warning, ..
        } => format!(
            "{}{}{}",
            match freshness {
                CatalogFreshness::Live => "live",
                CatalogFreshness::FreshCache => "fresh cache",
                CatalogFreshness::StaleFallback => "stale fallback",
            },
            warning
                .as_deref()
                .map(|warning| format!(" · {warning}"))
                .unwrap_or_default(),
            if picker.unmatched_overrides().is_empty() {
                String::new()
            } else {
                format!(
                    " · {} unmatched override(s)",
                    picker.unmatched_overrides().len()
                )
            }
        ),
        crate::app::ModelPickerLoadState::Error { message } => format!("error: {message}"),
    };
    lines.push(Line::from(Span::styled(
        format!(
            "   /{} · {} · source: {source}",
            picker.query(),
            picker.filter().as_str()
        ),
        Style::default().fg(
            if matches!(
                picker.state(),
                crate::app::ModelPickerLoadState::Error { .. }
            ) {
                styles.error()
            } else {
                styles.dim()
            },
        ),
    )));
    lines.push(blank());
    lines.push(Line::from(Span::styled(
        if area.width < 80 {
            "   Enter default · s session · Esc cancel"
        } else {
            "   Enter to set as default · s to use this session only · Esc to cancel"
        },
        Style::default().fg(styles.dim()),
    )));
    lines.push(Line::from(Span::styled(
        "   ↑↓ choose · / search · tab filter · ctrl+r refresh",
        Style::default().fg(styles.dim()),
    )));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(styles.accent())),
        ),
        area,
    );
}

fn draw_effort_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(picker) = state.effort_picker.as_ref() else {
        return;
    };
    crate::effort_render::draw(
        frame,
        picker,
        state.active_model_label(),
        state.styles(),
        area,
    );
}

fn model_override_suffix(row: &crate::model_picker::ModelPickerMatch) -> String {
    let Some(first) = row.assertions.first() else {
        return String::new();
    };
    let mut first = first.chars().take(72).collect::<String>();
    if first.chars().count() < row.assertions[0].chars().count() {
        first.push('…');
    }
    format!(
        " · {}override: {}{}",
        if row.has_contradiction {
            "CONFLICT "
        } else {
            ""
        },
        first,
        if row.assertions.len() > 1 {
            format!(" (+{})", row.assertions.len() - 1)
        } else {
            String::new()
        }
    )
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0)
}

fn draw_command_palette(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(view) = state.command_palette() else {
        return;
    };
    let styles = state.styles();
    let capacity = usize::from(area.height.saturating_sub(1));
    // A typo/description-only query deliberately has no implicit highlight.
    // `NO_HIGHLIGHT` is `usize::MAX`, so it must never feed paging arithmetic
    // or a slice range.
    let anchor = view.highlighted().unwrap_or(0);
    let start = anchor
        .saturating_add(1)
        .saturating_sub(capacity.max(1))
        .min(view.matches().len());
    let end = start.saturating_add(capacity).min(view.matches().len());
    let name_width = view
        .matches()
        .iter()
        .map(|row| row.entry.descriptor.synopsis().chars().count())
        .max()
        .unwrap_or(0)
        .min(36)
        .min(usize::from(area.width / 2));
    let mut lines = Vec::new();
    if view.matches().is_empty() {
        lines.push(Line::from(Span::styled(
            " No matching commands",
            Style::default().fg(styles.warn()),
        )));
    } else {
        for (index, row) in view.matches()[start..end].iter().enumerate() {
            let selected = start + index == view.selected();
            let name = row.entry.descriptor.synopsis();
            let padding = " ".repeat(name_width.saturating_sub(name.chars().count()) + 2);
            let detail = row
                .entry
                .availability
                .reason()
                .unwrap_or_else(|| row.entry.descriptor.description());
            let color = if selected {
                styles.accent()
            } else {
                styles.text()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "❯ " } else { "  " },
                    Style::default().fg(color),
                ),
                Span::styled(
                    name,
                    Style::default().fg(color).add_modifier(if selected {
                        ratatui::style::Modifier::BOLD
                    } else {
                        ratatui::style::Modifier::empty()
                    }),
                ),
                Span::raw(padding),
                Span::styled(
                    detail.to_owned(),
                    Style::default().fg(if row.entry.availability.is_available() {
                        styles.dim()
                    } else {
                        styles.warn()
                    }),
                ),
            ]));
        }
    }
    lines.push(Line::from(Span::styled(
        " ↑↓ choose · enter select · esc close",
        Style::default().fg(styles.dim()),
    )));
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_secret_prompt(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(secret) = state.pending_secret.as_ref() else {
        return;
    };
    let width = area.width.saturating_sub(4).clamp(34, 68);
    let card = centered_dialog_rect(area, width, 13.min(area.height));
    frame.render_widget(Clear, card);
    let shown = secret.masked_preview(usize::from(card.width.saturating_sub(3)));
    let lines = vec![
        Line::from(Span::styled(
            "Connect credential",
            Style::default().fg(state.styles().accent()).bold(),
        )),
        blank(),
        Line::from(Span::styled(
            secret.prompt.clone(),
            Style::default().fg(state.styles().text()),
        )),
        Line::from(Span::styled(
            format!("Reference: {}", secret.reference),
            Style::default().fg(state.styles().dim()),
        )),
        blank(),
        Line::from(vec![
            Span::styled(shown, Style::default().fg(state.styles().text())),
            Span::styled("▏", Style::default().fg(state.styles().accent())),
        ]),
        blank(),
        Line::from(Span::styled(
            secret
                .error
                .as_deref()
                .unwrap_or("Type or paste a key. First 5 and last character shown."),
            Style::default().fg(if secret.error.is_some() {
                state.styles().warn()
            } else {
                state.styles().dim()
            }),
        )),
        blank(),
        Line::from(Span::styled(
            "enter validate & save · esc cancel",
            Style::default().fg(state.styles().border()),
        )),
    ];
    let block = Block::default()
        .title(Span::styled(
            " masked input ",
            Style::default().fg(state.styles().accent()).bold(),
        ))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().accent()));
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

/// Title the in-session panel carries for one wizard step.
///
/// `/login` is an alias for `/connect`, and the reference titles the first page
/// of that flow `Login`. Later pages keep the wizard's own page title, because
/// they are choosing a model or a server rather than a sign-in method.
fn onboarding_panel_title(step: heycode_onboarding::OnboardingStep, fallback: &str) -> &str {
    if step == heycode_onboarding::OnboardingStep::RuntimeClass {
        "Login"
    } else {
        fallback
    }
}

/// The prompt row above the numbered options.
fn onboarding_panel_prompt(step: heycode_onboarding::OnboardingStep) -> &'static str {
    if step == heycode_onboarding::OnboardingStep::RuntimeClass {
        "Select login method:"
    } else {
        "Select an option:"
    }
}

/// Compose the in-session `/login` panel body.
fn onboarding_panel_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    onboarding_panel_lines_bounded(state, width, usize::MAX)
}

fn onboarding_panel_lines_bounded(
    state: &AppState,
    width: u16,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let Some(onboarding) = state.onboarding.as_ref() else {
        return Vec::new();
    };
    let styles = state.styles();
    let mut lines = vec![
        crate::panel_frame::title(
            onboarding_panel_title(onboarding.step, onboarding.title),
            styles,
        ),
        crate::panel_frame::blank(),
    ];
    let body = onboarding
        .input
        .as_ref()
        .map_or(onboarding.body, |input| input.description.as_str());
    lines.extend(crate::panel_frame::wrap(
        body,
        width,
        Style::default().fg(styles.text()),
    ));
    if let Some(notice) = state.onboarding_notice.as_deref().filter(|s| !s.is_empty()) {
        lines.extend(crate::panel_frame::wrap(
            notice,
            width,
            Style::default().fg(styles.warn()),
        ));
    }
    if let Some(query) = onboarding.search.as_ref() {
        lines.push(crate::panel_frame::blank());
        let text = if onboarding.step == heycode_onboarding::OnboardingStep::Endpoint {
            format!("Server URL: {query}")
        } else if let Some(input) = onboarding.input.as_ref() {
            format!(
                "{} ({}/{}): {query}",
                input.label, input.position, input.total
            )
        } else if query.is_empty() {
            "Search: type to filter".to_owned()
        } else {
            format!("Search: {query}")
        };
        lines.push(Line::from(Span::styled(
            format!("{}{text}", crate::panel_frame::INDENT),
            Style::default().fg(styles.text()),
        )));
    }
    lines.push(crate::panel_frame::blank());
    lines.push(Line::from(Span::styled(
        format!(
            "{}{}",
            crate::panel_frame::INDENT,
            onboarding_panel_prompt(onboarding.step)
        ),
        Style::default().fg(styles.text()),
    )));
    lines.push(crate::panel_frame::blank());
    if onboarding.options.is_empty() {
        lines.push(crate::panel_frame::note(
            "No matches. Edit your search, or press Esc to cancel.",
            styles,
        ));
    }
    let header = lines;
    let mut options = Vec::new();
    for (index, option) in onboarding.options.iter().enumerate() {
        let selected = index == onboarding.selected;
        let mut label = vec![Span::styled(
            option.label.clone(),
            Style::default().fg(if selected {
                styles.accent()
            } else {
                styles.text()
            }),
        )];
        if !option.description.is_empty() {
            label.push(Span::styled(
                " · ",
                Style::default().fg(if selected {
                    styles.accent()
                } else {
                    styles.dim()
                }),
            ));
            label.push(Span::styled(
                option.description.clone(),
                Style::default().fg(styles.dim()),
            ));
        }
        options.push(panel_frame::wrap_option(
            crate::panel_frame::option_spans(
                if selected {
                    crate::panel_frame::Marker::Cursor
                } else {
                    crate::panel_frame::Marker::None
                },
                index.saturating_add(1),
                label,
                false,
                styles,
            ),
            width,
        ));
    }
    let footer = vec![
        crate::panel_frame::blank(),
        crate::panel_frame::hint("Esc to cancel", styles),
    ];
    // Preserve the wide eight-row list, but measure rows after label wrapping.
    let available_rows =
        available_rows.min(header.len().saturating_add(footer.len()).saturating_add(8));
    panel_frame::option_window(header, options, footer, onboarding.selected, available_rows)
}

/// Rows the in-session `/login` panel needs, including its top rule.
fn onboarding_panel_height(state: &AppState, width: u16) -> u16 {
    crate::panel_frame::height(onboarding_panel_lines(state, width).len())
}

fn draw_onboarding(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    if state.onboarding_is_panel() {
        crate::panel_frame::render(
            frame,
            area,
            state.styles(),
            onboarding_panel_lines_bounded(
                state,
                area.width,
                usize::from(area.height.saturating_sub(1)),
            ),
        );
        return;
    }
    let Some(onboarding) = state.onboarding.as_ref() else {
        return;
    };
    let width = area.width.saturating_sub(4).min(82);
    let notice = state.onboarding_notice.as_deref().unwrap_or("");
    let notice_rows = if notice.is_empty() {
        0
    } else {
        notice
            .chars()
            .count()
            .div_ceil(usize::from(width.saturating_sub(8).max(1)))
            .clamp(1, 4) as u16
            + 1
    };
    let search_rows = u16::from(onboarding.search.is_some()) * 2;
    let desired = 9usize
        .saturating_add(usize::from(search_rows))
        .saturating_add(onboarding.options.len().max(1).saturating_mul(3))
        .saturating_add(usize::from(notice_rows));
    let height = u16::try_from(desired)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let card = centered_dialog_rect(area, width, height);
    frame.render_widget(Clear, card);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(state.styles().border())),
        card,
    );
    let inner = Rect::new(
        card.x.saturating_add(3),
        card.y.saturating_add(2),
        card.width.saturating_sub(6),
        card.height.saturating_sub(4),
    );
    if inner.height < 4 || inner.width < 8 {
        return;
    }
    frame.render_widget(
        Paragraph::new(onboarding.title).style(Style::default().fg(state.styles().text()).bold()),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let body = onboarding
        .input
        .as_ref()
        .map_or(onboarding.body, |input| input.description.as_str());
    frame.render_widget(
        Paragraph::new(body).style(Style::default().fg(state.styles().dim())),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let notice_rows = notice_rows.min(inner.height.saturating_sub(6));
    if notice_rows > 0 {
        frame.render_widget(
            Paragraph::new(notice)
                .style(Style::default().fg(state.styles().warn()))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            Rect::new(inner.x, inner.y + 3, inner.width, notice_rows - 1),
        );
    }
    if let Some(query) = onboarding.search.as_ref() {
        let text = if onboarding.step == heycode_onboarding::OnboardingStep::Endpoint {
            format!("Server URL: {query}")
        } else if let Some(input) = onboarding.input.as_ref() {
            format!(
                "{} ({}/{}): {query}",
                input.label, input.position, input.total
            )
        } else if query.is_empty() {
            "Search: type to filter".to_owned()
        } else {
            format!("Search: {query}")
        };
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(state.styles().text())),
            Rect::new(inner.x, inner.y + 3 + notice_rows, inner.width, 1),
        );
    }
    let options_y = inner.y + 3 + notice_rows + search_rows;
    let available = inner.height.saturating_sub(5 + notice_rows + search_rows);
    if onboarding.options.is_empty() && available > 0 {
        frame.render_widget(
            Paragraph::new("No matches. Edit your search or press Esc to go back.")
                .style(Style::default().fg(state.styles().dim())),
            Rect::new(inner.x, options_y, inner.width, 1),
        );
    }
    let row_height = if available >= 3 { 3 } else { 1 };
    let visible = usize::from((available / row_height).max(1));
    let start = onboarding
        .selected
        .saturating_sub(visible.saturating_sub(1));
    for (offset, (index, option)) in onboarding
        .options
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let y = options_y
            + u16::try_from(offset)
                .unwrap_or(u16::MAX)
                .saturating_mul(row_height);
        let selected = index == onboarding.selected;
        let title_style = if selected {
            Style::default()
                .fg(state.styles().accent())
                .reversed()
                .bold()
        } else {
            Style::default().fg(state.styles().text()).bold()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}",
                if selected { "›" } else { " " },
                option.label
            ))
            .style(title_style),
            Rect::new(inner.x, y, inner.width, 1),
        );
        if row_height > 1 {
            frame.render_widget(
                Paragraph::new(format!("  {}", option.description))
                    .style(Style::default().fg(state.styles().dim())),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
        }
    }
    let escape = if matches!(
        onboarding.step,
        heycode_onboarding::OnboardingStep::Welcome | heycode_onboarding::OnboardingStep::Reconnect
    ) {
        "quit"
    } else {
        "back"
    };
    let footer = format!("↑↓ move   Enter select   Esc {escape}");
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(state.styles().dim())),
        Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
    );
}

fn centered_dialog_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn draw_theme_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    panel_frame::render(
        frame,
        area,
        state.styles(),
        theme_picker_lines_bounded(
            state,
            area.width,
            usize::from(area.height.saturating_sub(1)),
        ),
    );
}

fn theme_picker_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    theme_picker_lines_bounded(state, width, usize::MAX)
}

fn theme_picker_lines_bounded(
    state: &AppState,
    width: u16,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let Some(picker) = state.theme_picker() else {
        return Vec::new();
    };
    let styles = state.styles();
    let mut header = vec![panel_frame::title("Theme", styles), panel_frame::blank()];
    header.extend(panel_frame::wrap(
        "Choose the text style that looks best with your terminal",
        width,
        Style::default().fg(styles.text()).bold(),
    ));
    header.push(panel_frame::blank());
    let mut options = Vec::new();
    for (index, theme) in picker.themes().iter().enumerate() {
        options.push(panel_frame::wrap_option(
            panel_frame::option_spans(
                if index == picker.selected() {
                    panel_frame::Marker::Cursor
                } else {
                    panel_frame::Marker::None
                },
                index + 1,
                vec![
                    Span::styled(
                        theme.title().to_owned(),
                        Style::default().fg(if index == picker.current() {
                            styles.success()
                        } else {
                            styles.text()
                        }),
                    ),
                    Span::styled(
                        format!(" · {}", theme.id().as_str()),
                        Style::default().fg(styles.dim()),
                    ),
                ],
                index == picker.current(),
                styles,
            ),
            width,
        ));
    }
    let mut lines = vec![panel_frame::blank()];
    lines.push(panel_frame::preview_rule(width, styles));
    lines.extend(theme_preview_rows(width, styles));
    lines.push(panel_frame::preview_rule(width, styles));
    lines.push(panel_frame::blank());
    lines.extend(panel_frame::wrap_note(
        "Enter to select · Esc to cancel",
        width,
        styles,
    ));
    panel_frame::option_window(header, options, lines, picker.selected(), available_rows)
}

/// Rows in the theme preview: what a diff looks like under this theme.
///
/// The reference fences a four-line diff between dashed rules so the added
/// and removed roles are visible before the theme is committed. heycode has the
/// same two roles, so the block carries the same shape with heycode's own text.
fn theme_preview_rows(width: u16, styles: crate::terminal::Styles) -> Vec<Line<'static>> {
    // A changed row carries its background to the panel edge, as the source
    // does; a background that stopped at the last glyph would read as a
    // highlight on the text rather than on the line.
    let row_width = usize::from(width).saturating_sub(panel_frame::INDENT.len());
    [
        (1, ' ', "function greet() {"),
        (2, '-', "  console.log(\"Hello, World!\");"),
        (2, '+', "  console.log(\"Hello, heycode!\");"),
        (3, ' ', "}"),
    ]
    .into_iter()
    .map(|(number, sign, text): (u8, char, &str)| {
        if sign == ' ' {
            Line::from(vec![
                Span::styled(
                    format!("{} {number}  ", panel_frame::INDENT),
                    Style::default().fg(styles.dim()),
                ),
                Span::styled(text.to_owned(), Style::default().fg(styles.text())),
            ])
        } else {
            let added = sign == '+';
            let padding = row_width.saturating_sub(4 + UnicodeWidthStr::width(text));
            Line::from(vec![
                Span::styled(
                    format!("{} {number} {sign}", panel_frame::INDENT),
                    Style::default().fg(styles.diff_marker(added)),
                ),
                Span::styled(
                    format!("{text}{}", " ".repeat(padding)),
                    Style::default().fg(styles.text()),
                ),
            ])
            .style(Style::default().bg(styles.diff_background(added)))
        }
    })
    .collect()
}

fn draw_keymap_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let lines = keymap_picker_lines(
        state,
        area.width,
        usize::from(area.height.saturating_sub(1)),
    );
    panel_frame::render(frame, area, state.styles(), lines);
}

fn keymap_picker_lines(state: &AppState, width: u16, available_rows: usize) -> Vec<Line<'static>> {
    let Some(picker) = state.keymap_picker() else {
        return Vec::new();
    };
    let styles = state.styles();
    let total = picker.rows().len();
    let digits = total.to_string().len();
    let mut header = vec![
        panel_frame::title("Keybindings", styles),
        panel_frame::note(
            &format!("{total} bindings · settings revision {}", picker.revision()),
            styles,
        ),
        panel_frame::blank(),
    ];
    header.extend(panel_frame::wrap_note(
        "ℹ Enter loads the selected action into /keymap; the command commits the change.",
        width,
        styles,
    ));
    header.push(panel_frame::blank());
    let options = picker
        .rows()
        .iter()
        .enumerate()
        .map(|(index, (action, chord))| {
            panel_frame::wrap_option(
                panel_frame::option_spans_padded(
                    if index == picker.selected() {
                        panel_frame::Marker::Cursor
                    } else {
                        panel_frame::Marker::None
                    },
                    index + 1,
                    digits,
                    vec![
                        Span::styled(
                            format!("{:<24}", action.as_str()),
                            Style::default().fg(styles.text()),
                        ),
                        Span::styled(chord.to_string(), Style::default().fg(styles.dim())),
                    ],
                    false,
                    styles,
                ),
                width,
            )
        })
        .collect();
    let mut footer = vec![panel_frame::blank()];
    footer.extend(panel_frame::wrap_note(
        "↑/↓ to navigate · Enter to edit · Esc to close",
        width,
        styles,
    ));
    panel_frame::option_window(header, options, footer, picker.selected(), available_rows)
}

fn draw_scroll_speed_picker(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(picker) = state.scroll_speed_picker() else {
        return;
    };
    let styles = state.styles();
    let filled = usize::from(picker.quarters().div_ceil(4)).clamp(1, SCROLL_SPEED_GAUGE);
    let gauge = "■".repeat(filled) + &"·".repeat(SCROLL_SPEED_GAUGE - filled);
    let lines = vec![
        panel_frame::title("Scroll speed", styles),
        panel_frame::blank(),
        Line::from(vec![
            Span::styled(
                format!("{}{gauge}  ", panel_frame::INDENT),
                Style::default().fg(styles.accent()),
            ),
            Span::styled(
                format!("{}× per wheel notch", format_scroll_speed(picker.speed())),
                Style::default().fg(styles.text()),
            ),
            Span::styled(
                if picker.quarters() == 4 {
                    " (default)".to_owned()
                } else {
                    String::new()
                },
                Style::default().fg(styles.dim()),
            ),
        ]),
        panel_frame::blank(),
        panel_frame::note(
            &format!(
                "Preview     {} rows moved · settings revision {}",
                picker.preview_offset(),
                picker.revision()
            ),
            styles,
        ),
        panel_frame::blank(),
        panel_frame::hint(
            "Scroll to feel it · ←/→ adjust · r reset to default · Enter save · Esc cancel",
            styles,
        ),
    ];
    panel_frame::render(frame, area, styles, lines);
}

/// Cells in the `/scroll-speed` gauge.
const SCROLL_SPEED_GAUGE: usize = 10;

// ─── transcript ─────────────────────────────────────────────────────────────

fn draw_transcript(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    if state.focus_view() {
        draw_focus_transcript(frame, state, area);
        return;
    }
    // The modal owns its opening command until it closes. Preserve the
    // underlying item and stable indices while omitting that last receipt.
    let hide_opening_command = state.items.last().is_some_and(|item| {
        matches!(item, Item::Command(text) if match text.trim() {
            "/skills" => state.skills_panel().is_some(),
            "/memory" => state.memory_panel.is_some(),
            "/model" => state.model_picker.is_some(),
            "/effort" => state.effort_picker.is_some(),
            "/sandbox" => state.sandbox_panel().is_some(),
            "/autocompact" => state.autocompact_panel().is_some(),
            _ => false,
        })
    });
    let visible_items = &state.items[..state
        .items
        .len()
        .saturating_sub(usize::from(hide_opening_command))];
    let width = usize::from(area.width);
    let show_reasoning = state.show_reasoning;
    let styles = state.styles();
    let style_generation = state.transcript_style_generation();
    let mut cache = std::mem::take(&mut state.transcript_cache);
    let anchor_spec = crate::transcript::ViewportSpec {
        width,
        visible_lines: usize::from(area.height),
        scroll_from_bottom: state.scroll_from_bottom,
        show_reasoning,
        style_generation,
    };
    if let Some((index, row)) = state.reasoning_reveal.take() {
        state.scroll_from_bottom = cache.scroll_for_row(
            visible_items,
            anchor_spec,
            index,
            0,
            row,
            |item, neighbors| {
                render_transcript_item(item, neighbors, width, show_reasoning, styles)
            },
        );
    } else if state.scroll_from_bottom > 0
        && state.scroll_from_bottom != usize::MAX
        && cache.last_scroll == state.scroll_from_bottom
        && cache.last_width == width
        && let Some((index, row)) = cache.visible_rows.first().copied()
        && index >= state.items.len().saturating_sub(32)
    {
        // Preserve the visible anchor as recent streaming content grows.
        // Far-history jumps retain the existing bounded viewport index.
        state.scroll_from_bottom = cache.scroll_for_row(
            visible_items,
            anchor_spec,
            index,
            row,
            0,
            |item, neighbors| {
                render_transcript_item(item, neighbors, width, show_reasoning, styles)
            },
        );
    }
    let viewport = cache.viewport(
        visible_items,
        crate::transcript::ViewportSpec {
            width,
            visible_lines: usize::from(area.height),
            scroll_from_bottom: state.scroll_from_bottom,
            show_reasoning,
            style_generation,
        },
        |item, neighbors| render_transcript_item(item, neighbors, width, show_reasoning, styles),
    );
    state.transcript_area = area;
    state.reasoning_hit_rows = cache
        .visible_rows
        .iter()
        .enumerate()
        .filter_map(|(row, (index, local_row))| {
            (*local_row == 0
                && state
                    .items
                    .get(*index)
                    .is_some_and(|item| item.group_parent().is_none())
                && matches!(
                    state.items.get(*index),
                    Some(
                        Item::Reasoning { .. }
                            | Item::Tool { .. }
                            | Item::FindingsReport { .. }
                            | Item::Compaction { .. }
                    )
                ))
            .then_some((
                area.y
                    .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
                *index,
            ))
        })
        .collect();
    for (visible_row, (index, local_row)) in cache.visible_rows.iter().enumerate() {
        if let Some(Item::Tool { view, .. }) = state.items.get(*index)
            && !view.expanded
            && !view.spawn_tree().is_empty()
        {
            let mut row = 1;
            for child in view.spawn_tree() {
                if *local_row == row {
                    state.task_console.hits.push((
                        Rect::new(area.x, area.y + visible_row as u16, area.width, 1),
                        crate::task_console::TaskHit::Open(child.key.clone()),
                    ));
                }
                row += 1 + usize::from(child.telemetry.current_tool.is_some());
            }
        }
    }
    state.transcript_cache = cache;
    frame.render_widget(Paragraph::new(viewport), area);
}

#[derive(Default)]
struct FocusToolSummary {
    item_count: usize,
    reads: usize,
    edits: usize,
    added: u64,
    removed: u64,
    other: usize,
    failures: usize,
    running: Vec<String>,
}

fn draw_focus_transcript(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let width = usize::from(area.width.max(1));
    let styles = state.styles();
    let start = state
        .items
        .iter()
        .rposition(|item| matches!(item, Item::User(_)))
        .unwrap_or(0);
    let turn = &state.items[start..];
    let mut lines = Vec::new();
    let mut shown = 0usize;

    if let Some(user) = turn.iter().find(|item| matches!(item, Item::User(_))) {
        lines.extend(render_transcript_item(
            user,
            ItemNeighbors::default(),
            width,
            false,
            styles,
        ));
        shown += 1;
    }

    let summary = focus_tool_summary(turn);
    if let Some(rendered_summary) = render_focus_tool_summary(&summary, width, styles) {
        lines.push(rendered_summary);
        lines.push(blank());
        shown = shown.saturating_add(summary.item_count);
    }

    if let Some(answer) = turn
        .iter()
        .rev()
        .find(|item| matches!(item, Item::Assistant(_) | Item::Error(_)))
    {
        lines.extend(render_transcript_item(
            answer,
            ItemNeighbors::default(),
            width,
            false,
            styles,
        ));
        shown += 1;
    }

    let commands = turn
        .iter()
        .filter(|item| matches!(item, Item::Command(_)))
        .collect::<Vec<_>>();
    shown = shown.saturating_add(commands.len());
    let hidden = state.items.len().saturating_sub(shown);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "✻ Focus view · {hidden} transcript item{} hidden (/focus to show)",
                if hidden == 1 { "" } else { "s" }
            ),
            Style::default().fg(styles.dim()),
        )));
    }
    for command in commands {
        lines.push(blank());
        lines.extend(render_transcript_item(
            command,
            ItemNeighbors::default(),
            width,
            false,
            styles,
        ));
    }
    if let Some(notice) = turn.iter().rev().find_map(|item| match item {
        Item::Info(text)
            if text.starts_with("Focus view ") || text.starts_with("Session color ") =>
        {
            Some(text)
        }
        _ => None,
    }) {
        lines.push(blank());
        lines.push(Line::from(vec![
            Span::styled("↳ ", Style::default().fg(styles.dim())),
            Span::styled(notice.clone(), Style::default().fg(styles.text())),
        ]));
    }

    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    let height = usize::from(area.height);
    let (from, to) = if state.scroll_from_bottom == usize::MAX {
        (0, lines.len().min(height))
    } else {
        let to = lines.len().saturating_sub(state.scroll_from_bottom);
        (to.saturating_sub(height), to)
    };
    state.transcript_area = area;
    state.reasoning_hit_rows.clear();
    frame.render_widget(Paragraph::new(lines[from..to].to_vec()), area);
}

fn focus_tool_summary(items: &[Item]) -> FocusToolSummary {
    let mut summary = FocusToolSummary::default();
    for item in items {
        match item {
            Item::Tool {
                name,
                args,
                result,
                view,
                ..
            } if !view.group_hidden && !view.merged => {
                summary.item_count += 1;
                if result.is_none() {
                    summary.running.push(tool_display_name(name).to_owned());
                }
                if result.as_ref().is_some_and(|(ok, _)| !*ok) {
                    summary.failures += 1;
                }
                match canonical_tool_name(name) {
                    "read" => summary.reads += 1,
                    "read_many" => {
                        summary.reads += args
                            .get("files")
                            .and_then(serde_json::Value::as_array)
                            .map_or(1, Vec::len);
                    }
                    "write" | "edit" | "multi_edit" => {
                        let (files, added, removed) = result
                            .as_ref()
                            .map_or((1, 0, 0), |(_, value)| focus_edit_stats(value));
                        summary.edits += files;
                        summary.added = summary.added.saturating_add(added);
                        summary.removed = summary.removed.saturating_add(removed);
                    }
                    _ => summary.other += 1,
                }
            }
            Item::ServerTool { result, .. } => {
                summary.item_count += 1;
                summary.other += 1;
                if result.as_ref().is_some_and(|result| {
                    result.outcome() == heycode_core::ServerToolOutcome::Error
                }) {
                    summary.failures += 1;
                }
            }
            _ => {}
        }
    }
    summary
}

fn focus_edit_stats(value: &serde_json::Value) -> (usize, u64, u64) {
    let previews = value
        .get("diff_previews")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .or_else(|| value.get("diff_preview").map(std::slice::from_ref))
        .unwrap_or_default();
    if !previews.is_empty() {
        let (added, removed) = previews.iter().fold((0_u64, 0_u64), |counts, preview| {
            (
                counts.0.saturating_add(
                    preview
                        .get("inserted_lines")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                ),
                counts.1.saturating_add(
                    preview
                        .get("removed_lines")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default(),
                ),
            )
        });
        return (previews.len(), added, removed);
    }
    let (mut added, mut removed) = (0_u64, 0_u64);
    if let Some(diff) = value.get("diff").and_then(serde_json::Value::as_str) {
        for line in diff.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                added += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                removed += 1;
            }
        }
    }
    (1, added, removed)
}

fn render_focus_tool_summary(
    summary: &FocusToolSummary,
    width: usize,
    styles: crate::terminal::Styles,
) -> Option<Line<'static>> {
    let mut clauses = Vec::new();
    if summary.edits > 0 {
        clauses.push(format!(
            "Edited {} file{} +{} -{}",
            summary.edits,
            if summary.edits == 1 { "" } else { "s" },
            summary.added,
            summary.removed
        ));
    }
    if summary.reads > 0 {
        clauses.push(format!(
            "read {} file{}",
            summary.reads,
            if summary.reads == 1 { "" } else { "s" }
        ));
    }
    if summary.other > 0 {
        clauses.push(format!(
            "ran {} other tool{}",
            summary.other,
            if summary.other == 1 { "" } else { "s" }
        ));
    }
    if summary.failures > 0 {
        clauses.push(format!("{} failed", summary.failures));
    }
    if !summary.running.is_empty() {
        clauses.push(format!("running {}", summary.running.join(", ")));
    }
    if clauses.is_empty() {
        return None;
    }
    let text = crate::terminal::truncate_to_width(&clauses.join(", "), width);
    Some(Line::from(Span::styled(
        text,
        Style::default().fg(if summary.failures > 0 {
            styles.error()
        } else {
            styles.text()
        }),
    )))
}

fn wrap_user_message(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for source in text.lines() {
        if source.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut row = String::new();
        let mut used = 0_usize;
        for character in source.chars() {
            let character_width = character.width().unwrap_or(0);
            if used > 0 && used.saturating_add(character_width) > width {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(character);
            used = used.saturating_add(character_width);
        }
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Lay one transcript item out as styled rows.
///
/// Every untrusted text path in the transcript — tool output, hook
/// contributions, citations, MCP results and the user's own line — funnels
/// through here, so the control-byte normalization lives at this one sink
/// rather than in each arm, where a new arm would silently miss it.
pub(crate) fn conversation_export_text(state: &AppState) -> Result<String, String> {
    const LIMIT: usize = 16 * 1024 * 1024;
    let mut output = format!(
        "heycode {}\n{} · {}\n{}\n\n",
        env!("CARGO_PKG_VERSION"),
        state.active_model_label(),
        human_runtime_label(&state.runtime),
        compact_cwd(&state.cwd)
    );
    output = markdown::terminal_safe_span(&output).into_owned();
    let active = state.items.iter().rposition(|item| matches!(item, Item::Command(text) if text.split_whitespace().next() == Some("/export")));
    let width = if state.transcript_area.width == 0 {
        80
    } else {
        usize::from(state.transcript_area.width)
    };
    for (index, item) in state.items.iter().enumerate() {
        if Some(index) == active {
            continue;
        }
        for line in render_transcript_item(
            item,
            item_neighbors(&state.items, index),
            width,
            state.show_reasoning,
            state.styles(),
        ) {
            for span in line.spans {
                if output
                    .len()
                    .saturating_add(span.content.len())
                    .saturating_add(1)
                    > LIMIT
                {
                    return Err("Conversation exceeds the 16 MiB export limit".to_owned());
                }
                output.push_str(&span.content);
            }
            output.push('\n');
        }
    }
    Ok(output.trim_end_matches('\n').to_owned())
}

/// Where one transcript item sits relative to an ephemeral command echo.
///
/// Claude Code 2.1.269 draws an accepted slash command and the line it
/// produces as one block: `❯ /reload-skills` immediately followed by
/// `  ⎿  Reloaded skills: …`, with no blank row between them. Both halves of
/// that pairing need to know about each other, so the neighbour facts are
/// resolved once, centrally, instead of in each command's own push site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct ItemNeighbors {
    /// The nearest preceding item that draws rows is a command echo, so this
    /// item is that command's receipt.
    pub(crate) after_command: bool,
    /// The nearest following item that draws rows is this command's receipt,
    /// so the echo must not close with a blank row.
    pub(crate) before_receipt: bool,
}

/// A settled line a command produced, rendered as the echo's receipt.
///
/// Claude gives command failures the same `⎿` receipt as command successes —
/// `❯ /cd missing` closes with `  ⎿  Couldn't find a directory at …` in the
/// ordinary text colour, not an error-coloured row — so both kinds pair with
/// the echo above them.
fn is_command_receipt(item: &Item) -> bool {
    matches!(item, Item::Info(_) | Item::Error(_))
}

/// Resolve [`ItemNeighbors`] for `items[index]`, skipping items that draw no
/// rows at all so a hidden lifecycle diagnostic cannot split an echo from its
/// receipt.
pub(crate) fn item_neighbors(items: &[Item], index: usize) -> ItemNeighbors {
    if items.get(index).is_some_and(Item::renders_no_rows) {
        return ItemNeighbors::default();
    }
    let drawn = |position: &usize| !items[*position].renders_no_rows();
    let previous = (0..index)
        .rev()
        .find(drawn)
        .map(|position| &items[position]);
    let next = (index.saturating_add(1)..items.len())
        .find(drawn)
        .map(|position| &items[position]);
    ItemNeighbors {
        after_command: matches!(previous, Some(Item::Command(_))),
        before_receipt: matches!(items.get(index), Some(Item::Command(_)))
            && next.is_some_and(is_command_receipt),
    }
}

pub(crate) fn render_transcript_item(
    item: &Item,
    neighbors: ItemNeighbors,
    width: usize,
    show_reasoning: bool,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let mut lines = render_transcript_item_raw(item, neighbors, width, show_reasoning, styles);
    for line in &mut lines {
        for span in &mut line.spans {
            let safe = match markdown::terminal_safe_span(span.content.as_ref()) {
                std::borrow::Cow::Borrowed(_) => None,
                std::borrow::Cow::Owned(value) => Some(value),
            };
            if let Some(safe) = safe {
                span.content = std::borrow::Cow::Owned(safe);
            }
        }
    }
    lines
}

fn render_transcript_item_raw(
    item: &Item,
    neighbors: ItemNeighbors,
    width: usize,
    show_reasoning: bool,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    if item.renders_no_rows() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    match item {
        Item::Attachments {
            attachments,
            document_routes,
        } => {
            for attachment in attachments {
                let dimensions = attachment.dimensions().map_or_else(String::new, |value| {
                    format!(" · {}×{}", value.width(), value.height())
                });
                let audio = attachment
                    .audio()
                    .map_or_else(String::new, audio_metadata_label);
                let route = document_routes
                    .iter()
                    .find(|route| route.selected() == attachment)
                    .map(|route| match route.kind() {
                        heycode_core::DocumentInputRouteKind::Native => " · native document",
                        heycode_core::DocumentInputRouteKind::Extracted => {
                            " · locally extracted document"
                        }
                    })
                    .unwrap_or("");
                lines.push(Line::from(vec![
                    Span::styled("▧ ", Style::default().fg(styles.accent())),
                    Span::styled(
                        attachment
                            .display_name()
                            .unwrap_or(if attachment.media_type().is_audio() {
                                "audio"
                            } else {
                                "image"
                            })
                            .to_owned(),
                        Style::default().fg(styles.text()),
                    ),
                    Span::styled(
                        format!(
                            " · {}{dimensions}{audio}{route}",
                            attachment.media_type().as_str()
                        ),
                        Style::default().fg(styles.dim()),
                    ),
                ]));
            }
        }
        Item::AudioOutput { attachments } => {
            for attachment in attachments {
                let metadata = attachment
                    .audio()
                    .map_or_else(String::new, audio_metadata_label);
                lines.push(Line::from(vec![
                    Span::styled("♪ assistant audio ", Style::default().fg(styles.accent())),
                    Span::styled(
                        attachment.display_name().unwrap_or("audio").to_owned(),
                        Style::default().fg(styles.text()),
                    ),
                    Span::styled(
                        format!(" · {}{metadata}", attachment.media_type().as_str()),
                        Style::default().fg(styles.dim()),
                    ),
                ]));
            }
        }
        Item::User(text) => {
            let rows = wrap_user_message(text, width.saturating_sub(2).max(1));
            for (index, raw) in rows.iter().enumerate() {
                let used = raw.width().saturating_add(2);
                lines.push(
                    Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                "❯ ".to_owned()
                            } else {
                                "  ".to_owned()
                            },
                            Style::default().fg(styles.prompt_glyph()),
                        ),
                        Span::styled(raw.clone(), Style::default().fg(styles.text())),
                        Span::raw(" ".repeat(width.saturating_sub(used))),
                    ])
                    .style(Style::default().bg(styles.prompt_background())),
                );
            }
            lines.push(blank());
        }
        Item::Command(text) => {
            // The source band is the width of its own text, not of the
            // terminal: `❯ /skills` occupies ten cells at 126 columns in the
            // pinned 2.1.269 captures. Rows there that do reach the right edge
            // are frame padding rather than a wider band — the same
            // `/reload-skills` row shrinks from 126 to exactly 60 cells when
            // that capture is resized, which content-sized bands would not do.
            let rows = wrap_user_message(text, width.saturating_sub(3).max(1));
            for (index, raw) in rows.iter().enumerate() {
                let used = raw.width().saturating_add(2);
                lines.push(
                    Line::from(vec![
                        Span::styled(
                            if index == 0 { "❯ " } else { "  " },
                            Style::default().fg(styles.prompt_glyph()),
                        ),
                        Span::styled(raw.clone(), Style::default().fg(styles.text())),
                        Span::raw(" ".repeat(width.saturating_sub(used).min(1))),
                    ])
                    .style(Style::default().bg(styles.prompt_background())),
                );
            }
            // A command and the line it produced are one block.
            if !neighbors.before_receipt {
                lines.push(blank());
            }
        }
        Item::Assistant(text) => {
            let mut rendered =
                markdown::render_markdown_with_styles(text, width.saturating_sub(2).max(1), styles);
            let mut first = true;
            for line in &mut rendered {
                if line.spans.is_empty() {
                    continue;
                }
                line.spans.insert(
                    0,
                    Span::styled(
                        if first { "● " } else { "  " },
                        Style::default().fg(if first { styles.accent() } else { styles.dim() }),
                    ),
                );
                first = false;
            }
            lines.extend(rendered);
            lines.push(blank());
        }
        Item::Reasoning { text, done, view } => {
            lines.extend(reasoning_view(
                view.expanded.unwrap_or(show_reasoning) || view.group_details,
                styles.dim(),
                text,
                *done,
                width,
                view,
            ));
            if view.expanded.unwrap_or(show_reasoning) || view.group_details {
                lines.push(blank());
            }
        }
        Item::Tool {
            name,
            args,
            result,
            untrusted_content,
            view,
            ..
        } => {
            lines.extend(tool_view(
                name,
                args,
                result.as_ref(),
                *untrusted_content,
                width,
                view,
                styles,
            ));
            lines.push(blank());
        }
        Item::ProviderState {
            provider,
            model,
            protocol,
            kind,
            output_index,
        } => lines.push(Line::from(vec![
            Span::styled("◇ provider state ", Style::default().fg(styles.dim())),
            Span::styled(
                format!("{provider}/{model}"),
                Style::default().fg(styles.text()).bold(),
            ),
            Span::styled(
                format!(" · {protocol} · {kind} · output {output_index}"),
                Style::default().fg(styles.dim()),
            ),
        ])),
        Item::ServerTool {
            logical,
            provider_name,
            result,
            ..
        } => {
            let (state, color) = result
                .as_ref()
                .map_or(("running", styles.warn()), |result| {
                    match result.outcome() {
                        heycode_core::ServerToolOutcome::Success => ("succeeded", styles.success()),
                        heycode_core::ServerToolOutcome::Error => ("failed", styles.error()),
                    }
                });
            lines.push(Line::from(vec![
                Span::styled("⏺ ", Style::default().fg(color)),
                Span::styled(
                    format!("provider tool {state}: {logical}"),
                    Style::default().fg(styles.text()).bold(),
                ),
                Span::styled(
                    format!(" ({provider_name})"),
                    Style::default().fg(styles.dim()),
                ),
            ]));
            if let Some(result) = result {
                if let Some(count) = result.output_count() {
                    lines.push(Line::from(Span::styled(
                        format!("  ⎿ {count} outputs"),
                        Style::default().fg(styles.dim()),
                    )));
                }
                if let Some(code) = result.error_code() {
                    lines.push(Line::from(Span::styled(
                        format!("  ⎿ provider error {code}"),
                        Style::default().fg(styles.error()),
                    )));
                }
                for source in result.sources() {
                    lines.push(Line::from(vec![
                        Span::styled("  ⎿ source ", Style::default().fg(styles.dim())),
                        Span::styled(
                            source.title().unwrap_or(source.url()).to_owned(),
                            Style::default().fg(styles.text()),
                        ),
                        Span::styled(
                            format!(" — {}", source.url()),
                            Style::default().fg(styles.dim()),
                        ),
                    ]));
                }
            }
            lines.push(blank());
        }
        Item::ServerToolUsage {
            logical,
            requests,
            cost,
        } => lines.push(Line::from(Span::styled(
            format!("provider tool usage: {logical} · {requests} requests · {cost}"),
            Style::default().fg(styles.dim()),
        ))),
        Item::Citation {
            url,
            title,
            cited_text,
            start_index,
            end_index,
        } => {
            lines.push(Line::from(vec![
                Span::styled("↗ citation: ", Style::default().fg(styles.accent())),
                Span::styled(
                    title.as_deref().unwrap_or(url).to_owned(),
                    Style::default().fg(styles.text()),
                ),
                Span::styled(format!(" — {url}"), Style::default().fg(styles.dim())),
            ]));
            if let Some(text) = cited_text {
                let range = match (start_index, end_index) {
                    (Some(start), Some(end)) => format!(" [{start}..{end}]"),
                    _ => String::new(),
                };
                lines.push(Line::from(Span::styled(
                    format!("  ⎿ {text}{range}"),
                    Style::default().fg(styles.dim()),
                )));
            }
        }
        Item::FindingsReport {
            report,
            expanded,
            focused,
        } => {
            for (text, kind) in findings_report_rows(report, *expanded, *focused, width) {
                lines.push(finding_row_line(text, kind, styles));
            }
            lines.push(blank());
        }
        Item::Compaction {
            native,
            strategy,
            replaced_upto_seq,
            summary,
            provider_items,
            expanded,
            focused,
        } => {
            lines.push(Line::from(Span::styled(
                format!(
                    "{} Compacted{}",
                    if *expanded { "▾" } else { "▸" },
                    if *expanded {
                        ""
                    } else {
                        " (ctrl+o to see full summary)"
                    }
                ),
                Style::default()
                    .fg(if *focused {
                        styles.accent()
                    } else {
                        styles.dim()
                    })
                    .bold(),
            )));
            if *expanded {
                let label = if *native {
                    format!(
                        "Native checkpoint: {} through event {replaced_upto_seq} · {provider_items} provider items",
                        strategy.as_deref().unwrap_or("provider-native")
                    )
                } else {
                    format!("Conversation summary through event {replaced_upto_seq}")
                };
                lines.extend(wrap_plain_dim(&label, width, styles));
                if let Some(summary) = summary {
                    lines.extend(wrap_plain_dim(summary, width, styles));
                }
                if *focused {
                    lines.push(Line::from(Span::styled(
                        "Enter/ctrl+o: collapse · Esc: compose",
                        Style::default().fg(styles.dim()),
                    )));
                }
            }
            lines.push(blank());
        }
        // Session wiring is diagnostics, not conversation. The accessible
        // projection keeps it under an explicitly labelled Diagnostics
        // section; the normal transcript stays focused on user-visible work.
        Item::RuntimeLink { .. } | Item::RouteChange { .. } => {}
        Item::PlanMode { active } => lines.push(Line::from(Span::styled(
            if *active {
                "plan mode enabled"
            } else {
                "plan mode disabled"
            },
            Style::default().fg(styles.warn()),
        ))),
        Item::Goal {
            action,
            phase,
            objective,
            revision,
        } => {
            lines.push(Line::from(Span::styled(
                format!(
                    "goal {action} · revision {revision}{}",
                    phase
                        .as_deref()
                        .map_or_else(String::new, |phase| format!(" · {phase}"))
                ),
                Style::default().fg(styles.warn()).bold(),
            )));
            if let Some(objective) = objective {
                lines.extend(wrap_plain_dim(objective, width, styles).into_iter().take(4));
            }
        }
        Item::Workflow { action, summary } => lines.push(Line::from(Span::styled(
            format!("workflow {action} · {summary}"),
            Style::default().fg(styles.warn()),
        ))),
        Item::Schedule { action, summary } => lines.push(Line::from(Span::styled(
            format!("schedule {action} · {summary}"),
            Style::default().fg(styles.warn()),
        ))),
        Item::Info(text) => {
            if neighbors.after_command {
                lines.extend(command_receipt_lines(text, width, styles));
            } else {
                lines.extend(wrap_plain_dim(text, width, styles));
            }
            lines.push(blank());
        }
        Item::Notice(text) => {
            let safe = markdown::terminal_safe_span(text);
            for (index, row) in wrap_user_message(&safe, width.saturating_sub(2).max(1))
                .into_iter()
                .enumerate()
            {
                lines.push(Line::from(vec![
                    Span::styled(
                        if index == 0 { "⏺ " } else { "  " },
                        Style::default().fg(styles.warn()),
                    ),
                    Span::styled(row, Style::default().fg(styles.warn())),
                ]));
            }
            lines.push(blank());
        }
        Item::Error(text) => {
            let safe = markdown::terminal_safe_span(text);
            if neighbors.after_command {
                lines.extend(command_receipt_lines(&safe, width, styles));
            } else {
                for (index, row) in wrap_user_message(&safe, width.saturating_sub(2).max(1))
                    .into_iter()
                    .enumerate()
                {
                    lines.push(Line::from(Span::styled(
                        format!("{}{}", if index == 0 { "✗ " } else { "  " }, row),
                        Style::default().fg(styles.error()),
                    )));
                }
            }
            lines.push(blank());
        }
    }
    lines
}

const FINDING_PREVIEW_LIMIT: usize = 32;
const FINDING_DETAIL_LIMIT: usize = 32;
const FINDING_TITLE_CHARS: usize = 160;
const FINDING_BODY_CHARS: usize = 512;

#[derive(Clone, Copy)]
enum FindingRowKind {
    Header,
    Path,
    Title,
    Body,
    Dim,
}

fn finding_row_line(
    text: String,
    kind: FindingRowKind,
    styles: crate::terminal::Styles,
) -> Line<'static> {
    let body = Style::default().fg(styles.text());
    let accent = Style::default().fg(styles.accent());
    let dim = Style::default().fg(styles.dim());
    match kind {
        FindingRowKind::Header => {
            if let Some((marker, rest)) = text.split_once("Code review") {
                return Line::from(vec![
                    Span::styled(marker.to_owned(), Style::default().fg(styles.success())),
                    Span::styled("Code review", body.bold()),
                    Span::styled(rest.to_owned(), body),
                ]);
            }
            Line::from(Span::styled(text, body))
        }
        FindingRowKind::Path => {
            if let Some(path) = text.strip_prefix("  ⎿  ") {
                Line::from(vec![
                    Span::styled("  ⎿  ", dim),
                    Span::styled(path.to_owned(), accent),
                ])
            } else {
                Line::from(Span::styled(text, accent))
            }
        }
        FindingRowKind::Title => {
            if let Some((marker, rest)) = text.split_once("● ")
                && let Some((location, rest)) = rest.split_once(' ')
            {
                let mut spans = vec![
                    Span::styled(format!("{marker}● "), body),
                    Span::styled(format!("{location} "), accent),
                ];
                if rest.starts_with('[')
                    && let Some((category, title)) = rest.split_once("] ")
                {
                    spans.push(Span::styled(format!("{category}] "), dim));
                    spans.push(Span::styled(title.to_owned(), body));
                } else {
                    spans.push(Span::styled(rest.to_owned(), body));
                }
                return Line::from(spans);
            }
            Line::from(Span::styled(text, body))
        }
        FindingRowKind::Body => Line::from(Span::styled(text, body)),
        FindingRowKind::Dim => Line::from(Span::styled(text, dim)),
    }
}

fn finding_severity_label(severity: heycode_session::ReviewSeverity) -> &'static str {
    match severity {
        heycode_session::ReviewSeverity::Critical => "critical",
        heycode_session::ReviewSeverity::High => "high",
        heycode_session::ReviewSeverity::Medium => "medium",
        heycode_session::ReviewSeverity::Low => "low",
    }
}

fn finding_severity_rank(severity: heycode_session::ReviewSeverity) -> u8 {
    match severity {
        heycode_session::ReviewSeverity::Critical => 4,
        heycode_session::ReviewSeverity::High => 3,
        heycode_session::ReviewSeverity::Medium => 2,
        heycode_session::ReviewSeverity::Low => 1,
    }
}

fn finding_report_highest(
    report: &heycode_session::FindingReport,
) -> heycode_session::ReviewSeverity {
    report
        .findings()
        .iter()
        .map(heycode_session::ReportedFinding::severity)
        .max_by_key(|severity| finding_severity_rank(*severity))
        .unwrap_or(heycode_session::ReviewSeverity::Low)
}

fn bounded_finding_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn push_finding_rows(
    rows: &mut Vec<(String, FindingRowKind)>,
    text: String,
    kind: FindingRowKind,
    width: usize,
) {
    let safe = markdown::terminal_safe_span(&text);
    rows.extend(
        wrap_user_message(&safe, width.max(1))
            .into_iter()
            .map(|row| (row, kind)),
    );
}

fn findings_report_rows(
    report: &heycode_session::FindingReport,
    expanded: bool,
    focused: bool,
    width: usize,
) -> Vec<(String, FindingRowKind)> {
    let mut rows = Vec::new();
    let marker = if expanded {
        '▾'
    } else if focused {
        '▸'
    } else {
        '⏺'
    };
    let focus_hint = if focused {
        " · enter toggle · esc compose"
    } else {
        ""
    };
    let level = report
        .level()
        .map(|level| match level {
            heycode_session::ReviewLevel::Low => "low",
            heycode_session::ReviewLevel::Medium => "medium",
            heycode_session::ReviewLevel::High => "high",
            heycode_session::ReviewLevel::Xhigh => "xhigh",
            heycode_session::ReviewLevel::Max => "max",
        })
        .map(|level| format!("{level} · "))
        .unwrap_or_default();
    push_finding_rows(
        &mut rows,
        format!(
            "{marker} Code review({level}{} finding{}){focus_hint}",
            report.findings().len(),
            if report.findings().len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        FindingRowKind::Header,
        width,
    );
    if expanded {
        push_finding_rows(
            &mut rows,
            format!(
                "  Local report; not externally published. Highest severity: {} · workspace r{}.",
                finding_severity_label(finding_report_highest(report)),
                report.source().workspace_revision()
            ),
            FindingRowKind::Dim,
            width,
        );
    }
    let visible = if expanded {
        FINDING_DETAIL_LIMIT
    } else {
        FINDING_PREVIEW_LIMIT
    };
    let mut groups =
        std::collections::BTreeMap::<&str, Vec<&heycode_session::ReportedFinding>>::new();
    for finding in report.findings().iter().take(visible) {
        groups.entry(finding.path()).or_default().push(finding);
    }
    for (path, findings) in groups {
        push_finding_rows(
            &mut rows,
            format!("  ⎿  {path}"),
            FindingRowKind::Path,
            width,
        );
        for finding in findings {
            let line = if finding.line_start() == finding.line_end() {
                finding.line_start().to_string()
            } else {
                format!("{}-{}", finding.line_start(), finding.line_end())
            };
            let category = finding
                .category()
                .map(|category| format!("[{category}] "))
                .unwrap_or_default();
            push_finding_rows(
                &mut rows,
                format!(
                    "       ● {line} {category}{}",
                    bounded_finding_text(finding.title(), FINDING_TITLE_CHARS)
                ),
                FindingRowKind::Title,
                width,
            );
            if expanded {
                push_finding_rows(
                    &mut rows,
                    format!(
                        "         severity: {}",
                        finding_severity_label(finding.severity())
                    ),
                    FindingRowKind::Dim,
                    width,
                );
                for (label, value) in [
                    ("trigger", finding.trigger()),
                    ("failure", finding.failure()),
                    ("impact", finding.impact()),
                ] {
                    push_finding_rows(
                        &mut rows,
                        format!(
                            "         {label}: {}",
                            bounded_finding_text(value, FINDING_BODY_CHARS)
                        ),
                        FindingRowKind::Body,
                        width,
                    );
                }
                if let Some(verdict) = finding.verdict() {
                    let verdict = match verdict {
                        heycode_session::FindingVerificationVerdict::Confirmed => "CONFIRMED",
                        heycode_session::FindingVerificationVerdict::Plausible => "PLAUSIBLE",
                    };
                    push_finding_rows(
                        &mut rows,
                        format!("         verdict: {verdict}"),
                        FindingRowKind::Body,
                        width,
                    );
                }
                if let Some(outcome) = finding.outcome() {
                    let outcome = match outcome {
                        heycode_session::FindingOutcome::Fixed => "fixed",
                        heycode_session::FindingOutcome::Skipped => "skipped",
                        heycode_session::FindingOutcome::NoChangeNeeded => "no change needed",
                    };
                    push_finding_rows(
                        &mut rows,
                        format!("         outcome: {outcome}"),
                        FindingRowKind::Body,
                        width,
                    );
                }
                push_finding_rows(
                    &mut rows,
                    format!(
                        "         revision: {}",
                        finding.revision().chars().take(12).collect::<String>()
                    ),
                    FindingRowKind::Dim,
                    width,
                );
            }
        }
    }
    if report.findings().len() > visible {
        let remainder = report.findings().len() - visible;
        push_finding_rows(
            &mut rows,
            format!(
                "  {remainder} additional legacy finding{} retained in session history",
                if remainder == 1 { "" } else { "s" }
            ),
            FindingRowKind::Dim,
            width,
        );
    }
    rows
}

pub(crate) fn findings_report_height_bound(
    report: &heycode_session::FindingReport,
    expanded: bool,
    focused: bool,
    width: usize,
) -> usize {
    findings_report_rows(report, expanded, focused, width)
        .len()
        .saturating_add(1)
}

fn audio_metadata_label(value: heycode_core::AttachmentAudioMetadata) -> String {
    let channels = match value.channels() {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        count => format!("{count} channels"),
    };
    format!(
        " · {:.2} s · {} kHz · {channels} · {}-bit",
        std::time::Duration::from_millis(value.duration_ms()).as_secs_f64(),
        value.sample_rate_hz() / 1_000,
        value.bits_per_sample()
    )
}

fn blank() -> Line<'static> {
    Line::from(String::new())
}

/// Reasoning block: one dim summary when collapsed; full stream when expanded.
fn reasoning_view(
    expanded: bool,
    dim: Color,
    text: &str,
    done: bool,
    width: usize,
    view: &crate::app::ReasoningView,
) -> Vec<Line<'static>> {
    if done && !expanded && !view.focused && !view.interrupted && view.elapsed_seconds == Some(0) {
        return Vec::new();
    }
    let style = Style::default().fg(dim);
    let mut out = vec![Line::from(Span::styled(
        format!(
            "{} · {}",
            view.label(done, expanded),
            if view.focused {
                "enter toggle · esc compose"
            } else {
                "ctrl+r"
            }
        ),
        if view.focused { style.bold() } else { style },
    ))];
    if expanded {
        for row in wrap_user_message(text, width.saturating_sub(4).max(1)) {
            out.push(Line::from(Span::styled(format!("  │ {row}"), style)));
        }
    } else if !done {
        if text.is_empty() {
            out.push(Line::from(Span::styled(
                "  │ Provider has not supplied readable thinking text",
                style,
            )));
        } else {
            let rows = wrap_user_message(text, width.saturating_sub(4).max(1));
            for row in rows.iter().skip(rows.len().saturating_sub(2)) {
                out.push(Line::from(Span::styled(format!("  │ {row}"), style)));
            }
        }
    }
    out
}

// ─── per-tool views ────────────────────────────────────────────────────────

fn arg<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Prefix of the first row of any receipt — a tool card's result or a slash
/// command's settled outcome — matching the captured Claude reference
/// (`  ⎿  Read 4 lines`, `  ⎿  Reloaded skills: 14 skills available`).
pub(crate) const RECEIPT_PREFIX: &str = "  ⎿  ";
/// Prefix of every receipt row after the first, which the reference aligns
/// under the receipt text rather than repeating the elbow.
pub(crate) const RECEIPT_CONTINUATION: &str = "     ";
/// Cells both prefixes occupy.
pub(crate) const RECEIPT_INDENT: usize = 5;

/// Tail of a `bash` body kept on a settled card.
const BASH_TAIL_LINES: usize = 2;
/// Head of an `edit`/`write` diff kept on a settled card.
const DIFF_HEAD_LINES: usize = 10;
/// Tail of a `read` body kept on a settled card.
const READ_TAIL_LINES: usize = 4;
/// Tail of a `grep`/`glob` match list kept on a settled card.
const MATCH_TAIL_LINES: usize = 5;
/// Tail of any other tool body kept on a settled card.
const OTHER_TAIL_LINES: usize = 6;
/// Longest argument summary a card title keeps before eliding.
const SUMMARY_MAX_CHARS: usize = 100;

/// Upper bound on the rows `tool_view` produces for this card.
///
/// The transcript height index treats the bound as a correctness contract, not
/// a hint (`crate::transcript`), so the two share the per-view line budgets
/// above: growing a view means growing its budget, which moves both at once.
pub(crate) fn tool_view_height_bound(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    untrusted_content: bool,
    width: usize,
) -> usize {
    // A running card is the header plus the `⎿ …` placeholder; the untrusted
    // banner and the body only exist once the call settles.
    let Some((ok, value)) = result else {
        return 2;
    };
    if !ok {
        return 3 + usize::from(untrusted_content);
    }
    let body = match canonical_tool_name(name) {
        // Tail rows, the `… N more lines` row and the exit pill.
        "bash" => BASH_TAIL_LINES + 2,
        // Diff rows plus the outcome message.
        "edit" | "multi_edit" | "write" => DIFF_HEAD_LINES + 2,
        // Path/line-count row plus the tail.
        "read" => READ_TAIL_LINES + 1,
        // Match-count row, the tail, and any bounded-search notices, each of
        // which can wrap at a narrow width.
        "grep" | "glob" => {
            MATCH_TAIL_LINES
                + 1
                + as_text(value)
                    .lines()
                    .filter(|line| {
                        line.starts_with("(Partial search;")
                            || line.starts_with("(Output limited:")
                            || line.starts_with("(showing ")
                    })
                    .map(|notice| {
                        wrap_prose(
                            notice,
                            width.saturating_sub(RECEIPT_CONTINUATION.width()).max(1),
                        )
                        .len()
                    })
                    .sum::<usize>()
        }
        // One row per todo, and the list is the model's to size.
        "todo_write" => value.as_array().map_or(0, Vec::len),
        // Every required question in a batch is a meaningful receipt. Use
        // the same wrapping as the card so viewport indexing cannot clip it.
        "ask_user_question" => question_answer_lines(args, value)
            .unwrap_or_else(|| vec!["Answer recorded · expand for details".into()])
            .iter()
            .map(|line| wrap_prose(&safe_tool_line(line), width.saturating_sub(4).max(1)).len())
            .sum(),
        "enter_worktree" | "exit_worktree" => worktree_summary(canonical_tool_name(name), value)
            .map_or(4, |rows| {
                rows.iter()
                    .map(|row| wrap_prose(row, width.saturating_sub(2).max(1)).len())
                    .sum()
            }),
        _ => OTHER_TAIL_LINES,
    };
    // Header row, the optional untrusted banner, then the body.
    let generic = 1_usize
        .saturating_add(usize::from(untrusted_content))
        .saturating_add(body);
    generic
        .max(crate::tool_family_cards::compact_height_bound(name, untrusted_content).unwrap_or(0))
}

pub(crate) fn expanded_tool_height_bound(
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    width: usize,
) -> usize {
    use unicode_width::UnicodeWidthStr;
    let row_width = width.saturating_sub(2).max(1);
    let rows = |text: &str| {
        text.lines()
            .map(|line| safe_tool_line(line).width().max(1).div_ceil(row_width))
            .sum::<usize>()
    };
    // Permission reasons are bounded by the approval owner; include the longest
    // allowed reason and the source label without undercounting narrow layouts.
    rows(&format!("Arguments: {}", as_text(args)))
        .saturating_add(result.map_or(32_usize.div_ceil(row_width), |(_, value)| {
            let structured = value
                .get("diff")
                .and_then(serde_json::Value::as_str)
                .map_or(0, rows)
                + value
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map_or(0, rows);
            let file_rows = value
                .get("files")
                .and_then(serde_json::Value::as_array)
                .map_or(0, |files| {
                    files
                        .iter()
                        .map(|file| {
                            file.get("content")
                                .and_then(serde_json::Value::as_str)
                                .map_or(0, rows)
                                .saturating_add(8)
                        })
                        .sum::<usize>()
                });
            rows(&as_text(value)).max(
                structured
                    + value
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .map_or(0, rows)
                    + file_rows,
            )
        }))
        .saturating_add((2 * heycode_agent::MAX_DENY_REASON_CHARS + 128).div_ceil(row_width))
        .saturating_add(4)
}

/// Read only the typed question receipt; IDs and selection schema remain raw metadata.
pub(crate) fn question_answer_lines(
    args: &serde_json::Value,
    value: &serde_json::Value,
) -> Option<Vec<String>> {
    fn answer(value: &serde_json::Value) -> Option<String> {
        if let Some(text) = value.as_str() {
            return Some(text.to_owned());
        }
        let labels = value
            .as_array()?
            .iter()
            .map(serde_json::Value::as_str)
            .collect::<Option<Vec<_>>>()?;
        Some(labels.join(", "))
    }
    if let Some(answers) = value.get("answers").and_then(serde_json::Value::as_array) {
        let mut lines = Vec::new();
        for entry in answers {
            lines.push(format!("Question: {}", entry.get("question")?.as_str()?));
            lines.push(format!("You answered: {}", answer(entry.get("answer")?)?));
        }
        return (!lines.is_empty()).then_some(lines);
    }
    let mut lines = Vec::new();
    if let Some(question) = args.get("question").and_then(serde_json::Value::as_str) {
        lines.push(format!("Question: {question}"));
    }
    lines.push(format!("You answered: {}", answer(value.get("answer")?)?));
    Some(lines)
}

pub(crate) fn agent_message_receipt(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    view: &crate::app::ToolViewState,
) -> Option<(String, String)> {
    if name != "agent_message" || view.expanded {
        return None;
    }
    let label = args.pointer("/source/agent_name")?.as_str()?;
    let id = args.pointer("/source/agent_id")?.as_str()?;
    let (_, value) = result?;
    let text = value.as_str()?;
    let prefix = format!(
        "[Agent message from {} ({id})]\n",
        serde_json::to_string(label).ok()?
    );
    Some((
        safe_tool_line(label),
        text.strip_prefix(&prefix).unwrap_or(text).to_owned(),
    ))
}

pub(crate) fn agent_completion_detail(
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
) -> Option<String> {
    let source = args.get("source")?;
    let outcome = source.get("outcome")?.as_str()?;
    if !matches!(outcome, "failed" | "cancelled" | "interrupted") {
        return None;
    }
    let prefix = format!(
        "[agent {} ({}) {}; run {}]\n",
        source.get("agent_name")?.as_str()?,
        source.get("agent_id")?.as_str()?,
        outcome,
        source.get("run_id")?.as_str()?
    );
    let body = result?.1.as_str()?.strip_prefix(&prefix)?;
    body.lines()
        .find(|line| !line.trim().is_empty())
        .map(safe_tool_line)
}

/// An attributed terminal occurrence is a named event, not a job tool call.
pub(crate) fn agent_completion_receipt(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    view: &crate::app::ToolViewState,
) -> Option<(String, String)> {
    if view.expanded {
        return None;
    }
    let label = view.completed_agent_label.as_ref()?;
    let outcome = if name == "agent_completion" {
        args.pointer("/source/outcome")?.as_str()?
    } else if view.completed_job.is_some() && result.is_some_and(|(ok, _)| *ok) {
        "completed"
    } else {
        return None;
    };
    matches!(
        outcome,
        "completed" | "failed" | "cancelled" | "interrupted"
    )
    .then(|| (safe_tool_line(label), outcome.to_owned()))
}

fn tool_view(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    width: usize,
    view: &crate::app::ToolViewState,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    if let Some((label, message)) = agent_message_receipt(name, args, result, view) {
        let mut header = Line::styled(
            crate::terminal::truncate_to_width(&format!("○ {label}"), width),
            Style::default().fg(styles.text()),
        );
        if view.focused {
            header.style = header.style.bg(styles.prompt_background());
        }
        let mut lines = vec![header];
        let rows: Vec<_> = message.lines().collect();
        lines.extend(receipt_rows(
            rows.iter()
                .take(OTHER_TAIL_LINES)
                .map(|row| (*row).to_owned()),
            styles.text(),
            width,
        ));
        if rows.len() > OTHER_TAIL_LINES {
            lines.extend(receipt_rows(
                ["… expand for the full message".to_owned()],
                styles.dim(),
                width,
            ));
        }
        return lines;
    }
    if let Some((label, outcome)) = agent_completion_receipt(name, args, result, view) {
        let color = match outcome.as_str() {
            "completed" => styles.success(),
            "failed" | "interrupted" => styles.error(),
            _ => styles.warn(),
        };
        let detail = agent_completion_detail(args, result)
            .map(|detail| format!(" — {detail}"))
            .unwrap_or_default();
        let suffix = format!(" {outcome}");
        let label =
            crate::terminal::truncate_to_width(&label, width.saturating_sub(2 + suffix.width()));
        let mut line = Line::from(vec![
            Span::styled(
                crate::terminal::truncate_to_width("○ ", width),
                Style::default().fg(color),
            ),
            Span::styled(
                crate::terminal::truncate_to_width(
                    &format!("{label}{suffix}{detail}"),
                    width.saturating_sub(2),
                ),
                Style::default().fg(styles.text()),
            ),
        ]);
        if view.focused {
            line.style = line.style.bg(styles.prompt_background());
        }
        return vec![line];
    }
    if !view.spawn_tree().is_empty() && !view.expanded {
        if let [child] = view.spawn_tree() {
            let status_color = match child.status {
                crate::task_console::TaskStatus::Failed
                | crate::task_console::TaskStatus::Interrupted => styles.error(),
                crate::task_console::TaskStatus::Waiting => styles.warn(),
                _ => styles.dim(),
            };
            let label = crate::terminal::truncate_to_width(
                &safe_tool_line(&child.label),
                width.saturating_sub(9),
            );
            let mut header = Line::from(vec![
                Span::styled("⏺ ", Style::default().fg(styles.accent())),
                Span::styled("Agent", Style::default().fg(styles.text()).bold()),
                Span::styled(format!("({label})"), Style::default().fg(styles.text())),
            ]);
            if width < 9 {
                header = Line::styled(
                    crate::terminal::truncate_to_width("⏺ Agent", width),
                    Style::default().fg(styles.text()).bold(),
                );
            }
            if view.focused {
                header.style = header.style.bg(styles.prompt_background());
            }
            let receipt = format!("Backgrounded agent · {}", child.status.label());
            let body_width = width.saturating_sub(5);
            let mut receipt_line = Line::from(vec![Span::styled(
                crate::terminal::truncate_to_width("  ⎿  ", width),
                Style::default().fg(styles.dim()),
            )]);
            if receipt.width() <= body_width {
                receipt_line.spans.extend([
                    Span::styled("Backgrounded agent", Style::default().fg(styles.text())),
                    Span::styled(
                        format!(" · {}", child.status.label()),
                        Style::default().fg(status_color),
                    ),
                ]);
                let hint = " (click to open · header expands)";
                if receipt.width() + hint.width() <= body_width {
                    receipt_line
                        .spans
                        .push(Span::styled(hint, Style::default().fg(styles.dim())));
                }
            } else {
                receipt_line.spans.push(Span::styled(
                    crate::terminal::truncate_to_width(child.status.label(), body_width),
                    Style::default().fg(status_color),
                ));
            }
            return vec![header, receipt_line];
        }
        let mut lines = vec![Line::styled(
            format!(
                "● {} {}",
                view.spawn_tree().len(),
                if view.spawn_tree().len() == 1 {
                    "agent"
                } else {
                    "agents"
                }
            ),
            Style::default().fg(styles.text()).bold(),
        )];
        for (index, child) in view.spawn_tree().iter().enumerate() {
            let branch = if index + 1 == view.spawn_tree().len() {
                "└─"
            } else {
                "├─"
            };
            let style = match child.status {
                crate::task_console::TaskStatus::Failed
                | crate::task_console::TaskStatus::Interrupted => styles.error(),
                crate::task_console::TaskStatus::Waiting => styles.warn(),
                _ => styles.dim(),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {branch} "), Style::default().fg(styles.dim())),
                Span::styled(
                    crate::terminal::truncate_to_width(&child.label, width.saturating_sub(24)),
                    Style::default().fg(styles.text()),
                ),
                Span::styled(
                    format!(" · {}", child.status.label()),
                    Style::default().fg(style),
                ),
            ]));
            if let Some(tool) = &child.telemetry.current_tool {
                lines.push(Line::styled(
                    format!("     {}", safe_tool_line(tool)),
                    Style::default().fg(styles.dim()),
                ));
            }
        }
        lines.push(Line::styled(
            "  click a child to open · /agents lists conversations",
            Style::default().fg(styles.dim()),
        ));
        return lines;
    }
    if let Some(summary) = &view.group_summary {
        let mut lines = vec![Line::from(Span::styled(
            crate::terminal::truncate_to_width(
                // The source shows a bare, indented summary row for a grouped
                // batch; expansion state is carried by the cards below it.
                &format!("  {summary}"),
                width,
            ),
            Style::default().fg(if view.focused {
                styles.text()
            } else {
                styles.dim()
            }),
        ))];
        if view.expanded {
            lines.extend(tool_card_view(
                name,
                args,
                result,
                untrusted_content,
                width,
                view,
                styles,
            ));
        }
        return lines;
    }
    tool_card_view(name, args, result, untrusted_content, width, view, styles)
}

fn tool_card_view(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    width: usize,
    view: &crate::app::ToolViewState,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let expanded = view.expanded || view.group_details;
    if canonical_tool_name(name) == "SendUserFile" {
        return crate::file_delivery::draw_lines(
            result,
            expanded,
            view.focused,
            &view.status(result),
            width,
            styles,
        );
    }
    let grouped = view.group_summary.is_some() || view.group_parent.is_some();
    if !expanded
        && !grouped
        && let Some(mut lines) = crate::tool_family_cards::draw_lines(
            name,
            args,
            result,
            untrusted_content,
            width,
            styles,
        )
    {
        if view.focused
            && let Some(header) = lines.first_mut()
        {
            header.style = header.style.bg(styles.prompt_background());
        }
        return lines;
    }
    if canonical_tool_name(name) == "workflow" && !expanded {
        return compact_tool_view(name, args, result, None, width, styles);
    }
    let status = if canonical_tool_name(name) == "job" && !arg(args, "outcome").is_empty() {
        arg(args, "outcome").to_owned()
    } else {
        view.status(result)
    };
    let display_title = if canonical_tool_name(name) == "todo_write" {
        if let Some((true, value)) = result {
            let todos = value.as_array().map(Vec::as_slice).unwrap_or_default();
            let completed = todos
                .iter()
                .filter(|todo| {
                    todo.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                })
                .count();
            format!("Plan updated · {completed}/{} complete", todos.len())
        } else {
            "Update plan".to_owned()
        }
    } else {
        format!(
            "{}({})",
            orchestration_title(name, args)
                .map(|title| if expanded { name } else { title })
                .unwrap_or_else(|| {
                    if canonical_tool_name(name) == "job_output" {
                        "Task output"
                    } else {
                        tool_display_name(name)
                    }
                }),
            summarize_args(name, args)
        )
    };
    // Every ungrouped card carries the reference bullet; the disclosure
    // triangle is reserved for the group summary row that owns expansion.
    let header = Line::from(vec![
        Span::styled(
            if grouped { "  " } else { "⏺ " },
            Style::default().fg(styles.accent()),
        ),
        Span::styled(
            display_title
                .split_once('(')
                .map_or(display_title.clone(), |(name, _)| name.to_owned()),
            Style::default().fg(styles.text()).bold(),
        ),
        Span::styled(
            display_title
                .split_once('(')
                .map_or(String::new(), |(_, args)| format!("({args}")),
            Style::default().fg(styles.text()),
        ),
        Span::styled(
            if canonical_tool_name(name) == "todo_write" && result.is_some_and(|(ok, _)| *ok) {
                String::new()
            } else if result.is_some_and(|(ok, value)| {
                *ok && value.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true)
            }) {
                " · preview".into()
            } else if result.is_some_and(|(ok, value)| {
                *ok && value.get("changed").and_then(serde_json::Value::as_bool) == Some(false)
            }) {
                " · unchanged".into()
            } else if matches!(
                canonical_tool_name(name),
                "read"
                    | "read_many"
                    | "write"
                    | "edit"
                    | "multi_edit"
                    | "bash"
                    | "glob"
                    | "grep"
                    | "schedule_create"
                    | "schedule_delete"
                    | "schedule_list"
            ) && result.is_some_and(|(ok, _)| *ok)
            {
                String::new()
            } else {
                format!(" · {status}")
            },
            Style::default().fg(if result.is_some_and(|(ok, _)| !ok) {
                styles.error()
            } else {
                styles.dim()
            }),
        ),
        Span::styled(
            if view.focused && !grouped && width >= 100 {
                if expanded {
                    " · click to collapse"
                } else {
                    " · click to expand"
                }
            } else {
                ""
            },
            Style::default().fg(styles.dim()),
        ),
    ])
    .style(if view.focused {
        Style::default().bg(styles.prompt_background())
    } else {
        Style::default()
    });
    if !expanded {
        let mut lines = compact_tool_view(name, args, result, None, width, styles);
        lines[0] = header;
        if canonical_tool_name(name) == "todo_write"
            && let Some((true, value)) = result
        {
            lines.truncate(1);
            if let Some(active) = value.as_array().and_then(|todos| {
                todos.iter().find(|todo| {
                    todo.get("status").and_then(serde_json::Value::as_str) == Some("in_progress")
                })
            }) {
                lines.push(Line::from(Span::styled(
                    format!("  ● {}", arg(active, "content")),
                    Style::default().fg(styles.text()),
                )));
            }
            return lines;
        }
        if let Some(text) = view
            .retrieved_output
            .values()
            .next_back()
            .filter(|text| !text.is_empty())
        {
            lines.truncate(1);
            for row in text.lines().take(2) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", crate::task_console::safe(row)),
                    Style::default().fg(styles.dim()),
                )));
            }
        }
        if !matches!(
            canonical_tool_name(name),
            "edit"
                | "multi_edit"
                | "write"
                | "grep"
                | "glob"
                | "bash"
                | "enter_worktree"
                | "exit_worktree"
                | "ask_user_question"
        ) {
            lines.truncate(3);
        }
        return lines;
    }
    if canonical_tool_name(name) == "todo_write" {
        let mut lines = compact_tool_view(name, args, result, None, width, styles);
        lines[0] = header;
        return lines;
    }
    if matches!(canonical_tool_name(name), "edit" | "multi_edit")
        && let Some((true, value)) = result
        && let Some(preview) = structured_edit_preview(value, width, styles, true)
    {
        let mut lines = vec![header];
        lines.extend(preview);
        return lines;
    }
    let mut lines = vec![header];
    // Semantic orchestration titles omit operational arguments in normal view.
    // Explicit expansion recovers the exact arguments alongside the result.
    let mut body: Vec<String> = Vec::new();
    if canonical_tool_name(name) == "workflow" || orchestration_title(name, args).is_some() {
        body.push(format!("Arguments: {}", as_text(args)));
    }
    if let Some(approval) = view
        .approval
        .as_deref()
        .filter(|approval| *approval != "approved")
    {
        body.push(format!("Permission: {approval}"));
    }
    if let Some(boundary) = untrusted_content {
        body.push(format!(
            "Source: {:?} (external tool content)",
            boundary.source()
        ));
    }
    if let Some((_, value)) = result {
        if matches!(canonical_tool_name(name), "edit" | "multi_edit" | "write")
            && let Some(diff) = value.get("diff").and_then(serde_json::Value::as_str)
        {
            body.push(diff.to_owned());
            if let Some(message) = value.get("message").and_then(serde_json::Value::as_str) {
                body.push(message.to_owned());
            }
            if value
                .get("diff_truncated")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                body.push(
                    "Diff preview truncated; replacement counts cover the full operation.".into(),
                );
            }
        } else if canonical_tool_name(name) == "read" && value.get("content").is_some() {
            // Lead with the same receipt the collapsed card shows, then the
            // retained lines, so expanding adds detail instead of replacing it.
            let returned = value
                .get("lines_returned")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            body.push(format!(
                "Read {returned} line{}",
                if returned == 1 { "" } else { "s" }
            ));
            body.push(
                value
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
            let total = value
                .get("total_lines")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "unknown".into(), |total| total.to_string());
            let remaining = value
                .get("lines_remaining")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "unknown".into(), |remaining| remaining.to_string());
            body.push(format!(
                "{} lines shown · {total} total · {remaining} remaining · {} bytes",
                value
                    .get("lines_returned")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                value
                    .get("total_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default()
            ));
            if value.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
                body.push("More content is available through Read pagination.".into());
            }
        } else if canonical_tool_name(name) == "bash" {
            // The expanded shell card shows the whole output, but the stream
            // and exit markers stay out of it the way the reference does; a
            // non-zero exit is stated once, first.
            let text = as_text(value);
            if let Some((label, false)) = extract_exit(&text) {
                body.push(format!(
                    "Error: Exit code {}",
                    label.trim_matches(['[', ']'])
                ));
            }
            body.extend(
                text.lines()
                    .filter(|line| {
                        !line.starts_with("[exit code:")
                            && !line.starts_with("[killed by signal:")
                            && !line.starts_with("[timed out")
                            && line.trim() != "[stderr]"
                    })
                    .map(str::to_owned),
            );
        } else if canonical_tool_name(name) == "read_many" && value.get("files").is_some() {
            if let Some(files) = value.get("files").and_then(serde_json::Value::as_array) {
                for file in files {
                    body.push(format!("{} · {}", arg(file, "path"), arg(file, "status")));
                    if let Some(content) = file.get("content").and_then(serde_json::Value::as_str) {
                        body.push(content.to_owned());
                        let total = file
                            .get("total_lines")
                            .and_then(serde_json::Value::as_u64)
                            .map_or_else(|| "unknown".into(), |value| value.to_string());
                        let remaining = file
                            .get("lines_remaining")
                            .and_then(serde_json::Value::as_u64)
                            .map_or_else(|| "unknown".into(), |value| value.to_string());
                        body.push(format!("{total} total lines · {remaining} remaining"));
                    } else {
                        body.push(
                            file.get("error")
                                .or_else(|| file.get("reason"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        );
                    }
                }
            }
        } else if canonical_tool_name(name) == "job_output" {
            body.push(
                value
                    .pointer("/page/text")
                    .or_else(|| value.get("text"))
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| as_text(value), str::to_owned),
            );
        } else {
            body.push(as_text(value));
        }
    } else {
        body.push("Waiting for the result…".into());
    }
    for ((stream, offset), text) in &view.retrieved_output {
        body.push(format!("Additional {stream} output (from byte {offset}):"));
        body.push(text.clone());
    }
    let rows = body
        .iter()
        .flat_map(|section| section.lines())
        .flat_map(|raw| {
            wrap_user_message(
                &safe_tool_line(raw),
                width.saturating_sub(RECEIPT_PREFIX.width()).max(1),
            )
        })
        .collect::<Vec<_>>();
    lines.extend(receipt_rows(rows, styles.dim(), width));
    lines
}

/// Numbered file content under a Write card, matching the reference layout
/// (`      1 first note`) with the numbers right-aligned to the widest one.
fn numbered_content_rows(
    content: &str,
    limit: usize,
    width: usize,
    color: Color,
) -> Vec<Line<'static>> {
    let shown = content.lines().take(limit).count();
    let digits = shown.to_string().len();
    content
        .lines()
        .take(limit)
        .enumerate()
        .map(|(index, line)| {
            let label = format!("      {:>digits$} ", index + 1);
            Line::from(vec![
                Span::raw(label.clone()),
                Span::styled(
                    crate::terminal::truncate_to_width(
                        &safe_tool_line(line),
                        width.saturating_sub(label.width()).max(1),
                    ),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect()
}

/// Render result rows the way the reference does: the elbow on the first row,
/// every later row aligned under its text.
fn receipt_rows(
    rows: impl IntoIterator<Item = String>,
    color: Color,
    width: usize,
) -> Vec<Line<'static>> {
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let prefix = if index == 0 {
                RECEIPT_PREFIX
            } else {
                RECEIPT_CONTINUATION
            };
            Line::from(vec![
                Span::raw(prefix),
                Span::styled(
                    crate::terminal::truncate_to_width(
                        &safe_tool_line(&row),
                        width.saturating_sub(prefix.width()).max(1),
                    ),
                    Style::default().fg(color),
                ),
            ])
        })
        .collect()
}

fn compact_tool_view(
    name: &str,
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
    untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
    width: usize,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let display_name = canonical_tool_name(name);
    if display_name == "SendUserFile" {
        let view = crate::app::ToolViewState::default();
        return crate::file_delivery::draw_lines(
            result,
            false,
            false,
            &view.status(result),
            width,
            styles,
        );
    }
    if display_name == "workflow" {
        let title = crate::workflow_render::tool_title(args);
        let failed = result.is_some_and(|(ok, _)| !ok);
        let mut lines = vec![Line::from(vec![
            Span::styled("⏺ ", Style::default().fg(styles.accent())),
            Span::styled(
                "Workflow",
                Style::default()
                    .fg(if failed {
                        styles.error()
                    } else {
                        styles.text()
                    })
                    .bold(),
            ),
            Span::styled(format!("({title})"), Style::default().fg(styles.text())),
        ])];
        lines.extend(receipt_rows(
            [crate::workflow_render::tool_receipt(args, result)],
            if failed { styles.error() } else { styles.dim() },
            width,
        ));
        return lines;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled("⏺ ".to_owned(), Style::default().fg(styles.accent())),
        Span::styled(
            orchestration_title(name, args)
                .unwrap_or_else(|| tool_display_name(name))
                .to_owned(),
            Style::default().fg(styles.text()).bold(),
        ),
        Span::styled(
            format!("({})", summarize_args(name, args)),
            Style::default().fg(styles.dim()),
        ),
    ])];

    let Some((ok, value)) = result else {
        // A running shell keeps its command visible on the receipt row, the
        // way the reference does, instead of an anonymous ellipsis.
        let pending = if display_name == "bash" {
            format!(
                "{RECEIPT_PREFIX}$ {}",
                crate::terminal::truncate_to_width(
                    &safe_tool_line(arg(args, "command")),
                    width.saturating_sub(RECEIPT_PREFIX.width() + 2),
                )
            )
        } else {
            format!("{RECEIPT_PREFIX}…")
        };
        lines.push(Line::from(Span::styled(
            pending,
            Style::default().fg(styles.dim()),
        )));
        return lines;
    };

    if let Some(boundary) = untrusted_content {
        let source = match boundary.source() {
            heycode_core::UntrustedContentSource::Web => "WEB",
            heycode_core::UntrustedContentSource::Mcp => "MCP SERVER",
            heycode_core::UntrustedContentSource::Lsp => "LANGUAGE SERVER",
            heycode_core::UntrustedContentSource::ToolOrchestration => "TOOL ORCHESTRATION",
        };
        lines.push(Line::from(Span::styled(
            format!("  ⚠ UNTRUSTED {source} CONTENT · data, not instructions"),
            Style::default().fg(styles.warn()).bold(),
        )));
    }

    if !ok {
        let text = value
            .get("message")
            .or_else(|| value.get("error"))
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| as_text(value), str::to_owned);
        let mut rows: Vec<String> = if display_name == "bash" {
            let message = text
                .lines()
                .find(|line| {
                    !line.trim().is_empty()
                        && !line.starts_with("[exit code:")
                        && !line.starts_with("[killed by signal:")
                })
                .unwrap_or("Command failed")
                .to_owned();
            let mut rows = vec![message];
            if let Some((label, _)) = extract_exit(&text) {
                rows.insert(
                    0,
                    format!("Error: Exit code {}", label.trim_matches(['[', ']'])),
                );
            }
            rows
        } else {
            wrap_prose(&text, width.saturating_sub(RECEIPT_PREFIX.width()).max(1))
                .into_iter()
                .take(2)
                .collect()
        };
        rows.truncate(2);
        lines.extend(receipt_rows(rows, styles.error(), width));
        return lines;
    }

    if let Some(rows) = orchestration_receipt(name, args, value) {
        lines.extend(receipt_rows(
            rows,
            if crate::transcript::orchestration_needs_attention(value) {
                styles.warn()
            } else {
                styles.dim()
            },
            width,
        ));
        return lines;
    }

    match display_name {
        "schedule_create" => {
            let id = arg(value, "schedule_id");
            if id.is_empty() {
                lines.extend(receipt_rows([as_text(value)], styles.dim(), width));
            } else {
                let schedule = if !arg(value, "cron").is_empty() {
                    format!("{} · {}", arg(value, "cron"), arg(value, "timezone"))
                } else {
                    schedule_selector(args)
                };
                lines.extend(receipt_rows(
                    [format!(
                        "Scheduled {} ({})",
                        safe_tool_line(id),
                        safe_tool_line(&schedule)
                    )],
                    styles.text(),
                    width,
                ));
            }
        }
        "schedule_delete"
            if value.get("deleted").and_then(serde_json::Value::as_bool) == Some(true) =>
        {
            lines.extend(receipt_rows(
                [format!(
                    "Deleted {}",
                    safe_tool_line(arg(value, "schedule_id"))
                )],
                styles.text(),
                width,
            ));
        }
        "schedule_list" => {
            if let Some(schedules) = value.as_array() {
                lines.extend(receipt_rows(
                    [if schedules.is_empty() {
                        "No scheduled jobs".to_owned()
                    } else {
                        format!(
                            "{} schedule{}",
                            schedules.len(),
                            if schedules.len() == 1 { "" } else { "s" }
                        )
                    }],
                    styles.text(),
                    width,
                ));
                for schedule in schedules.iter().take(OTHER_TAIL_LINES.saturating_sub(1)) {
                    lines.push(Line::styled(
                        format!(
                            "{RECEIPT_CONTINUATION}{}: {}",
                            safe_tool_line(arg(schedule, "schedule_id")),
                            safe_tool_line(arg(schedule, "prompt"))
                        ),
                        Style::default().fg(styles.dim()),
                    ));
                }
            } else {
                lines.extend(receipt_rows(
                    tail(
                        &as_text(value)
                            .lines()
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                        OTHER_TAIL_LINES,
                    ),
                    styles.dim(),
                    width,
                ));
            }
        }
        "enter_plan_mode" => {
            let status = value.get("status").and_then(serde_json::Value::as_str);
            let message = match status {
                Some("committed" | "already_active") => {
                    "Plan mode active · research without changes"
                }
                Some("queued") => "Plan mode requested · waiting for activation",
                Some("cancelled") => "Plan mode request cancelled",
                _ => "Plan mode result · expand to inspect",
            };
            lines.push(Line::styled(
                format!("  {message}"),
                Style::default().fg(styles.dim()),
            ));
        }
        "exit_plan_mode" => {
            let message = match value.get("status").and_then(serde_json::Value::as_str) {
                Some("approved") => {
                    match value.get("permissions").and_then(serde_json::Value::as_str) {
                        Some("ask") => "Plan accepted · Default permissions",
                        Some("accepted_edits") => "Plan accepted · file edits allowed",
                        _ => "Plan accepted · expand to inspect permissions",
                    }
                }
                Some("stay_in_plan") => {
                    "Remain in Plan mode · revise the plan before making changes"
                }
                Some("mode_changed") => "Plan mode changed by user · follow current permissions",
                _ => "Plan review result · expand to inspect",
            };
            lines.push(Line::styled(
                format!("  {message}"),
                Style::default().fg(styles.dim()),
            ));
        }
        "enter_worktree" | "exit_worktree" if worktree_summary(display_name, value).is_some() => {
            for text in worktree_summary(display_name, value).unwrap_or_default() {
                for row in wrap_prose(&text, width.saturating_sub(2).max(1)) {
                    lines.push(Line::styled(
                        format!("  {row}"),
                        Style::default().fg(styles.text()),
                    ));
                }
            }
        }
        "job_output" => {
            let text = value
                .pointer("/page/text")
                .or_else(|| value.get("text"))
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| as_text(value), str::to_owned);
            if text.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  No new output",
                    Style::default().fg(styles.dim()),
                )));
            } else {
                for raw in text.lines().take(2) {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", crate::task_console::safe(raw)),
                        Style::default().fg(styles.dim()),
                    )));
                }
            }
        }
        "bash" => {
            let text = as_text(value);
            let exit = extract_exit(&text);
            let body = text
                .lines()
                .filter(|l| {
                    !l.starts_with("[exit code:")
                        && !l.starts_with("[killed by signal:")
                        && !l.starts_with("[timed out")
                        && l.trim() != "[stderr]"
                        && !l.trim().is_empty()
                })
                .map(str::to_owned)
                .collect::<Vec<_>>();
            // A non-zero exit is still a completed call, so the receipt leads
            // with the failure the way the reference does before the output.
            let failed = exit.as_ref().is_some_and(|(_, ok)| !ok);
            let mut rows = Vec::new();
            if let Some((label, false)) = &exit {
                rows.push(format!(
                    "Error: Exit code {}",
                    label.trim_matches(['[', ']'])
                ));
            }
            rows.extend(tail(&body, BASH_TAIL_LINES));
            if body.len() > BASH_TAIL_LINES {
                rows.push(format!("… {} more lines", body.len() - BASH_TAIL_LINES));
            }
            if rows.is_empty() {
                rows.push("(no output)".to_owned());
            }
            lines.extend(receipt_rows(
                rows,
                if failed { styles.error() } else { styles.dim() },
                width,
            ));
        }
        "edit" | "multi_edit" | "write" => {
            if let Some(preview) = structured_edit_preview(value, width, styles, false) {
                lines.extend(preview);
                return lines;
            }
            let msg = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.as_str())
                .unwrap_or("");
            let mut receipt = vec![Span::raw(RECEIPT_PREFIX)];
            receipt.extend(emphasized_file_receipt(msg, arg(args, "path"), styles));
            lines.push(Line::from(receipt));
            if let Some(diff) = value.get("diff").and_then(serde_json::Value::as_str) {
                for raw in diff.lines().take(DIFF_HEAD_LINES) {
                    let color = match raw.chars().next() {
                        Some('+') => styles.success(),
                        Some('-') => styles.error(),
                        _ => styles.dim(),
                    };
                    lines.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(raw.to_owned(), Style::default().fg(color)),
                    ]));
                }
                if diff.lines().count() > DIFF_HEAD_LINES
                    || value
                        .get("diff_truncated")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                {
                    lines.push(Line::styled(
                        "      … diff preview truncated",
                        Style::default().fg(styles.dim()),
                    ));
                }
            } else if display_name == "write"
                && value.get("changed").and_then(serde_json::Value::as_bool) != Some(false)
            {
                lines.extend(numbered_content_rows(
                    arg(args, "content"),
                    DIFF_HEAD_LINES,
                    width,
                    styles.text(),
                ));
            }
        }
        "read" => {
            let text = value
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| as_text(value), str::to_owned);
            let n = text.lines().count();
            lines.extend(receipt_rows(
                [format!("Read {n} line{}", if n == 1 { "" } else { "s" })],
                styles.dim(),
                width,
            ));
        }
        "read_many" => {
            let files = value.get("files").and_then(serde_json::Value::as_array);
            let counts = files.map_or([0, 0, 0], |files| {
                let mut counts = [0, 0, 0];
                for file in files {
                    match arg(file, "status") {
                        "read" => counts[0] += 1,
                        "error" => counts[1] += 1,
                        _ => counts[2] += 1,
                    }
                }
                counts
            });
            let mut receipt = format!(
                "Read {} file{}",
                counts[0],
                if counts[0] == 1 { "" } else { "s" }
            );
            if counts[1] > 0 || counts[2] > 0 {
                receipt.push_str(&format!(" · {} failed · {} deferred", counts[1], counts[2]));
            }
            lines.extend(receipt_rows(
                [receipt],
                if counts[1] > 0 {
                    styles.warn()
                } else {
                    styles.dim()
                },
                width,
            ));
        }
        "grep" | "glob" => {
            let text = as_text(value);
            let all: Vec<String> = text
                .lines()
                .filter(|line| {
                    !line.is_empty()
                        && !line.starts_with("(showing ")
                        && !line.starts_with("(Partial search;")
                        && !line.starts_with("(Output limited:")
                })
                .map(str::to_owned)
                .collect();
            // The reference counts files for a path pattern and lines for a
            // content search, and says so even when nothing matched.
            let unit = if display_name == "glob" {
                "file"
            } else {
                "line"
            };
            let bounded = text
                .lines()
                .any(|line| line.starts_with("(Partial search;"));
            let count = format!(
                "Found {}{} {unit}{}",
                if bounded { "at least " } else { "" },
                all.len(),
                if all.len() == 1 { "" } else { "s" }
            );
            let mut rows = vec![count];
            rows.extend(all.into_iter().take(MATCH_TAIL_LINES));
            lines.extend(receipt_rows(rows, styles.dim(), width));
            for notice in text.lines().filter(|line| {
                line.starts_with("(Partial search;")
                    || line.starts_with("(Output limited:")
                    || line.starts_with("(showing ")
            }) {
                for row in wrap_prose(
                    notice,
                    width.saturating_sub(RECEIPT_CONTINUATION.width()).max(1),
                ) {
                    lines.push(Line::styled(
                        format!("{RECEIPT_CONTINUATION}{row}"),
                        Style::default().fg(styles.warn()),
                    ));
                }
            }
        }
        "todo_write" => {
            if let Some(todos) = value.as_array() {
                for t in todos {
                    let content = t.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let (glyph, color) = match status {
                        "completed" => ("✔", styles.success()),
                        "in_progress" => ("◉", styles.accent()),
                        _ => ("○", styles.dim()),
                    };
                    lines.push(Line::from(vec![
                        Span::raw("  ⎿ "),
                        Span::styled(format!("{glyph} "), Style::default().fg(color)),
                        Span::styled(content.to_owned(), Style::default().fg(styles.text())),
                    ]));
                }
            }
        }
        "ask_user_question" => {
            let rendered = question_answer_lines(args, value)
                .unwrap_or_else(|| vec!["Answer recorded · expand for details".into()]);
            for text in rendered {
                for row in wrap_prose(&safe_tool_line(&text), width.saturating_sub(4).max(1)) {
                    lines.push(Line::from(vec![
                        Span::raw("  ➿ "),
                        Span::styled(row, Style::default().fg(styles.success())),
                    ]));
                }
            }
        }
        "web_search" | "web_fetch" => {
            let text = as_text(value);
            lines.extend(receipt_rows(
                tail(
                    &text.lines().map(str::to_owned).collect::<Vec<_>>(),
                    OTHER_TAIL_LINES,
                ),
                styles.dim(),
                width,
            ));
        }
        _ => {
            let text = as_text(value);
            lines.extend(receipt_rows(
                tail(
                    &text.lines().map(str::to_owned).collect::<Vec<_>>(),
                    OTHER_TAIL_LINES,
                ),
                if *ok { styles.dim() } else { styles.error() },
                width,
            ));
        }
    }
    lines
}

fn summarize_args(name: &str, args: &serde_json::Value) -> String {
    let pick = |k: &str| {
        args.get(k)
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let primary = match canonical_tool_name(name) {
        "bash" => pick("command"),
        "read" | "write" | "edit" | "multi_edit" => pick("path"),
        "read_many" => format!(
            "{} files",
            args.get("files")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len)
        ),
        "schedule_create" => format!("{}: {}", schedule_selector(args), pick("prompt")),
        "schedule_delete" | "schedule_update" => pick("schedule_id"),
        "job_output" => pick("job_id"),
        "agent_control" | "job_control" | "interrupt_task" => String::new(),
        "ask_user_question_async" => pick("question"),
        // `Search(pattern: "…", path: "…")`, as the reference labels both.
        "grep" | "glob" => {
            let pattern = pick("pattern");
            let path = pick("path");
            if path.is_empty() {
                format!("pattern: {pattern:?}")
            } else {
                format!("pattern: {pattern:?}, path: {path:?}")
            }
        }
        "web_search" => pick("query"),
        "web_fetch" => pick("url"),
        "task" | "agent" => pick("label"),
        "job" => pick("job_id"),
        "load_skill" => pick("name"),
        "ask_user_question" => pick("question"),
        "todo_write" => "(list)".to_owned(),
        _ => String::new(),
    };
    // The reference keeps the whole command or path on the title row at a
    // normal viewport, so the cap only guards pathological inputs.
    let mut shown = primary.replace("\\n", " ");
    if shown.chars().count() > SUMMARY_MAX_CHARS {
        shown = format!(
            "{}…",
            shown.chars().take(SUMMARY_MAX_CHARS).collect::<String>()
        );
    }
    shown
}

/// Keep each admitted native selector explicit; do not infer a cron schedule
/// or a next-run timestamp from an unrelated timing field.
fn emphasized_file_receipt(
    message: &str,
    path: &str,
    styles: crate::terminal::Styles,
) -> Vec<Span<'static>> {
    let style = Style::default().fg(styles.text());
    let mut spans = Vec::new();
    let mut remainder = message;
    while !remainder.is_empty() {
        if !path.is_empty() && remainder.starts_with(path) {
            spans.push(Span::styled(path.to_owned(), style.bold()));
            remainder = &remainder[path.len()..];
            continue;
        }
        let numeric = remainder.starts_with(|ch: char| ch.is_ascii_digit());
        let end = remainder
            .char_indices()
            .skip(1)
            .find_map(|(index, ch)| {
                (ch.is_ascii_digit() != numeric
                    || (!path.is_empty() && remainder[index..].starts_with(path)))
                .then_some(index)
            })
            .unwrap_or(remainder.len());
        spans.push(Span::styled(
            remainder[..end].to_owned(),
            if numeric { style.bold() } else { style },
        ));
        remainder = &remainder[end..];
    }
    spans
}

fn schedule_selector(args: &serde_json::Value) -> String {
    if !arg(args, "cron").is_empty() {
        return safe_tool_line(arg(args, "cron"));
    }
    for (key, label, unit) in [
        ("after_seconds", "after", "s"),
        ("every_seconds", "every", "s"),
        ("at_ms", "at", "ms epoch"),
        ("wakeup_seconds", "wakeup every", "s"),
    ] {
        if let Some(value) = args.get(key) {
            return format!("{label} {value}{unit}");
        }
    }
    "timing unspecified".to_owned()
}

fn worktree_summary(name: &str, value: &serde_json::Value) -> Option<Vec<String>> {
    let status = value.get("status").and_then(serde_json::Value::as_str);
    let label = match (name, status) {
        ("enter_worktree", Some("entered")) => "Entered worktree",
        ("exit_worktree", Some("exited_retained")) => "Returned to workspace",
        _ => return None,
    };
    let cwd = value.get("cwd")?.as_str()?;
    let mut rows = vec![format!("{label}: {}", safe_tool_line(cwd))];
    if name == "exit_worktree"
        && let Some(paths) = value
            .get("retained_worktrees")
            .and_then(serde_json::Value::as_array)
    {
        for path in paths.iter().filter_map(serde_json::Value::as_str).take(2) {
            rows.push(format!("Retained worktree: {}", safe_tool_line(path)));
        }
        if paths.len() > 2 {
            rows.push(format!(
                "{} more retained worktrees · expand to view",
                paths.len() - 2
            ));
        }
    }
    Some(rows)
}

fn structured_edit_preview(
    value: &serde_json::Value,
    width: usize,
    styles: crate::terminal::Styles,
    expanded: bool,
) -> Option<Vec<Line<'static>>> {
    let previews = if let Some(preview) = value
        .get("diff_preview")
        .filter(|preview| preview.is_object())
    {
        vec![preview]
    } else {
        value
            .get("diff_previews")?
            .as_array()?
            .iter()
            .filter(|preview| preview.is_object())
            .collect::<Vec<_>>()
    };
    if previews.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    if value.get("changed").and_then(serde_json::Value::as_bool) == Some(false) {
        lines.push(Line::styled(
            "  ⎿  Content unchanged",
            Style::default().fg(styles.text()),
        ));
        return Some(lines);
    }
    let dry_run = value.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true);
    for (index, preview) in previews.iter().enumerate() {
        if !expanded && lines.len() >= 24 {
            lines.push(Line::styled(
                "      … additional edit previews; expand to inspect",
                Style::default().fg(styles.dim()),
            ));
            break;
        }
        let added = preview
            .get("inserted_lines")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let removed = preview
            .get("removed_lines")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let text = Style::default().fg(styles.text());
        let mut receipt = vec![Span::raw(RECEIPT_PREFIX)];
        if previews.len() > 1 {
            receipt.push(Span::styled(format!("Edit {}: ", index + 1), text));
        }
        receipt.push(Span::styled(
            if dry_run { "Would add " } else { "Added " },
            text,
        ));
        receipt.push(Span::styled(added.to_string(), text.bold()));
        receipt.push(Span::styled(
            format!(
                " line{}, {} ",
                if added == 1 { "" } else { "s" },
                if dry_run { "remove" } else { "removed" }
            ),
            text,
        ));
        receipt.push(Span::styled(removed.to_string(), text.bold()));
        receipt.push(Span::styled(
            format!(" line{}", if removed == 1 { "" } else { "s" }),
            text,
        ));
        if preview
            .get("first_replacement_only")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            receipt.push(Span::styled(" (first replacement)", text));
        }
        lines.push(Line::from(receipt));
        let rows = preview.get("rows").and_then(serde_json::Value::as_array);
        for row in rows
            .into_iter()
            .flatten()
            .take(if expanded { 24 } else { DIFF_HEAD_LINES })
        {
            let kind = arg(row, "kind");
            let number = row
                .get("line")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            let marker = match kind {
                "removed" => '-',
                "added" => '+',
                _ => ' ',
            };
            let label = format!("{number:>2} {marker}");
            let text = safe_tool_line(arg(row, "text"));
            let body_width = width.saturating_sub(12);
            let text =
                crate::terminal::truncate_to_width(&text, body_width.saturating_sub(label.width()));
            let mut number_style = Style::default().fg(styles.dim());
            let mut text_style = Style::default().fg(styles.text());
            let mut padding = String::new();
            if kind == "removed" || kind == "added" {
                let added = kind == "added";
                let background = styles.diff_background(added);
                number_style = number_style.fg(styles.diff_marker(added)).bg(background);
                text_style = text_style.bg(background);
                padding = " ".repeat(body_width.saturating_sub(label.width() + text.width()));
            }
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(label, number_style),
                Span::styled(text, text_style),
                Span::styled(padding, text_style),
            ]));
        }
        if rows.is_some_and(|rows| rows.len() > if expanded { 24 } else { DIFF_HEAD_LINES })
            || preview
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        {
            lines.push(Line::styled(
                "      … diff preview truncated",
                Style::default().fg(styles.dim()),
            ));
        }
    }
    Some(lines)
}

/// Action labels are human presentation; retained tool names remain exact.
fn orchestration_title<'a>(name: &'a str, args: &serde_json::Value) -> Option<&'a str> {
    let action = arg(args, "action");
    Some(match canonical_tool_name(name) {
        "agent_completion" => "Agent result",
        "agent_message" => "Agent message",
        "send_message" => "Message agent",
        "list_agents" => "Inspect agents",
        "list_jobs" => "Inspect jobs",
        "ask_user_question_async" => "Optional question",
        "agent_control" | "interrupt_task" => match action {
            "list" => "Inspect agents",
            "wait" => "Waiting for agents",
            "send" => "Message agent",
            "interrupt" => "Interrupt agent",
            "archive" | "close" => "Archive agent",
            "restore" => "Restore agent",
            _ => "Agent control",
        },
        "job_control" => match action {
            "list" => "Inspect jobs",
            "output" => "Job output",
            "cancel" => "Cancel job",
            _ => "Job control",
        },
        _ => return None,
    })
}

fn orchestration_receipt(
    name: &str,
    args: &serde_json::Value,
    value: &serde_json::Value,
) -> Option<Vec<String>> {
    let name = canonical_tool_name(name);
    let action = match name {
        "list_agents" | "list_jobs" => "list",
        "agent_control" | "job_control" | "interrupt_task" => arg(args, "action"),
        "ask_user_question_async" => {
            return Some(vec![match arg(value, "status") {
                "pending" => "Question available · continue working".into(),
                "cancelled" | "canceled" => "Question dismissed".into(),
                _ => "Question updated · expand for details".into(),
            }]);
        }
        _ => return None,
    };
    let agents = name != "job_control" && name != "list_jobs";
    let rows = match action {
        "list" | "wait" => {
            let records = value
                .get(if agents { "agents" } else { "jobs" })
                .or_else(|| value.get("tasks"))
                .and_then(serde_json::Value::as_array)
                .or_else(|| value.as_array());
            let records = if let Some(records) = records {
                records.as_slice()
            } else if value.get("state").is_some() {
                std::slice::from_ref(value)
            } else {
                return Some(vec![
                    if action == "wait" {
                        "Agent wait completed · expand for details"
                    } else {
                        "Inspection completed · expand for details"
                    }
                    .into(),
                ]);
            };
            let mut rows = vec![format!(
                "{} {}{}",
                records.len(),
                if agents { "agent" } else { "job" },
                if records.len() == 1 { "" } else { "s" }
            )];
            // Attention rows come first so a routine snapshot cannot obscure them.
            let mut records = records.iter().collect::<Vec<_>>();
            records.sort_by_key(|record| !crate::transcript::orchestration_needs_attention(record));
            for record in records.into_iter().take(OTHER_TAIL_LINES - 1) {
                let label = record
                    .get("label")
                    .or_else(|| record.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(if agents { "Agent" } else { "Job" });
                let state = record.get("status").or_else(|| record.get("state"));
                let status = state
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| {
                        state
                            .and_then(|state| state.get("Settled"))
                            .and_then(serde_json::Value::as_str)
                    })
                    .unwrap_or("status unavailable")
                    .to_ascii_lowercase();
                rows.push(format!(
                    "{} · {}",
                    safe_tool_line(label),
                    safe_tool_line(&status)
                ));
            }
            rows
        }
        "interrupt" | "cancel" => vec![match arg(value, "status") {
            "not_found" => "Agent not found".into(),
            "not_running" => "Job is no longer running".into(),
            _ if value.get("requested").and_then(serde_json::Value::as_bool) == Some(true) => {
                "Cancellation requested · awaiting confirmation".into()
            }
            _ => "Cancellation response received · expand for details".into(),
        }],
        "archive" | "close" | "restore" => vec![match arg(value, "status") {
            "not_found" => "Agent not found".into(),
            "requested" => "Request accepted · awaiting confirmation".into(),
            "applied" if action == "restore" => "Agent restored".into(),
            "applied" => "Agent archived".into(),
            _ => "Agent updated · expand for details".into(),
        }],
        "send" => vec!["Message delivered or queued · expand for details".into()],
        "output" => {
            let text = value
                .pointer("/page/text")
                .or_else(|| value.get("text"))
                .or_else(|| value.get("output"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Output available · expand for details");
            text.lines()
                .take(OTHER_TAIL_LINES)
                .map(str::to_owned)
                .collect()
        }
        _ => vec!["Control response received · expand for details".into()],
    };
    Some(rows)
}

fn tool_display_name(name: &str) -> &str {
    match canonical_tool_name(name) {
        "read" => "Read",
        "read_many" => "ReadMany",
        "write" => "Write",
        "edit" | "multi_edit" => "Update",
        "bash" => "Bash",
        "schedule_create" => "CronCreate",
        "schedule_list" => "CronList",
        "schedule_delete" => "CronDelete",
        "workflow" => "Workflow",
        "agent" => "Agent",
        // The reference presents both path and content searches as `Search`;
        // the durable event keeps the real `glob`/`grep` tool name.
        "glob" | "grep" => "Search",
        other => other,
    }
}

fn canonical_tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__heycode__").unwrap_or(name)
}

fn as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

pub(crate) fn safe_tool_line(text: &str) -> String {
    crate::task_console::safe(&text.replace('\t', "    "))
}

fn extract_exit(text: &str) -> Option<(String, bool)> {
    for line in text.lines().rev() {
        if let Some(rest) = line.strip_prefix("[exit code: ") {
            let code = rest.trim_end_matches(']');
            let ok = code == "0";
            return Some((format!("[{code}]"), ok));
        }
        if line.starts_with("[timed out") {
            return Some(("timeout".to_owned(), false));
        }
        if line.starts_with("[killed by signal") {
            return Some((line.to_owned(), false));
        }
    }
    None
}

fn wrap_prose(text: &str, width: usize) -> Vec<String> {
    text.lines()
        .flat_map(|line| {
            if line.is_empty() {
                return vec![String::new()];
            }
            markdown::wrap_styled(&[Span::raw(line.to_owned())], width.max(1), 0)
                .into_iter()
                .map(|line| {
                    line.spans
                        .into_iter()
                        .map(|span| span.content.into_owned())
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Lay a settled command outcome out under the echo that produced it.
///
/// The leader is dim and the text keeps the ordinary body colour, which is
/// what the pinned 2.1.269 captures record (`#999999` leader, terminal
/// default text in dark; `#666666` in light). Continuation rows align under
/// the text column rather than under the glyph.
pub(crate) fn command_receipt_lines(
    text: &str,
    width: usize,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let content = width.saturating_sub(RECEIPT_INDENT).max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    for source in text.lines() {
        let wrapped = if source.is_empty() {
            vec![Line::default()]
        } else {
            markdown::wrap_styled(
                &[Span::styled(
                    source.to_owned(),
                    Style::default().fg(styles.text()),
                )],
                content,
                0,
            )
        };
        for line in wrapped {
            let leader = if rows.is_empty() {
                RECEIPT_PREFIX
            } else {
                RECEIPT_CONTINUATION
            };
            let mut spans = vec![Span::styled(leader, Style::default().fg(styles.dim()))];
            spans.extend(line.spans);
            rows.push(Line::from(spans));
        }
    }
    if rows.is_empty() {
        rows.push(Line::from(Span::styled(
            RECEIPT_PREFIX,
            Style::default().fg(styles.dim()),
        )));
    }
    rows
}

fn wrap_plain_dim(text: &str, width: usize, styles: crate::terminal::Styles) -> Vec<Line<'static>> {
    text.lines()
        .flat_map(|line| {
            if line.is_empty() {
                return vec![Line::default()];
            }
            markdown::wrap_styled(
                &[Span::styled(
                    line.to_owned(),
                    Style::default().fg(styles.dim()),
                )],
                width.max(1),
                0,
            )
        })
        .collect()
}

fn tail(v: &[String], max: usize) -> Vec<String> {
    if v.len() <= max {
        v.to_vec()
    } else {
        v[v.len() - max..].to_vec()
    }
}

// ─── input / ask card / status ─────────────────────────────────────────────

/// Rows the input area needs: one per draft line plus the border, growing from
/// the single-row minimum up to a cap so a pasted block stays fully visible
/// without swallowing the transcript.
pub(crate) fn input_height(state: &AppState, area: Rect) -> u16 {
    const MIN_ROWS: u16 = 1;
    const MAX_ROWS: u16 = 10;
    // Two quiet horizontal rules, a prompt gutter and a small right pad.
    let inner_width = usize::from(area.width.saturating_sub(5)).max(1);
    let rows: usize = state
        .input
        .lines()
        .iter()
        .map(|line| line.chars().count().max(1).div_ceil(inner_width))
        .sum();
    let rows = u16::try_from(rows)
        .unwrap_or(MAX_ROWS)
        .clamp(MIN_ROWS, MAX_ROWS);
    // Never claim more than leaves three transcript rows plus the status line.
    (rows + 2).min(area.height.saturating_sub(4).max(3))
}

fn draw_input(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let mode = if state.vim_enabled() {
        if state.vim_insert() {
            " · VIM INSERT"
        } else {
            " · VIM NORMAL"
        }
    } else {
        ""
    };
    // A terminal with no cursor addressing cannot carry a box; drawing one
    // there produces a garbled frame rather than an ugly one.
    let borders = if state.chrome().borders() {
        Borders::TOP | Borders::BOTTOM
    } else {
        Borders::NONE
    };
    let styles = state.styles();
    let prompt_color = resolve_prompt_color(state.prompt_color(), styles);
    let workflow_requested = heycode_agent::workflow_requested(&state.input.lines().join("\n"));
    let detail = if let Some(caption) = state.voice.caption() {
        Some(caption.to_owned())
    } else if state.task_console.active {
        state
            .task_console
            .selected_record()
            .map(|row| format!(" @{} ", crate::task_console::safe(&row.label)))
    } else {
        if workflow_requested {
            Some(format!("Workflow requested for this turn{mode}"))
        } else {
            match (state.pending_attachments.len(), mode.is_empty()) {
                (0, true) => None,
                (0, false) => Some(mode.trim_start_matches(" · ").to_owned()),
                (count, true) => Some(format!("{count} attachment(s)")),
                (count, false) => Some(format!("{count} attachment(s){mode}")),
            }
        }
    };
    let mut block = Block::default()
        .borders(borders)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(styles.border()));
    if let Some(detail) = detail {
        block = block.title(
            Line::from(Span::styled(
                format!(" {detail} "),
                Style::default().fg(styles.dim()),
            ))
            .alignment(if state.task_console.active {
                Alignment::Right
            } else {
                Alignment::Left
            }),
        );
    }
    if !state.task_console.active
        && let Some(title) = state.current_session_title()
    {
        let title = crate::terminal::truncate_to_width(
            &crate::task_console::safe(title),
            usize::from(area.width.saturating_sub(8).max(1)),
        );
        block = block.title(
            Line::from(Span::styled(
                format!(" {title} "),
                Style::default().fg(prompt_color),
            ))
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_width = inner.width.min(2);
    let prompt = Rect::new(inner.x, inner.y, prompt_width, inner.height);
    let editor = Rect::new(
        inner.x.saturating_add(prompt_width),
        inner.y,
        inner.width.saturating_sub(prompt_width),
        inner.height,
    );
    if prompt.width > 0 {
        let (first_gutter, continuation_gutter) = if prompt.width == 3 {
            ("❯  ", "│  ")
        } else {
            ("❯ ", "  ")
        };
        let mut gutters = vec![Line::from(Span::styled(
            first_gutter,
            Style::default().fg(prompt_color),
        ))];
        gutters.extend((1..prompt.height).map(|_| {
            Line::from(Span::styled(
                continuation_gutter,
                Style::default().fg(styles.border()),
            ))
        }));
        frame.render_widget(Paragraph::new(gutters), prompt);
    }

    // TextArea defaults to underlining the entire active line.
    state.input.set_cursor_line_style(Style::default());
    state.input.set_cursor_style(
        if state.task_console.strip_focused
            || state.task_console.view == crate::task_console::ConsoleView::List
            || state.task_console.preview
        {
            Style::default()
        } else {
            Style::default().add_modifier(ratatui::style::Modifier::REVERSED)
        },
    );
    let pattern = if workflow_requested {
        r"(?i)\b(ultracode|workflow)\b"
    } else {
        ""
    };
    if state
        .input
        .search_pattern()
        .map_or("", |regex| regex.as_str())
        != pattern
    {
        let _valid_pattern = state.input.set_search_pattern(pattern);
    }
    state
        .input
        .set_search_style(Style::default().fg(Color::Rgb(167, 139, 250)).bold());
    frame.render_widget(&state.input, editor);
    if state.input.lines().iter().all(String::is_empty) {
        let placeholder = if !state.pending_message_texts().is_empty() {
            Some("Press up to edit queued messages".into())
        } else if state.task_console.active {
            state
                .task_console
                .selected_record()
                .filter(|row| row.capabilities.steer)
                .map(|row| format!("Message @{}…", crate::task_console::safe(&row.label)))
        } else {
            None
        };
        if let Some(placeholder) = placeholder {
            frame.render_widget(
                Paragraph::new(placeholder).style(Style::default().fg(styles.dim())),
                editor,
            );
        }
    }
    if let Some(hint) = state.quit_hint() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(styles.accent()).bold(),
            ))),
            editor,
        );
    }
}

fn resolve_prompt_color(
    color: Option<crate::human_commands::PromptColor>,
    styles: crate::terminal::Styles,
) -> Color {
    use crate::human_commands::PromptColor;
    use heycode_ui::terminal::ColorLevel;
    use heycode_ui::theme::Rgb;

    let Some(color) = color else {
        return styles.text();
    };
    let light_background = foreground_is_dark(styles.text());
    let rgb = match (color, light_background) {
        (PromptColor::Red, true) => Rgb::new(180, 35, 24),
        (PromptColor::Blue, true) => Rgb::new(23, 92, 211),
        (PromptColor::Green, true) => Rgb::new(6, 118, 71),
        (PromptColor::Yellow, true) => Rgb::new(122, 82, 0),
        (PromptColor::Purple, true) => Rgb::new(105, 56, 239),
        (PromptColor::Orange, true) => Rgb::new(181, 71, 8),
        (PromptColor::Pink, true) => Rgb::new(193, 21, 116),
        (PromptColor::Cyan, true) => Rgb::new(14, 116, 144),
        (PromptColor::Red, false) => Rgb::new(248, 113, 113),
        (PromptColor::Blue, false) => Rgb::new(96, 165, 250),
        (PromptColor::Green, false) => Rgb::new(74, 222, 128),
        (PromptColor::Yellow, false) => Rgb::new(250, 204, 21),
        (PromptColor::Purple, false) => Rgb::new(192, 132, 252),
        (PromptColor::Orange, false) => Rgb::new(251, 146, 60),
        (PromptColor::Pink, false) => Rgb::new(244, 114, 182),
        (PromptColor::Cyan, false) => Rgb::new(34, 211, 238),
    };
    match styles.level() {
        ColorLevel::None => Color::Reset,
        ColorLevel::Basic => match color {
            PromptColor::Red => Color::Red,
            PromptColor::Blue => Color::Blue,
            PromptColor::Green => Color::Green,
            PromptColor::Yellow | PromptColor::Orange => Color::Yellow,
            PromptColor::Purple | PromptColor::Pink => Color::Magenta,
            PromptColor::Cyan => Color::Cyan,
        },
        ColorLevel::Ansi256 => Color::Indexed(heycode_ui::theme::quantize_256(rgb)),
        ColorLevel::TrueColor => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

pub(crate) fn foreground_is_dark(color: Color) -> bool {
    crate::terminal::foreground_is_dark(color)
}

/// Reference-shaped wording for one pending approval: what kind of action it
/// is, which file or command it names, and the question above the choices.
struct ApprovalCopy {
    heading: String,
    subject: Vec<String>,
    content: Vec<Line<'static>>,
    question: String,
    /// The subject cannot describe this call on its own, so the raw argument
    /// preview stays on the card.
    raw_fallback: bool,
}

/// Name the pending action the way the captured reference cards do.
fn approval_copy(
    name: &str,
    args: Option<&serde_json::Value>,
    width: usize,
    styles: crate::terminal::Styles,
) -> ApprovalCopy {
    let canonical = canonical_tool_name(name);
    let value = args.unwrap_or(&serde_json::Value::Null);
    let path = arg(value, "path");
    let leaf = |path: &str| {
        std::path::Path::new(path).file_name().map_or_else(
            || path.to_owned(),
            |leaf| leaf.to_string_lossy().into_owned(),
        )
    };
    let (heading, subject, question) = match canonical {
        "read" | "read_many" => (
            "Read file",
            if path.is_empty() {
                Vec::new()
            } else {
                vec![format!("Read({})", safe_tool_line(path))]
            },
            "Do you want to proceed?".to_owned(),
        ),
        "write" => (
            "Create file",
            if path.is_empty() {
                Vec::new()
            } else {
                vec![safe_tool_line(path)]
            },
            if path.is_empty() {
                "Do you want to create this file?".to_owned()
            } else {
                format!("Do you want to create {}?", leaf(path))
            },
        ),
        "edit" | "multi_edit" => (
            "Edit file",
            if path.is_empty() {
                Vec::new()
            } else {
                vec![safe_tool_line(path)]
            },
            if path.is_empty() {
                "Do you want to make this edit?".to_owned()
            } else {
                format!("Do you want to make this edit to {}?", leaf(path))
            },
        ),
        "bash" => {
            let command = arg(value, "command");
            (
                "Bash command",
                if command.is_empty() {
                    Vec::new()
                } else {
                    vec![safe_tool_line(command)]
                },
                "Do you want to proceed?".to_owned(),
            )
        }
        "schedule_create" => (
            "Create schedule",
            vec![
                format!(
                    "{}: {}",
                    schedule_selector(value),
                    safe_tool_line(arg(value, "prompt"))
                ),
                format!(
                    "Recurring: {}",
                    value
                        .get("recurring")
                        .map_or("unspecified".to_owned(), ToString::to_string)
                ),
            ],
            "Do you want to create this schedule?".to_owned(),
        ),
        "schedule_delete" => (
            "Delete schedule",
            vec![safe_tool_line(arg(value, "schedule_id"))],
            "Do you want to delete this schedule?".to_owned(),
        ),
        "schedule_update" => (
            "Update schedule",
            Vec::new(),
            "Do you want to update this schedule?".to_owned(),
        ),
        "glob" | "grep" => (
            "Read file",
            if arg(value, "pattern").is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "Search({})",
                    safe_tool_line(&summarize_args(canonical, value))
                )]
            },
            "Do you want to proceed?".to_owned(),
        ),
        _ => ("", Vec::new(), "Do you want to proceed?".to_owned()),
    };
    ApprovalCopy {
        heading: if heading.is_empty() {
            tool_display_name(name).to_owned()
        } else {
            heading.to_owned()
        },
        subject,
        // A Create-file card shows the file it would write, numbered, exactly
        // as the reference does above its question.
        content: if canonical == "write" {
            let content = arg(value, "content");
            let mut rows = content
                .lines()
                .take(ASK_PREVIEW_MAX_ROWS)
                .enumerate()
                .map(|(index, row)| {
                    let label = format!(" {} ", index + 1);
                    Line::from(vec![
                        Span::raw(label.clone()),
                        Span::styled(
                            crate::terminal::truncate_to_width(
                                &safe_tool_line(row),
                                width.saturating_sub(label.width()),
                            ),
                            Style::default().fg(styles.text()),
                        ),
                    ])
                })
                .collect::<Vec<_>>();
            let omitted = content.lines().count().saturating_sub(ASK_PREVIEW_MAX_ROWS);
            if omitted > 0 {
                rows.push(Line::styled(
                    format!(" … {omitted} more lines"),
                    Style::default().fg(styles.dim()),
                ));
            }
            rows
        } else {
            Vec::new()
        },
        question,
        raw_fallback: matches!(canonical, "edit" | "multi_edit"),
    }
}

/// The reference answers approvals with `Yes`/`No`; heycode keeps its own real
/// permission outcomes behind those words rather than inventing new ones.
fn approval_choice_label(choice: &str) -> &str {
    match choice {
        "Accept" => "Yes",
        "Allow identical calls this session" => "Yes, and allow identical calls this session",
        "Accept + allow edits this session" => {
            "Yes, and allow file edits this session (commands still ask)"
        }
        "Reject" => "No",
        other => other,
    }
}

fn draw_ask_card(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(ask_card_lines(state, area.width)), area);
}

fn ask_card_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    let Some(ask) = state.pending_ask.as_ref() else {
        return Vec::new();
    };
    let styles = state.styles();
    let text = Style::default().fg(styles.text());
    let dim = Style::default().fg(styles.dim());
    let accent = Style::default().fg(styles.accent());
    let rule = |color| Line::styled("─".repeat(usize::from(width)), Style::default().fg(color));
    let copy = approval_copy(
        &ask.name,
        state.pending_ask_arguments(),
        usize::from(width),
        styles,
    );
    let canonical = canonical_tool_name(&ask.name);
    let file_preview = matches!(canonical, "write" | "edit" | "multi_edit");
    let mut lines = vec![
        rule(styles.accent()),
        Line::styled(format!(" {}", copy.heading), accent.bold()),
    ];
    if let Some(owner) = &ask.owner_label {
        lines.push(Line::styled(
            format!(" {}", crate::task_console::safe(owner)),
            dim,
        ));
    }
    if state.queued_ask_count() > 0 {
        lines.push(Line::styled(
            format!(" {} more waiting", state.queued_ask_count()),
            dim,
        ));
    }
    if let Some(preview) = &ask.edit_preview {
        let mut preview_lines = ask_edit_preview_lines(preview, width, styles);
        lines.push(preview_lines.remove(0));
        lines.push(rule(styles.border()));
        lines.extend(preview_lines);
        lines.push(rule(styles.border()));
    } else {
        if canonical == "bash" {
            lines.push(blank());
        }
        for subject in &copy.subject {
            lines.push(Line::styled(
                format!("{}{subject}", if canonical == "bash" { "   " } else { " " }),
                if file_preview { dim } else { text },
            ));
        }
        if file_preview {
            lines.push(rule(styles.border()));
        }
        lines.extend(copy.content);
        if copy.subject.is_empty() || copy.raw_fallback {
            lines.extend(
                ask_preview_rows(&ask.args_preview, width)
                    .into_iter()
                    .map(|row| Line::styled(row, dim)),
            );
        }
        if file_preview {
            lines.push(rule(styles.border()));
        }
        if canonical == "bash" {
            if let Some(args) = state.pending_ask_arguments() {
                let description = arg(args, "description");
                if !description.is_empty() {
                    lines.push(Line::styled(
                        format!("   {}", safe_tool_line(description)),
                        dim,
                    ));
                }
            }
            lines.push(blank());
            lines.push(Line::styled(" This command requires approval", text));
        }
    }
    if !file_preview {
        lines.push(blank());
    }
    if let Some(reason) = ask.reason() {
        lines.push(Line::from(vec![
            Span::styled(" Amendment: ", accent.bold()),
            Span::styled(reason.to_owned(), text),
            Span::styled("▏", accent),
        ]));
        lines.push(blank());
        lines.push(Line::styled(
            " Enter to submit amendment · Esc to return",
            dim,
        ));
    } else {
        let mut question = vec![Span::styled(format!(" {}", copy.question), text)];
        if file_preview && let Some(args) = state.pending_ask_arguments() {
            let path = arg(args, "path");
            if let Some(leaf) = std::path::Path::new(path)
                .file_name()
                .and_then(|leaf| leaf.to_str())
                && !leaf.is_empty()
                && let Some((before, after)) = copy.question.split_once(leaf)
            {
                question = vec![
                    Span::styled(format!(" {before}"), text),
                    Span::styled(leaf.to_owned(), text.bold()),
                    Span::styled(after.to_owned(), text),
                ];
            }
        }
        lines.push(Line::from(question));
        for (index, choice) in state.approval_choices().iter().enumerate() {
            let selected = ask.selection == index;
            lines.push(Line::styled(
                format!(
                    " {} {}. {}",
                    if selected { "❯" } else { " " },
                    index + 1,
                    approval_choice_label(choice)
                ),
                if selected { accent } else { text },
            ));
        }
        lines.push(blank());
        lines.push(Line::styled(" Esc to cancel · Tab to amend", dim));
    }
    // Size and render the same wrapped rows, including long choices and paths.
    lines
        .into_iter()
        .flat_map(|line| {
            if line.width() <= usize::from(width.max(1)) {
                return vec![line];
            }
            let label = line.to_string();
            let numbered_choice = label
                .trim_start()
                .trim_start_matches('❯')
                .trim_start()
                .split_once(". ")
                .is_some_and(|(number, _)| number.parse::<usize>().is_ok());
            markdown::wrap_styled(
                &line.spans,
                usize::from(width.max(1)),
                if numbered_choice { 6 } else { 1 },
            )
            .into_iter()
            .map(|wrapped| wrapped.style(line.style))
            .collect()
        })
        .collect()
}

/// Most argument rows a card shows before cutting with a marker.
const ASK_PREVIEW_MAX_ROWS: usize = 12;

fn ask_edit_preview_lines(
    preview: &crate::approval_preview::EditApprovalPreview,
    width: u16,
    styles: crate::terminal::Styles,
) -> Vec<Line<'static>> {
    let body_width = usize::from(width.saturating_sub(1).max(1));
    let path = crate::terminal::truncate_to_width(
        crate::markdown::terminal_safe_span(preview.path()).as_ref(),
        body_width,
    );
    let mut lines = vec![Line::from(vec![
        Span::raw(" "),
        Span::styled(path, Style::default().fg(styles.dim())),
    ])];
    for row in preview.rows() {
        let (number, marker, marker_color, background) = match row.kind() {
            crate::approval_preview::EditApprovalRowKind::Context => {
                (row.old_line().unwrap_or_default(), ' ', styles.dim(), None)
            }
            crate::approval_preview::EditApprovalRowKind::Removed => (
                row.old_line().unwrap_or_default(),
                '-',
                styles.diff_marker(false),
                Some(styles.diff_background(false)),
            ),
            crate::approval_preview::EditApprovalRowKind::Added => (
                row.new_line().unwrap_or_default(),
                '+',
                styles.diff_marker(true),
                Some(styles.diff_background(true)),
            ),
        };
        let label = format!("{number:>1} {marker}");
        let text_width = body_width.saturating_sub(label.width());
        let text = crate::terminal::truncate_to_width(row.text(), text_width);
        let mut label_style = Style::default().fg(marker_color);
        let mut text_style = Style::default().fg(styles.text());
        let mut padding = String::new();
        if let Some(background) = background {
            label_style = label_style.bg(background);
            text_style = text_style.bg(background);
            padding = " ".repeat(text_width.saturating_sub(text.width()));
        }
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(label, label_style),
            Span::styled(text, text_style),
            Span::styled(padding, text_style),
        ]));
    }
    lines
}

/// The preview split into rows, cut at [`ASK_PREVIEW_MAX_ROWS`] with a marker.
fn ask_preview_rows(preview: &str, width: u16) -> Vec<String> {
    let mut rows: Vec<String> = preview
        .lines()
        .flat_map(|row| {
            wrap_user_message(
                &format!("  {row}"),
                usize::from(width.saturating_sub(2).max(1)),
            )
        })
        .collect();
    if rows.len() > ASK_PREVIEW_MAX_ROWS {
        let omitted = rows.len() - ASK_PREVIEW_MAX_ROWS;
        rows.truncate(ASK_PREVIEW_MAX_ROWS);
        rows.push(format!("… {omitted} more line(s)"));
    }
    rows
}

/// Height the ask card needs for this state: header, tool, preview rows,
/// blank, choices (two rows) or the reason editor (one), hint, plus the
/// border.
pub(crate) fn ask_card_height(state: &AppState, width: u16) -> u16 {
    u16::try_from(ask_card_lines(state, width).len()).unwrap_or(u16::MAX)
}

fn runtime_question_lines(
    state: &AppState,
    question: &crate::app::PendingRuntimeQuestionView,
    width: u16,
    available_rows: usize,
) -> Vec<Line<'static>> {
    let styles = state.styles();
    let text = Style::default().fg(styles.text());
    let dim = Style::default().fg(styles.dim());
    let accent = Style::default().fg(styles.accent());
    let wrap = |spans: Vec<Span<'static>>, indent| {
        let spans = spans
            .into_iter()
            .map(|mut span| {
                span.content = markdown::terminal_safe_span(&span.content)
                    .into_owned()
                    .into();
                span
            })
            .collect::<Vec<_>>();
        markdown::wrap_styled(&spans, usize::from(width.max(1)), indent)
    };
    let mut header = Vec::new();
    if question.progress.1 > 1 {
        header.push(Line::styled(
            format!(
                "Question {} of {}",
                question.progress.0, question.progress.1
            ),
            dim,
        ));
    }
    if let Some(label) = question.header.as_deref() {
        header.push(Line::from(Span::styled(
            crate::terminal::truncate_to_width(
                &format!(" □ {} ", markdown::terminal_safe_span(label)),
                usize::from(width),
            ),
            accent.reversed(),
        )));
        header.push(blank());
    }
    header.extend(wrap(
        vec![Span::styled(question.prompt.clone(), text.bold())],
        0,
    ));
    header.push(blank());
    let mut options = question
        .choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let selected = index == question.selection;
            let prefix = format!("{}{}. ", if selected { "❯ " } else { "  " }, index + 1);
            let indent = prefix.width();
            let mut rows = wrap(
                vec![
                    Span::styled(if selected { "❯ " } else { "  " }, accent),
                    Span::styled(format!("{}. ", index + 1), dim),
                    Span::styled(
                        if question.mode == heycode_core::QuestionMode::MultipleChoice {
                            format!(
                                "[{}] {choice}",
                                if question.selected_choices.contains(&index) {
                                    "x"
                                } else {
                                    " "
                                }
                            )
                        } else {
                            choice.clone()
                        },
                        if selected { accent } else { text },
                    ),
                ],
                indent,
            );
            if let Some(description) = question
                .choice_descriptions
                .get(index)
                .and_then(Option::as_deref)
            {
                rows.extend(wrap(
                    vec![Span::styled(
                        format!("{}{description}", " ".repeat(indent)),
                        dim,
                    )],
                    indent,
                ));
            }
            rows
        })
        .collect::<Vec<_>>();
    if !question.choices.is_empty() {
        let selected = question.selection == question.choices.len();
        options.push(wrap(
            vec![
                Span::styled(if selected { "❯ " } else { "  " }, accent),
                Span::styled(format!("{}. ", question.choices.len() + 1), dim),
                Span::styled("Type something.", if selected { accent } else { dim }),
            ],
            5,
        ));
    }
    let editing_custom =
        question.choices.is_empty() || question.selection == question.choices.len();
    let mut footer = vec![blank()];
    if editing_custom {
        footer.push(Line::from(vec![
            Span::styled("Answer  ", dim),
            Span::styled(
                runtime_question_visible_answer(question, width),
                text.bold(),
            ),
        ]));
    }
    let hint = if question.mode == heycode_core::QuestionMode::MultipleChoice {
        "Space to toggle · Enter to submit · ↑/↓ navigate · Esc cancel"
    } else if question.choices.is_empty() {
        "Type answer · Enter to submit · Esc to cancel"
    } else {
        "Enter to select · ↑/↓ to navigate · Esc to cancel"
    };
    footer.extend(wrap(vec![Span::styled(hint, dim)], 0));
    panel_frame::option_window(header, options, footer, question.selection, available_rows)
}

fn runtime_question_visible_answer(
    question: &crate::app::PendingRuntimeQuestionView,
    width: u16,
) -> String {
    let available = usize::from(width).saturating_sub("Answer  ".len() + 1);
    let mut used = 0;
    markdown::terminal_safe_span(&question.input)
        .chars()
        .rev()
        .take_while(|ch| {
            used += ch.width().unwrap_or(0);
            used <= available
        })
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn draw_runtime_question(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(question) = state.pending_runtime_question.as_ref() else {
        return;
    };
    if area.is_empty() {
        return;
    }
    let lines = runtime_question_lines(
        state,
        question,
        area.width,
        usize::from(area.height.saturating_sub(1)),
    );
    let answer_row = lines.iter().position(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.content == "Answer  ")
    });
    let mut rows = vec![Line::styled(
        "─".repeat(usize::from(area.width)),
        Style::default().fg(state.styles().border()),
    )];
    rows.extend(lines);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(rows), area);
    if let Some(row) = answer_row {
        let visible = runtime_question_visible_answer(question, area.width);
        let x = area
            .x
            .saturating_add(u16::try_from("Answer  ".len() + visible.width()).unwrap_or(u16::MAX))
            .min(area.right().saturating_sub(1));
        frame.set_cursor_position((
            x,
            area.y
                .saturating_add(u16::try_from(row + 1).unwrap_or(u16::MAX)),
        ));
    }
}

fn draw_mcp_elicitation(frame: &mut Frame<'_>, state: &AppState, area: Rect) {
    let Some(view) = state.pending_mcp_elicitation.as_ref() else {
        return;
    };
    let card = area;
    let mut lines = vec![
        Line::from(Span::styled(
            format!("MCP elicitation · {}", view.server),
            Style::default().fg(state.styles().accent()).bold(),
        )),
        blank(),
        Line::from(Span::styled(
            view.message.clone(),
            Style::default().fg(state.styles().text()),
        )),
        blank(),
    ];
    match &view.mode {
        crate::app::PendingMcpElicitationMode::Form {
            schema,
            input,
            error,
        } => {
            let fields = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .map(|properties| properties.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| "none".to_owned());
            lines.push(Line::from(vec![
                Span::styled("fields  ", Style::default().fg(state.styles().dim())),
                Span::styled(fields, Style::default().fg(state.styles().text())),
            ]));
            lines.push(Line::from(vec![
                Span::styled("JSON    ", Style::default().fg(state.styles().dim())),
                Span::styled(
                    input.clone(),
                    Style::default().fg(state.styles().text()).bold(),
                ),
            ]));
            if let Some(error) = error {
                lines.push(Line::from(Span::styled(
                    error.clone(),
                    Style::default().fg(state.styles().error()),
                )));
            }
            lines.push(Line::from(Span::styled(
                "type one JSON object · enter accept · esc decline",
                Style::default().fg(state.styles().border()),
            )));
        }
        crate::app::PendingMcpElicitationMode::Url { url } => {
            lines.push(Line::from(Span::styled(
                url.clone(),
                Style::default().fg(state.styles().text()),
            )));
            lines.push(Line::from(Span::styled(
                "complete the HTTPS flow · enter acknowledge · esc decline",
                Style::default().fg(state.styles().border()),
            )));
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(state.styles().accent()));
    frame.render_widget(Clear, card);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        card,
    );
}

fn draw_status(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    if area.is_empty() {
        return;
    }
    let fields = status_fields(state);
    // Reserve a context row at narrow widths. This keeps branch and context
    // visible together without guessing either owner's measurement.
    let rows = if area.height > 1 {
        let (context, other): (Vec<_>, Vec<_>) =
            fields.into_iter().partition(|field| field.priority == 90);
        vec![context, other]
    } else {
        vec![fields]
    };
    let styles = state.styles();
    for (index, mut fields) in rows.into_iter().enumerate() {
        if state.foreground_task().is_some() {
            let context_width = fields
                .iter()
                .filter(|field| field.priority == 90)
                .map(|field| crate::terminal::width_of(&field.text))
                .sum::<usize>();
            if let Some(model) = fields.iter_mut().find(|field| field.priority == 100) {
                model.text = crate::terminal::truncate_to_width(
                    &model.text,
                    usize::from(area.width).saturating_sub(context_width).max(1),
                );
            }
        }
        let kept = crate::terminal::fit_status(&fields, area.width);
        let mut spans = Vec::new();
        for field in kept {
            let color = match field.role {
                ThemeRole::Error => styles.error(),
                ThemeRole::Warn => styles.warn(),
                ThemeRole::Accent => styles.accent(),
                ThemeRole::Success => styles.success(),
                ThemeRole::PanelTitle => styles.panel_title(),
                ThemeRole::Code => styles.code(),
                _ => styles.dim(),
            };
            let text = field.text.trim_start_matches(" │ ").trim_start();
            spans.push(Span::styled(
                if spans.is_empty() { "  " } else { " │ " },
                Style::default().fg(styles.border()),
            ));
            spans.push(Span::styled(text.to_owned(), Style::default().fg(color)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, area.y + index as u16, area.width, 1),
        );
    }
}

/// The status line as prioritized fields.
///
/// Context and Git survive before optional model, usage and PR fields.
/// Keeping priorities in values makes narrow degradation assertable.
fn status_fields(state: &AppState) -> Vec<crate::terminal::StatusField> {
    use crate::terminal::StatusField;
    if let Some(owner) = state.foreground_task() {
        return task_status_fields(owner);
    }
    let context = if let Some(budget) = &state.context_budget {
        let prefix = match budget.confidence {
            heycode_llm::ContextConfidence::Exact => "",
            heycode_llm::ContextConfidence::Estimated => "~",
            heycode_llm::ContextConfidence::AtLeast => "≥",
        };
        let mut label = match budget.window {
            Some(window) => {
                let percent = budget.used.saturating_mul(100) / window.max(1);
                let remaining_prefix =
                    if budget.confidence == heycode_llm::ContextConfidence::AtLeast {
                        "≤"
                    } else {
                        prefix
                    };
                format!(
                    " │ ctx:{prefix}{}/{} ({remaining_prefix}{}% left)",
                    compact_token_count(budget.used),
                    compact_token_count(window),
                    100u64.saturating_sub(percent)
                )
            }
            None => format!(
                " │ context {prefix}{} · limit unknown",
                compact_token_count(budget.used)
            ),
        };
        match budget.activity {
            heycode_llm::ContextActivity::Compacting => label.push_str(" · compacting…"),
            heycode_llm::ContextActivity::Failed => label.push_str(" · compaction failed"),
            _ if state.context_meter().is_some_and(|meter| meter.warn) => {
                label.push_str(" · compaction soon")
            }
            _ => {}
        }
        label
    } else if let Some(meter) = state.context_meter() {
        let prefix = if meter.estimated { "~" } else { "" };
        match (meter.percent, state.context_window) {
            (Some(percent), Some(window)) => format!(
                " │ ctx:{prefix}{}/{} ({prefix}{}% left){}",
                compact_token_count(meter.tokens),
                compact_token_count(window),
                100u64.saturating_sub(percent),
                if meter.warn {
                    " · compaction soon"
                } else {
                    ""
                }
            ),
            _ => format!(
                " │ context {prefix}{} · limit unknown",
                compact_token_count(meter.tokens)
            ),
        }
    } else {
        state.context_window.map_or_else(String::new, |window| {
            format!(" │ ctx:—/{}", compact_token_count(window))
        })
    };
    let mut fields = Vec::new();
    let workspace = workspace_status(state);
    if !workspace.is_empty() {
        let role = if matches!(
            state.workspace_context(),
            crate::workspace_context::WorkspaceContextState::Ready(_)
        ) {
            ThemeRole::PanelTitle
        } else {
            ThemeRole::Dim
        };
        fields.push(StatusField::new(workspace, role, 85));
    }
    fields.push(StatusField::new(
        format!(
            "{}{}",
            if fields.is_empty() { "  " } else { " │ " },
            state.active_model_label()
        ),
        ThemeRole::Accent,
        50,
    ));
    if !context.is_empty() {
        fields.push(StatusField::new(context, context_status_role(state), 90));
    }
    if let Some(usage) = state.usage.as_ref() {
        let (input, output) = (usage.prompt_tokens, usage.completion_tokens);
        fields.push(StatusField::new(
            format!(
                " │ in:{} out:{}",
                compact_token_count(input),
                compact_token_count(output)
            ),
            ThemeRole::Code,
            60,
        ));
    }
    if let crate::workspace_context::WorkspaceContextState::Ready(context) =
        state.workspace_context()
        && let crate::workspace_context::PullRequestContext::Found { number, state, .. } =
            &context.pull_request
    {
        fields.push(StatusField::new(
            format!(" │ PR #{number} {}", state.to_ascii_lowercase()),
            ThemeRole::PanelTitle,
            40,
        ));
    }
    fields
}

/// Color reflects known context pressure; unavailable measurements stay neutral.
fn context_status_role(state: &AppState) -> ThemeRole {
    use heycode_llm::ContextActivity;
    let budget = state.context_budget.as_ref();
    let meter = state.context_meter();
    if budget.is_some_and(|budget| {
        budget.activity == ContextActivity::Failed
            || budget
                .window
                .is_some_and(|window| window > 0 && budget.used >= window)
    }) || meter.is_some_and(|meter| meter.percent.is_some_and(|percent| percent >= 100))
    {
        ThemeRole::Error
    } else if budget.is_some_and(|budget| budget.activity == ContextActivity::Compacting)
        || meter.is_some_and(|meter| meter.warn)
    {
        ThemeRole::Warn
    } else if meter.is_some_and(|meter| meter.percent.is_some()) {
        ThemeRole::Success
    } else {
        ThemeRole::Dim
    }
}

/// Task usage is the owner's accounted total across its retained conversation,
/// not the parent's latest request and not a measurement of child context size.
fn task_status_fields(
    owner: crate::task_console::ForegroundTask<'_>,
) -> Vec<crate::terminal::StatusField> {
    use crate::task_console::{TaskKind, safe};
    use crate::terminal::StatusField;
    let record = owner.record;
    if let Some(record) = record
        && record.kind != TaskKind::Child
    {
        return vec![StatusField::new(
            format!("  {} · {}", safe(&record.label), record.status_label()),
            ThemeRole::Dim,
            100,
        )];
    }
    let telemetry = record.map(|record| &record.telemetry);
    let model = telemetry
        .and_then(|telemetry| telemetry.model.as_deref())
        .filter(|model| !model.is_empty());
    let model_role = if model.is_some() {
        ThemeRole::Accent
    } else {
        ThemeRole::Dim
    };
    let model = model.map_or_else(|| "model unavailable".into(), safe);
    let count = |value: Option<u64>| value.map_or_else(|| "—".into(), compact_token_count);
    vec![
        StatusField::new(format!("  {model}"), model_role, 100),
        StatusField::new(" │ context unavailable", ThemeRole::Dim, 90),
        StatusField::new(
            format!(
                " │ total in:{} out:{}",
                count(telemetry.and_then(|telemetry| telemetry.input_tokens)),
                count(telemetry.and_then(|telemetry| telemetry.output_tokens)),
            ),
            ThemeRole::Code,
            60,
        ),
    ]
}

fn workspace_status(state: &AppState) -> String {
    use crate::workspace_context::WorkspaceContextState;
    match state.workspace_context() {
        WorkspaceContextState::Ready(context) => format!(
            "  {}{}",
            crate::terminal::truncate_to_width(&crate::task_console::safe(&context.branch), 30),
            if context.dirty { " *" } else { "" }
        ),
        WorkspaceContextState::Loading => "  Git loading…".into(),
        WorkspaceContextState::NotRepository => String::new(),
        WorkspaceContextState::Unavailable(_) => "  Git unavailable".into(),
    }
}

fn compact_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    if tokens >= 1_000_000 {
        let millions = tokens / 1_000_000;
        let hundred_thousands = (tokens % 1_000_000) / 100_000;
        return if hundred_thousands == 0 {
            format!("{millions}M")
        } else {
            format!("{millions}.{hundred_thousands}M")
        };
    }
    let thousands = tokens / 1_000;
    let hundreds = (tokens % 1_000) / 100;
    if hundreds == 0 {
        format!("{thousands}k")
    } else {
        format!("{thousands}.{hundreds}k")
    }
}

fn compact_cwd(cwd: &std::path::Path) -> String {
    let displayed = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .and_then(|home| {
            cwd.strip_prefix(home)
                .ok()
                .map(std::path::Path::to_path_buf)
        })
        .map_or_else(
            || cwd.display().to_string(),
            |relative| format!("~/{}", relative.display()),
        );
    if displayed.width() <= 64 {
        return displayed;
    }
    let tail = cwd
        .components()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    format!("…/{tail}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        approval_choice_label, approval_copy, ask_card_height, compact_token_count,
        compact_tool_view, focus_edit_stats, resolve_prompt_color, summarize_args,
        tool_display_name,
    };
    use crate::app::{AppState, Item, ToolViewState};
    use crate::human_commands::PromptColor;
    use heycode_agent::UiEvent;
    use ratatui::style::Color;

    fn test_styles() -> crate::terminal::Styles {
        let theme = heycode_ui::theme::default_theme().unwrap();
        crate::terminal::Styles::new(&theme.resolve(heycode_ui::terminal::ColorLevel::TrueColor))
    }

    fn card_text(
        name: &str,
        args: &serde_json::Value,
        result: Option<(bool, serde_json::Value)>,
    ) -> Vec<String> {
        compact_tool_view(name, args, result.as_ref(), None, 100, test_styles())
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn orchestration_receipts_are_semantic_and_wait_is_not_cancellation() {
        let wait = card_text("agent_control", &serde_json::json!({"action":"wait"}), Some((true, serde_json::json!({"agents":[{"label":"Atlas","status":"failed"}],"changed":false,"budget":{"remaining":1234}})))).join("\n");
        assert!(wait.contains("Waiting for agents"), "{wait}");
        assert!(wait.contains("Atlas · failed"), "{wait}");
        assert!(
            !wait.contains("Interrupt") && !wait.contains("1234") && !wait.contains('{'),
            "{wait}"
        );
        let legacy = card_text(
            "interrupt_task",
            &serde_json::json!({"action":"wait","task_id":"task-1"}),
            Some((true, serde_json::json!({"label":"Atlas","state":"failed"}))),
        )
        .join("\n");
        assert!(legacy.contains("Atlas · failed"), "{legacy}");
        let cancel = card_text(
            "job_control",
            &serde_json::json!({"action":"cancel"}),
            Some((
                true,
                serde_json::json!({"requested":true,"status":"requested"}),
            )),
        )
        .join("\n");
        assert!(
            cancel.contains("Cancellation requested · awaiting confirmation"),
            "{cancel}"
        );
        let question = card_text("ask_user_question_async", &serde_json::json!({"question":"Choose scope"}), Some((true, serde_json::json!({"question_id":"hidden-id","status":"pending","instruction":"hidden-instructions"})))).join("\n");
        assert!(question.contains("Question available"), "{question}");
        assert!(!question.contains("hidden-"), "{question}");
    }

    #[test]
    fn workflow_receipts_distinguish_dispatch_requests_and_outcomes() {
        for (action, result, receipt) in [
            (
                "start",
                serde_json::json!({"run_id":"run", "job_id":"job"}),
                "Started in background",
            ),
            (
                "resume",
                serde_json::json!({"run_id":"run", "job_id":"job"}),
                "Resumed in background",
            ),
            (
                "pause",
                serde_json::json!({"pause_requested":true}),
                "Pause requested",
            ),
            (
                "cancel",
                serde_json::json!({"cancel_requested":true}),
                "Cancellation requested",
            ),
            (
                "cancel",
                serde_json::json!({"cancel_requested":false}),
                "No cancellation requested",
            ),
            ("start", serde_json::json!({}), "Request completed"),
        ] {
            let card = card_text(
                "workflow",
                &serde_json::json!({"action":action,"definition":{"title":"Review"}}),
                Some((true, result)),
            );
            assert_eq!(card[0], "⏺ Workflow(Review)");
            assert_eq!(card[1], format!("  ⎿  {receipt}"));
            assert!(!card.join("\n").contains("Completed in"));
        }
    }

    #[test]
    fn expanded_workflow_preserves_request_and_result_metadata() {
        let args = serde_json::json!({"action":"start","definition":{"title":"Review","steps":[{"id":"private-node"}]}});
        let result = (
            true,
            serde_json::json!({"run_id":"private-run","job_id":"private-job"}),
        );
        let lines = super::tool_view(
            "workflow",
            &args,
            Some(&result),
            None,
            120,
            &ToolViewState {
                expanded: true,
                ..Default::default()
            },
            test_styles(),
        );
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Workflow("), "{text}");
        for expected in ["private-node", "private-run", "private-job"] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
    }

    #[test]
    fn finding_card_preserves_reference_semantic_colors_and_header_weight() {
        let styles = test_styles();
        let header = super::finding_row_line(
            "⏺ Code review(medium · 1 finding)".into(),
            super::FindingRowKind::Header,
            styles,
        );
        assert!(
            header.spans[1]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert!(
            !header.spans[2]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        let path =
            super::finding_row_line("  ⎿  sample.py".into(), super::FindingRowKind::Path, styles);
        assert_eq!(path.spans[1].style.fg, Some(styles.accent()));
        let finding = super::finding_row_line(
            "       ● 2 [synthetic] Example finding".into(),
            super::FindingRowKind::Title,
            styles,
        );
        assert_eq!(finding.spans[1].content, "2 ");
        assert_eq!(finding.spans[1].style.fg, Some(styles.accent()));
        assert_eq!(finding.spans[2].style.fg, Some(styles.dim()));
        assert_eq!(finding.spans[3].style.fg, Some(styles.text()));
    }

    #[test]
    fn read_and_search_cards_use_the_reference_receipt_wording() {
        let read = card_text(
            "read",
            &serde_json::json!({"path": "sample.txt"}),
            Some((true, serde_json::json!({"content": "1\talpha\n2\tbeta"}))),
        );
        assert_eq!(read[0], "⏺ Read(sample.txt)");
        assert_eq!(read[1], "  ⎿  Read 2 lines");

        let glob = card_text(
            "glob",
            &serde_json::json!({"pattern": "*.txt"}),
            Some((true, serde_json::json!("a.txt\nb.txt"))),
        );
        assert_eq!(glob[0], "⏺ Search(pattern: \"*.txt\")");
        assert_eq!(glob[1], "  ⎿  Found 2 files");
        assert_eq!(glob[2], "     a.txt");

        let empty = card_text(
            "grep",
            &serde_json::json!({"pattern": "absent", "path": "src"}),
            Some((true, serde_json::json!(""))),
        );
        assert_eq!(empty[0], "⏺ Search(pattern: \"absent\", path: \"src\")");
        assert_eq!(empty[1], "  ⎿  Found 0 lines");
    }

    #[test]
    fn shell_cards_show_the_command_while_running_and_the_exit_when_it_fails() {
        let running = card_text("bash", &serde_json::json!({"command": "sleep 2"}), None);
        assert_eq!(running[1], "  ⎿  $ sleep 2");

        let failed = card_text(
            "bash",
            &serde_json::json!({"command": "exit 3"}),
            Some((true, serde_json::json!("boom\n[exit code: 3]"))),
        );
        assert_eq!(failed[1], "  ⎿  Error: Exit code 3");
        assert_eq!(failed[2], "     boom");

        let ok = card_text(
            "bash",
            &serde_json::json!({"command": "printf hi"}),
            Some((true, serde_json::json!("one\ntwo\n[exit code: 0]"))),
        );
        assert_eq!(ok[1], "  ⎿  one");
        assert_eq!(ok[2], "     two");
        assert!(
            !ok.iter().any(|row| row.contains("[0]")),
            "a clean exit needs no pill: {ok:?}"
        );
    }

    #[test]
    fn write_cards_number_their_content_like_the_reference() {
        let write = card_text(
            "write",
            &serde_json::json!({"path": "notes.txt", "content": "first\nsecond\n"}),
            Some((
                true,
                serde_json::json!({"message": "Wrote 2 lines to notes.txt"}),
            )),
        );
        assert_eq!(write[0], "⏺ Write(notes.txt)");
        assert_eq!(write[1], "  ⎿  Wrote 2 lines to notes.txt");
        assert_eq!(write[2], "      1 first");
        assert_eq!(write[3], "      2 second");
    }

    #[test]
    fn approval_cards_name_the_action_and_answer_with_yes_or_no() {
        let write = approval_copy(
            "write",
            Some(&serde_json::json!({"path": "a/notes.txt", "content": "one\n"})),
            80,
            test_styles(),
        );
        assert_eq!(write.heading, "Create file");
        assert_eq!(write.subject, vec!["a/notes.txt".to_owned()]);
        assert_eq!(write.question, "Do you want to create notes.txt?");
        assert_eq!(write.content.len(), 1);

        let edit = approval_copy(
            "edit",
            Some(&serde_json::json!({"path": "a/sample.txt"})),
            80,
            test_styles(),
        );
        assert_eq!(edit.heading, "Edit file");
        assert_eq!(
            edit.question,
            "Do you want to make this edit to sample.txt?"
        );

        let shell = approval_copy(
            "bash",
            Some(&serde_json::json!({"command": "ls -l"})),
            80,
            test_styles(),
        );
        assert_eq!(shell.heading, "Bash command");
        assert_eq!(shell.subject, vec!["ls -l".to_owned()]);
        assert_eq!(shell.question, "Do you want to proceed?");

        let read = approval_copy(
            "read",
            Some(&serde_json::json!({"path": "sample.txt"})),
            80,
            test_styles(),
        );
        assert_eq!(read.heading, "Read file");
        assert_eq!(read.subject, vec!["Read(sample.txt)".to_owned()]);

        let search = approval_copy(
            "grep",
            Some(&serde_json::json!({"pattern": "delta"})),
            80,
            test_styles(),
        );
        assert_eq!(search.heading, "Read file");
        assert_eq!(
            search.subject,
            vec!["Search(pattern: \"delta\")".to_owned()]
        );

        assert_eq!(approval_choice_label("Accept"), "Yes");
        assert_eq!(approval_choice_label("Reject"), "No");
        assert!(
            approval_choice_label("Allow identical calls this session").starts_with("Yes, and")
        );
        assert!(approval_choice_label("Accept + allow edits this session").starts_with("Yes, and"));
    }

    #[test]
    fn search_tools_share_the_reference_display_label_but_keep_their_arguments() {
        assert_eq!(tool_display_name("glob"), "Search");
        assert_eq!(tool_display_name("grep"), "Search");
        assert_eq!(tool_display_name("agent"), "Agent");
        assert_eq!(tool_display_name("mcp__heycode__agent"), "Agent");
        assert_eq!(
            summarize_args(
                "glob",
                &serde_json::json!({"pattern": "*.rs", "path": "src"})
            ),
            "pattern: \"*.rs\", path: \"src\""
        );
    }

    #[test]
    fn schedule_cards_use_real_selectors_and_result_ids() {
        let args =
            serde_json::json!({"cron":"17 23 * * *", "prompt":"reminder", "recurring":false});
        let card = card_text(
            "schedule_create",
            &args,
            Some((
                true,
                serde_json::json!({"schedule_id":"abc123", "cron":"17 23 * * *", "timezone":"local", "restored_on_resume":true}),
            )),
        );
        assert_eq!(card[0], "⏺ CronCreate(17 23 * * *: reminder)");
        assert_eq!(card[1], "  ⎿  Scheduled abc123 (17 23 * * * · local)");
        assert_eq!(
            super::schedule_selector(&serde_json::json!({"after_seconds":90})),
            "after 90s"
        );
        let approval = approval_copy("schedule_create", Some(&args), 100, test_styles());
        assert_eq!(approval.heading, "Create schedule");
        assert!(approval.subject.iter().any(|row| row == "Recurring: false"));
        assert_eq!(
            card_text(
                "schedule_delete",
                &serde_json::json!({"schedule_id":"abc123"}),
                Some((
                    true,
                    serde_json::json!({"schedule_id":"abc123", "deleted":true})
                ))
            )[1],
            "  ⎿  Deleted abc123"
        );
    }

    #[test]
    fn only_known_subsecond_completed_reasoning_is_hidden() {
        let mut view = crate::app::ReasoningView::default();
        view.elapsed_seconds = Some(0);
        assert!(
            super::reasoning_view(false, Color::Reset, "retained", true, 100, &view).is_empty()
        );
        assert!(
            !super::reasoning_view(true, Color::Reset, "retained", true, 100, &view).is_empty()
        );
        assert!(
            !super::reasoning_view(false, Color::Reset, "retained", false, 100, &view).is_empty()
        );
        view.elapsed_seconds = None;
        assert!(
            !super::reasoning_view(false, Color::Reset, "retained", true, 100, &view).is_empty()
        );
    }

    #[test]
    fn approval_replaces_composer_and_reflows_choices_at_narrow_width() {
        let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
        state.items.push(Item::Tool {
            call_id: None,
            name: "bash".into(),
            args: serde_json::json!({"command":"printf hello", "description":"Print greeting"}),
            result: None,
            untrusted_content: None,
            view: ToolViewState::default(),
        });
        state.apply(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: 7,
            name: "bash".into(),
            args_preview: "command: printf hello".into(),
        });
        for width in [40, 100] {
            let lines = super::ask_card_lines(&state, width);
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            assert_eq!(usize::from(ask_card_height(&state, width)), lines.len());
            let backend = ratatui::backend::TestBackend::new(width, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| super::draw(frame, &mut state))
                .unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("Bash command"));
            assert!(screen.contains("Esc to cancel · Tab to amend"));
            assert!(!screen.contains("shift+tab to cycle"));
            assert!(!screen.contains("╭"));
        }
    }

    #[test]
    fn token_counts_switch_to_millions_at_the_exact_boundary() {
        assert_eq!(compact_token_count(999_999), "999.9k");
        assert_eq!(compact_token_count(1_000_000), "1M");
        assert_eq!(compact_token_count(1_200_000), "1.2M");
    }

    #[test]
    fn prompt_colors_resolve_no_higher_than_the_terminal_tier() {
        let theme = heycode_ui::theme::default_theme().unwrap();
        let none =
            crate::terminal::Styles::new(&theme.resolve(heycode_ui::terminal::ColorLevel::None));
        let basic =
            crate::terminal::Styles::new(&theme.resolve(heycode_ui::terminal::ColorLevel::Basic));
        let ansi =
            crate::terminal::Styles::new(&theme.resolve(heycode_ui::terminal::ColorLevel::Ansi256));
        let truecolor = crate::terminal::Styles::new(
            &theme.resolve(heycode_ui::terminal::ColorLevel::TrueColor),
        );
        assert_eq!(
            resolve_prompt_color(Some(PromptColor::Cyan), none),
            Color::Reset
        );
        assert_eq!(
            resolve_prompt_color(Some(PromptColor::Cyan), basic),
            Color::Cyan
        );
        assert!(matches!(
            resolve_prompt_color(Some(PromptColor::Cyan), ansi),
            Color::Indexed(_)
        ));
        assert!(matches!(
            resolve_prompt_color(Some(PromptColor::Cyan), truecolor),
            Color::Rgb(_, _, _)
        ));

        let light = heycode_ui::theme::builtin_themes()
            .unwrap()
            .into_iter()
            .find(|theme| theme.id().as_str() == "heycode-light")
            .unwrap();
        let light = crate::terminal::Styles::new(
            &light.resolve(heycode_ui::terminal::ColorLevel::TrueColor),
        );
        assert_eq!(
            resolve_prompt_color(Some(PromptColor::Cyan), truecolor),
            Color::Rgb(34, 211, 238),
            "dark surfaces retain the bright session accent"
        );
        assert_eq!(
            resolve_prompt_color(Some(PromptColor::Cyan), light),
            Color::Rgb(14, 116, 144),
            "light surfaces use the contrast-safe session accent"
        );
    }

    #[test]
    fn focus_edit_stats_use_structured_receipts_before_diff_text() {
        assert_eq!(
            focus_edit_stats(&serde_json::json!({
                "diff_preview": {"inserted_lines": 3, "removed_lines": 2},
                "diff": "+ignored\n-ignored"
            })),
            (1, 3, 2)
        );
        assert_eq!(
            focus_edit_stats(&serde_json::json!({
                "diff_previews": [
                    {"inserted_lines": 1, "removed_lines": 0},
                    {"inserted_lines": 2, "removed_lines": 4}
                ]
            })),
            (2, 3, 4)
        );
    }

    #[test]
    fn source_backed_edit_rows_are_included_in_the_approval_card_height() {
        let revision = "68e51be1877f35c23c31de14f66f805b4c87ce666d15fdb7556407ba89ade051";
        let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
        state.items.push(Item::Tool {
            call_id: None,
            name: "read".into(),
            args: serde_json::json!({"path":"src/config.txt"}),
            result: Some((
                true,
                serde_json::json!({
                    "path":"src/config.txt",
                    "content":"   1\talpha\n   2\tmode=slow\n   3\tgamma",
                    "offset":1,
                    "lines_returned":3,
                    "lines_remaining":0,
                    "total_lines":3,
                    "total_bytes":22,
                    "bytes_returned":22,
                    "revision":revision,
                    "page_line_ending":"lf",
                    "truncated":false,
                    "next_offset":null,
                    "next_byte_offset":null,
                    "partial_last_line":false,
                    "scan_limited":false,
                    "continuation":null
                }),
            )),
            untrusted_content: None,
            view: ToolViewState::default(),
        });
        state.items.push(Item::Tool {
            call_id: None,
            name: "edit".into(),
            args: serde_json::json!({
                "path":"src/config.txt",
                "old_string":"mode=slow",
                "new_string":"mode=fast",
                "expected_revision":revision
            }),
            result: None,
            untrusted_content: None,
            view: ToolViewState::default(),
        });
        state.apply(&UiEvent::ApprovalRequested {
            owner_session: None,
            id: 1,
            name: "edit".into(),
            args_preview: "raw fallback has four rows".into(),
        });

        let lines = super::ask_card_lines(&state, 100);
        assert_eq!(usize::from(ask_card_height(&state, 100)), lines.len());
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("2 -mode=slow"));
        assert!(text.contains("2 +mode=fast"));
        assert!(text.contains("Esc to cancel · Tab to amend"));
        assert!(!text.contains("Permission requested"));
        assert_eq!(lines[0].width(), 100);
        assert_eq!(lines[1].style.fg, Some(state.styles().accent()));
        let removed = lines
            .iter()
            .find(|line| line.to_string().contains("-mode=slow"))
            .unwrap();
        assert_eq!(
            removed.width(),
            100,
            "diff highlight fills the full available row"
        );
        assert_eq!(
            removed.spans.last().unwrap().style.bg,
            Some(state.styles().diff_background(false))
        );
        assert!(
            lines.iter().any(|line| line.width() == 0),
            "the choice footer retains its blank separator"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod command_panel_tests {
    use super::{onboarding_panel_height, onboarding_panel_lines};
    use crate::app::AppState;

    fn connect_snapshot() -> heycode_onboarding::OnboardingSnapshot {
        heycode_onboarding::OnboardingSnapshot {
            active: true,
            step: heycode_onboarding::OnboardingStep::RuntimeClass,
            title: "Choose a connection",
            body: "Choose how you'd like to connect.",
            options: vec![
                heycode_onboarding::OnboardingOption {
                    id: "subscription".to_owned(),
                    label: "Use a subscription".to_owned(),
                    description: "Connect your ChatGPT, Claude or Grok account".to_owned(),
                },
                heycode_onboarding::OnboardingOption {
                    id: "local".to_owned(),
                    label: "Use a local model".to_owned(),
                    description: "Connect to LM Studio, Ollama or a custom server".to_owned(),
                },
            ],
            selected: 0,
            search: None,
            input: None,
            from_connect: true,
        }
    }

    fn plain(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn in_session_login_is_a_titled_panel_with_numbered_options() {
        let mut state = AppState::new("test", "/workspace".into());
        state.onboarding = Some(connect_snapshot());
        assert!(state.onboarding_is_panel());
        assert!(!state.onboarding_is_fullscreen());
        let rows = plain(&onboarding_panel_lines(&state, 110));
        assert_eq!(rows.first().map(String::as_str), Some("   Login"));
        assert!(rows.iter().any(|row| row.contains("Select login method:")));
        assert!(
            rows.iter().any(|row| row
                == "   ❯ 1. Use a subscription · Connect your ChatGPT, Claude or Grok account"),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row
                == "     2. Use a local model · Connect to LM Studio, Ollama or a custom server"),
            "{rows:?}"
        );
        assert_eq!(rows.last().map(String::as_str), Some("   Esc to cancel"));
        assert_eq!(
            usize::from(onboarding_panel_height(&state, 110)),
            rows.len() + 1,
            "the surface reserves exactly the rows plus the top rule"
        );
    }

    #[test]
    fn first_run_setup_keeps_the_whole_viewport() {
        let mut state = AppState::new("test", "/workspace".into());
        let mut snapshot = connect_snapshot();
        snapshot.from_connect = false;
        snapshot.step = heycode_onboarding::OnboardingStep::Welcome;
        state.onboarding = Some(snapshot);
        assert!(state.onboarding_is_fullscreen());
        assert!(!state.onboarding_is_panel());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod option_viewport_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn screen(state: &mut AppState) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 24)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(60)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn narrow_theme_keeps_selected_long_label_and_apply_hint() {
        let themes = (0..16)
            .map(|index| {
                heycode_ui::theme::Theme::new(
                    format!("fixture-{index}"),
                    format!("Theme choice {index} with a long accessible descriptive name"),
                    heycode_ui::theme::HEYCODE_DARK,
                )
                .unwrap()
            })
            .collect();
        let bridge = crate::human_commands::HumanCommandBridge::default();
        let mut state = AppState::new("test", "/workspace".into());
        state.set_human_commands(bridge.clone());
        bridge.request(crate::human_commands::HumanCommandRequest::OpenTheme {
            themes,
            selected_id: "fixture-15".to_owned(),
            revision: 0,
        });
        state.poll_human_command();
        let text = screen(&mut state);
        assert!(text.contains("❯ 16. Theme choice 15"), "{text}");
        assert!(text.contains("Enter to select · Esc to cancel"), "{text}");
    }

    #[test]
    fn narrow_keymap_last_selection_and_edit_hint_remain_visible() {
        let bridge = crate::human_commands::HumanCommandBridge::default();
        let mut state = AppState::new("test", "/workspace".into());
        state.set_human_commands(bridge.clone());
        bridge.request(crate::human_commands::HumanCommandRequest::OpenKeymap {
            keymap: heycode_ui::keymap::Keymap::default(),
            revision: 0,
        });
        state.poll_human_command();
        let total = state.keymap_picker().unwrap().rows().len();
        for _ in 1..total {
            state.handle_terminal_event(&Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            )));
        }
        let selected = state.keymap_picker().unwrap().selected();
        assert_eq!(selected, total - 1);
        let text = screen(&mut state);
        assert!(text.contains(&format!("❯ {}.", selected + 1)), "{text}");
        assert!(text.contains("Enter to edit · Esc to close"), "{text}");
    }

    #[test]
    fn narrow_permissions_keep_last_choice_and_wrapped_explanation() {
        let mut state = AppState::new("test", "/workspace".into());
        state.open_permission_picker(heycode_exec::SandboxCapabilityReport {
            effective_mode: heycode_exec::SandboxMode::Off,
            active_backend: None,
            available_backend: None,
            choices: Vec::new(),
        });
        for _ in 0..3 {
            state.handle_terminal_event(&Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            )));
        }
        let text = screen(&mut state);
        assert!(text.contains("❯ 4. Plan"), "{text}");
        assert!(text.contains("Esc to cancel"), "{text}");
    }

    #[test]
    fn narrow_login_many_wrapped_options_retains_selection_and_hint() {
        let mut state = AppState::new("test", "/workspace".into());
        state.onboarding = Some(heycode_onboarding::OnboardingSnapshot {
            active: true, step: heycode_onboarding::OnboardingStep::RuntimeClass,
            title: "Choose a connection", body: "Choose how you'd like to connect.",
            options: (0..12).map(|index| heycode_onboarding::OnboardingOption { id: format!("provider-{index}"), label: format!("Provider {index} with extended connection options"), description: "Connect your subscription using the provider's configurable connection settings".to_owned() }).collect(),
            selected: 11, search: None, input: None, from_connect: true,
        });
        let text = screen(&mut state);
        assert!(text.contains("❯ 12. Provider 11"), "{text}");
        assert!(text.contains("Esc to cancel"), "{text}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod runtime_question_presentation_tests {
    use super::*;

    #[test]
    fn question_display_sanitizes_controls_without_changing_answer_state() {
        let state = AppState::new("test", "/workspace".into());
        let mut question = fixture();
        question.header = Some("Fixture\u{1b}header".into());
        question.prompt = "Prompt\u{7}content".into();
        question.choices[0] = "Alpha\u{9b}label".into();
        question.choice_descriptions[0] = Some("Description\u{1b}detail".into());
        question.selection = 2;
        question.input = "Answer\u{7}typed\u{9b}text".into();
        let lines = runtime_question_lines(&state, &question, 110, 30);
        for line in &lines {
            for span in &line.spans {
                assert!(!span.content.chars().any(char::is_control), "{span:?}");
            }
        }
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Fixture�header",
            "Prompt�content",
            "Alpha�label",
            "Description�detail",
            "Answer�typed�text",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(question.input, "Answer\u{7}typed\u{9b}text");
    }

    fn fixture() -> crate::app::PendingRuntimeQuestionView {
        crate::app::PendingRuntimeQuestionView {
            mode: heycode_core::QuestionMode::SingleChoice,
            progress: (1, 1),
            selected_choices: Default::default(),
            request_id: "fixture-question".into(),
            header: Some("Fixture".into()),
            prompt: "Which synthetic fixture should be selected?".into(),
            choices: vec!["Alpha".into(), "Beta".into()],
            choice_descriptions: vec![
                Some("First local fixture".into()),
                Some("Second local fixture".into()),
            ],
            selection: 0,
            input: String::new(),
        }
    }

    #[test]
    fn question_uses_numbered_choices_chip_and_dim_descriptions() {
        let state = AppState::new("test", "/workspace".into());
        let lines = runtime_question_lines(&state, &fixture(), 110, 17);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("□ Fixture"), "{text}");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            text.contains("❯ 1. Alpha\n     First local fixture"),
            "{text}"
        );
        assert!(text.contains("  3. Type something."), "{text}");
        assert!(
            !text.contains("Other") && !text.contains("Chat about this"),
            "{text}"
        );
        let description = lines
            .iter()
            .find(|line| line.to_string().contains("First local fixture"))
            .unwrap();
        assert!(
            description
                .spans
                .iter()
                .all(|span| span.style.fg == Some(state.styles().dim()))
        );
    }

    #[test]
    fn narrow_question_keeps_last_choice_and_hint_with_long_descriptions() {
        let mut state = AppState::new("test", "/workspace".into());
        let mut question = fixture();
        question.choices = (1..=20)
            .map(|index| format!("Fixture {index} with an extended descriptive option label"))
            .collect();
        question.choice_descriptions = (0..20).map(|_| Some("Long explanation of the fixture choice that wraps across multiple terminal rows".into())).collect();
        question.selection = 19;
        state.pending_runtime_question = Some(question);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .chunks(60)
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("❯ 20. Fixture 20"), "{text}");
        assert!(
            text.contains("Enter to select · ↑/↓ to navigate · Esc to cancel"),
            "{text}"
        );
    }

    #[test]
    fn custom_question_retains_answer_and_cursor_inside_narrow_viewport() {
        let mut state = AppState::new("test", "/workspace".into());
        let mut question = fixture();
        question.selection = 2;
        question.input = "界".repeat(90);
        state.pending_runtime_question = Some(question);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(cursor.x < 60 && cursor.y < 24);
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains("❯ 3. Type something."), "{text}");
        assert!(text.contains("Answer  界"), "{text}");
        assert!(text.contains("Esc to cancel"), "{text}");
    }
}
