//! One visual presentation of the task console's authoritative snapshots.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthChar;

use crate::{
    app::AppState,
    task_console::{
        ConsoleView, TaskCategory, TaskHit, TaskOutputKind, TaskOutputPage, TaskRecord, TaskStatus,
        safe,
    },
};

pub(crate) fn surface_height(state: &AppState, width: u16) -> u16 {
    if state.task_console.active
        && !state.task_console.preview
        && state.task_console.selected_record().is_some_and(|row| {
            matches!(
                row.kind,
                crate::task_console::TaskKind::Child | crate::task_console::TaskKind::Job
            )
        })
    {
        return 0;
    }
    match state.task_console.view {
        ConsoleView::Collapsed => 0,
        ConsoleView::List if state.task_console.category == TaskCategory::Jobs => 0,
        ConsoleView::List => {
            u16::try_from(state.task_console.visible_records().len().clamp(1, 7) + 4).unwrap_or(10)
        }
        ConsoleView::Detail => {
            if state.task_console.selected_record().is_some_and(|row| {
                matches!(
                    row.kind,
                    crate::task_console::TaskKind::Child | crate::task_console::TaskKind::Job
                )
            }) {
                1 + button_layout(state, Rect::new(0, 0, width, u16::MAX)).1
            } else {
                16 + button_layout(state, Rect::new(0, 0, width, u16::MAX)).1
            }
        }
    }
}

pub(crate) fn background_inspector(state: &AppState) -> bool {
    state.task_console.view == ConsoleView::Detail
        && !state.task_console.expanded_output
        && state.task_console.selected_record().is_some_and(|r| {
            !matches!(
                r.kind,
                crate::task_console::TaskKind::Child | crate::task_console::TaskKind::Job
            )
        })
}

fn color(state: &AppState, status: TaskStatus) -> Color {
    match status {
        TaskStatus::Running | TaskStatus::Starting | TaskStatus::Queued => state.styles().accent(),
        TaskStatus::Waiting | TaskStatus::Cancelling | TaskStatus::Cancelled => {
            state.styles().warn()
        }
        TaskStatus::Completed => state.styles().success(),
        TaskStatus::Failed | TaskStatus::Interrupted => state.styles().error(),
        TaskStatus::Idle | TaskStatus::Closed => state.styles().dim(),
    }
}

pub(crate) fn jobs_badge(state: &AppState, _width: u16) -> Option<(String, usize)> {
    let rows = state.strip_task_records();
    let jobs = rows
        .iter()
        .filter(|row| TaskCategory::Jobs.contains(row.kind) && row.status.active())
        .count();
    (jobs > 0).then(|| {
        (
            format!("{jobs} shell{}", if jobs == 1 { "" } else { "s" }),
            jobs,
        )
    })
}

/// Claude-style background browser replaces the composer and footer as one surface.
pub(crate) fn background_open(state: &AppState) -> bool {
    state.task_console.view == ConsoleView::List || state.task_console.preview
}

pub(crate) fn background_height(state: &AppState, available: u16) -> u16 {
    let requested = if state.task_console.preview {
        24
    } else {
        let rows = state.task_console.visible_records();
        let groups = [
            TaskCategory::Agents,
            TaskCategory::Jobs,
            TaskCategory::Teams,
            TaskCategory::Work,
        ]
        .iter()
        .filter(|category| rows.iter().any(|row| category.contains(row.kind)))
        .count();
        if rows.is_empty() {
            // Rule, title, spacer, empty-state row, spacer, hint.
            6
        } else {
            u16::try_from(rows.len().min(14) + groups * 2 + 7).unwrap_or(24)
        }
    };
    requested
        .min(available.saturating_sub(9))
        .max(available.min(6))
}

fn background_controls(
    record: &crate::task_console::TaskRecord,
    width: u16,
) -> (Vec<(Rect, TaskHit, String)>, u16) {
    let mut controls = vec![
        (
            TaskHit::Back,
            if record.status == TaskStatus::Failed {
                "← back · ↑/↓ scroll"
            } else {
                "← to go back"
            },
        ),
        (TaskHit::Main, "Esc/Enter/Space to close"),
    ];
    if record.status == TaskStatus::Failed {
        controls.push((TaskHit::DismissIssue, "d to dismiss issue"));
    }
    if record.capabilities.retry && record.status == TaskStatus::Failed {
        controls.push((TaskHit::Retry, "r to retry"));
    }
    if record.capabilities.interrupt {
        controls.push((TaskHit::Interrupt, "x to stop"));
    }
    if record.kind == crate::task_console::TaskKind::Child || record.capabilities.terminal_input {
        controls.push((TaskHit::Foreground, "f to foreground"));
    }
    let (mut x, mut y) = (0u16, 0u16);
    let mut result = Vec::new();
    for (hit, label) in controls {
        let label_width = (crate::terminal::width_of(label) as u16).min(width);
        if x > 0 && x.saturating_add(label_width + 3) > width {
            x = 0;
            y += 1;
        }
        let text = format!("{}{label}", if x == 0 { "" } else { " · " });
        let size = (crate::terminal::width_of(&text) as u16).min(width.saturating_sub(x));
        result.push((Rect::new(x, y, size, 1), hit, text));
        x += size;
    }
    (result, y + 1)
}

fn elapsed_label(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    if seconds == 0 {
        return "<1s".into();
    }
    let minutes = seconds / 60;
    let hours = minutes / 60;
    if hours > 0 {
        format!("{hours}h {}m", minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}m {}s", seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn progress_name(name: &str) -> String {
    let name = name.strip_prefix("mcp__heycode__").unwrap_or(name);
    match name {
        "bash" => "Bash".into(),
        "read" | "read_many" => "Read".into(),
        "write" => "Write".into(),
        "edit" | "multi_edit" => "Update".into(),
        "glob" => "Glob".into(),
        "grep" => "Grep".into(),
        "task" | "agent" | "spawn_agent" => "Agent".into(),
        other => {
            let mut characters = other.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + characters.as_str()
            })
        }
    }
}

fn progress_argument(name: &str, arguments: &serde_json::Value) -> String {
    let canonical = name.strip_prefix("mcp__heycode__").unwrap_or(name);
    let preferred = match canonical {
        "bash" => ["command", "cmd", "script"].as_slice(),
        "read" | "read_many" | "write" | "edit" | "multi_edit" => ["path", "file"].as_slice(),
        "glob" => ["pattern", "path"].as_slice(),
        "grep" => ["pattern", "query", "path"].as_slice(),
        _ => ["path", "command", "query", "url", "label"].as_slice(),
    };
    preferred
        .iter()
        .find_map(|key| arguments.get(*key).and_then(serde_json::Value::as_str))
        .map(|value| safe(value.lines().next().unwrap_or_default().trim()))
        .unwrap_or_default()
}

fn agent_progress(page: &TaskOutputPage) -> Vec<String> {
    #[derive(Default)]
    struct Progress {
        name: String,
        arguments: serde_json::Value,
        started: bool,
        finished: Option<bool>,
        failure: Option<String>,
    }
    let mut order = Vec::<String>::new();
    let mut calls = std::collections::BTreeMap::<String, Progress>::new();
    for event in &page.events {
        match &event.kind {
            TaskOutputKind::ToolMetadata {
                call_id,
                name,
                args,
            } => {
                if !calls.contains_key(call_id) {
                    order.push(call_id.clone());
                }
                let call = calls.entry(call_id.clone()).or_default();
                call.name.clone_from(name);
                call.arguments.clone_from(args);
            }
            TaskOutputKind::ToolStarted { call_id, name } => {
                if !calls.contains_key(call_id) {
                    order.push(call_id.clone());
                }
                let call = calls.entry(call_id.clone()).or_default();
                if call.name.is_empty() {
                    call.name.clone_from(name);
                }
                call.started = true;
            }
            TaskOutputKind::ToolFinished { call_id, ok, text } => {
                if !calls.contains_key(call_id) {
                    order.push(call_id.clone());
                }
                let call = calls.entry(call_id.clone()).or_default();
                call.finished = Some(*ok);
                if !ok {
                    call.failure = Some(safe(text.lines().next().unwrap_or_default()));
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| calls.remove(&id))
        .map(|call| {
            let display = progress_name(&call.name);
            let argument = progress_argument(&call.name, &call.arguments);
            let suffix = match (call.finished, call.failure) {
                (Some(false), Some(failure)) if !failure.is_empty() => format!(
                    " · failed — {}",
                    crate::terminal::truncate_to_width(&failure, 160)
                ),
                (Some(false), _) => " · failed".into(),
                (Some(true), _) => String::new(),
                (None, _) if call.started => " · running".into(),
                (None, _) => " · waiting".into(),
            };
            format!(
                "  {display}({argument}){suffix}",
                argument = crate::terminal::truncate_to_width(&argument, 240)
            )
        })
        .collect()
}

fn agent_inspector_lines(record: &TaskRecord, page: &TaskOutputPage) -> Vec<String> {
    let mut lines = vec![format!(
        "@{} ({})",
        safe(&record.label),
        record.status_label()
    )];
    let mut failure_details = Vec::new();
    if record.status == TaskStatus::Failed {
        let diagnostic = page
            .events
            .iter()
            .find_map(|event| match &event.kind {
                TaskOutputKind::Diagnostic {
                    diagnostic,
                    terminal: true,
                } => Some(diagnostic),
                _ => None,
            })
            .or(record.telemetry.terminal_diagnostic.as_ref());
        let message = diagnostic
            .map(|diagnostic| diagnostic.message.as_str())
            .or(record.detail.as_deref())
            .unwrap_or("Failure diagnostics were not provided by this execution owner.");
        lines.push(crate::terminal::truncate_to_width(
            &safe(message.lines().next().unwrap_or(message)),
            160,
        ));
        if let Some(diagnostic) = diagnostic {
            failure_details.extend(crate::task_console::diagnostic_lines(diagnostic));
        } else {
            failure_details.push(safe(message));
        }
        if !record.capabilities.retry {
            failure_details.push(
                "Retry unavailable: this failed run has no eligible retained native conversation."
                    .into(),
            );
        }
        if let Some(reason) = &page.unavailable {
            failure_details.push(format!("Output unavailable: {}", safe(reason)));
        }
        let partial = page
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                TaskOutputKind::Text(text) if !text.trim().is_empty() => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !partial.is_empty() {
            failure_details.push("Partial result".into());
            for text in partial {
                failure_details.extend(text.lines().take(6).map(safe));
            }
        }
        lines.push(String::new());
    }
    let mut facts = Vec::new();
    if let Some(milliseconds) = record.telemetry.elapsed_ms {
        facts.push(elapsed_label(milliseconds));
    }
    if let Some(tokens) = record
        .telemetry
        .input_tokens
        .zip(record.telemetry.output_tokens)
        .map(|(input, output)| input.saturating_add(output))
    {
        facts.push(format!("{} tokens", compact_count(tokens)));
    }
    let progress = agent_progress(page);
    if !progress.is_empty() {
        facts.push(format!(
            "{} tool{}",
            progress.len(),
            if progress.len() == 1 { "" } else { "s" }
        ));
    }
    if let Some(model) = record.telemetry.model.as_deref() {
        facts.push(safe(model.rsplit('/').next().unwrap_or(model)));
    }
    if !facts.is_empty() {
        lines.push(facts.join(" · "));
    }
    lines.extend([String::new(), "Progress".into()]);
    if progress.is_empty() {
        lines.push("  No tools reported".into());
    } else {
        lines.extend(progress);
    }
    if record.status != TaskStatus::Failed
        && let Some(detail) = record.detail.as_deref()
    {
        lines.push(format!("  {}", safe(detail)));
    }
    lines.extend([String::new(), "Prompt".into()]);
    let prompt = record.telemetry.initial_prompt.as_deref();
    if record.status == TaskStatus::Failed {
        // A compact first screen retains the requested task beside the failure.
        // The complete diagnostic and original prompt stay in the scrollable body.
        lines.push(
            prompt
                .map(|prompt| {
                    crate::terminal::truncate_to_width(
                        &safe(prompt.lines().next().unwrap_or(prompt)),
                        120,
                    )
                })
                .unwrap_or_else(|| "Prompt unavailable from this execution owner.".into()),
        );
        lines.extend([String::new(), "Diagnostics".into()]);
        lines.extend(failure_details);
        if let Some(prompt) = prompt {
            lines.extend([String::new(), "Full prompt".into()]);
            lines.extend(prompt.lines().map(safe));
        }
    } else if let Some(prompt) = prompt {
        lines.extend(prompt.lines().map(safe));
    } else {
        lines.push("Prompt unavailable from this execution owner.".into());
    }
    lines
}

fn work_inspector_lines(record: &TaskRecord, page: &TaskOutputPage) -> Vec<String> {
    let mut lines = vec![format!(
        "Work · {} ({})",
        safe(&record.label),
        record.status_label()
    )];
    if page.truncated {
        lines.push("Earlier Work revisions are outside retention.".into());
    }
    if let Some(reason) = &page.unavailable {
        lines.push(format!("Work record unavailable: {}", safe(reason)));
    }
    for event in &page.events {
        match &event.kind {
            TaskOutputKind::Diagnostic { diagnostic, .. } => {
                lines.extend(crate::task_console::diagnostic_lines(diagnostic))
            }
            TaskOutputKind::Text(text) | TaskOutputKind::Status(text) => {
                lines.extend(text.lines().take(512).map(safe));
            }
            TaskOutputKind::User(_)
            | TaskOutputKind::Reasoning(_)
            | TaskOutputKind::ToolMetadata { .. }
            | TaskOutputKind::ToolStarted { .. }
            | TaskOutputKind::ToolFinished { .. } => {}
        }
    }
    if lines.len() == 1 {
        lines.push("No Work record is available at this revision.".into());
    }
    lines
}

pub(crate) fn draw_background(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    use crate::task_console::TaskKind;
    let styles = state.styles();
    // The reference draws one full-width upper rule instead of a box border,
    // and indents panel content by three columns.
    frame.render_widget(
        Paragraph::new(crate::panel_frame::top_rule(area.width, styles)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let body = Rect::new(
        area.x.saturating_add(crate::command_panel_frame::INDENT),
        area.y.saturating_add(1),
        area.width
            .saturating_sub(crate::command_panel_frame::INDENT.saturating_add(1)),
        area.height.saturating_sub(2),
    );
    if body.height < 2 {
        return;
    }
    if state.task_console.preview {
        let Some(record) = state.task_console.selected_record().cloned() else {
            return;
        };
        let job = record.kind == TaskKind::Job;
        let work = record.kind == TaskKind::Work;
        let (controls, footer_height) = background_controls(&record, body.width);
        let lines = if job {
            let runtime = record
                .telemetry
                .elapsed_ms
                .map_or("—".into(), elapsed_label);
            vec![
                "Shell details".into(),
                String::new(),
                format!("Status:   {}", record.status_label()),
                format!("Runtime:  {runtime}"),
                format!(
                    "Command:  {}",
                    safe(record.telemetry.command.as_deref().unwrap_or(&record.label))
                ),
                String::new(),
                "Output:".into(),
            ]
        } else if work {
            work_inspector_lines(&record, &state.task_console.page)
        } else {
            agent_inspector_lines(&record, &state.task_console.page)
        };
        let lines = wrap(&lines, usize::from(body.width));
        let header_height = if job {
            (lines.len() as u16).min(body.height.saturating_sub(footer_height + 3))
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(lines.iter().cloned().map(Line::raw).collect::<Vec<_>>())
                .style(Style::default().fg(styles.text())),
            Rect::new(body.x, body.y, body.width, header_height),
        );
        let output_area = Rect::new(
            body.x,
            body.y + header_height,
            if job {
                body.width.saturating_sub(2)
            } else {
                body.width
            },
            body.height
                .saturating_sub(header_height + footer_height + 1)
                .min(if job { 12 } else { u16::MAX }),
        );
        let output_box = Block::default()
            .borders(if job { Borders::ALL } else { Borders::NONE })
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(styles.border()));
        let mut inner = output_box.inner(output_area);
        if job && inner.width > 2 {
            inner.x += 1;
            inner.width -= 2;
        }
        frame.render_widget(output_box, output_area);
        let lines = if job {
            wrap(&state.task_console.output_lines(), usize::from(inner.width))
        } else {
            lines
        };
        state.task_console.output_rows = lines.len();
        state.task_console.output_height = usize::from(inner.height);
        let max = lines.len().saturating_sub(usize::from(inner.height));
        state.task_console.output_scroll = state.task_console.output_scroll.min(max);
        let start = if !job {
            state.task_console.output_scroll.min(max)
        } else if state.task_console.follow {
            max
        } else {
            max.saturating_sub(state.task_console.output_scroll)
        };
        let shown = lines
            .len()
            .saturating_sub(start)
            .min(usize::from(inner.height));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(start)
                    .take(shown)
                    .map(Line::raw)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(styles.text())),
            inner,
        );
        if job && output_area.bottom() < body.bottom() {
            frame.render_widget(
                Paragraph::new(format!("Showing {shown} lines"))
                    .style(Style::default().fg(styles.dim())),
                Rect::new(body.x, output_area.bottom(), body.width, 1),
            );
        }
        for (mut rect, hit, label) in controls {
            rect.x += body.x;
            rect.y += area.bottom().saturating_sub(footer_height);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(styles.dim())),
                rect,
            );
            state.task_console.hits.push((rect, hit));
        }
        return;
    }
    let rows = state
        .task_console
        .visible_records()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let agents = rows
        .iter()
        .filter(|row| row.kind == TaskKind::Child)
        .count();
    let shells = rows
        .iter()
        .filter(|row| row.kind == TaskKind::Job && row.status.active())
        .count();
    let mut summaries = Vec::new();
    if agents > 0 {
        summaries.push(format!(
            "{agents} agent{}",
            if agents == 1 { "" } else { "s" }
        ));
    }
    if shells > 0 {
        summaries.push(format!(
            "{shells} active shell{}",
            if shells == 1 { "" } else { "s" }
        ));
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Background",
                Style::default().fg(styles.panel_title()).bold(),
            )),
            Line::raw(summaries.join(" · ")),
        ])
        .style(Style::default().fg(styles.text())),
        Rect::new(body.x, body.y, body.width, 2),
    );
    let mut entries: Vec<(String, Option<TaskHit>)> = Vec::new();
    for (category, label) in [
        (TaskCategory::Agents, "Agents"),
        (TaskCategory::Jobs, "Shells"),
        (TaskCategory::Teams, "Teams"),
        (TaskCategory::Work, "Work"),
    ] {
        let group = rows
            .iter()
            .filter(|row| category.contains(row.kind))
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        entries.push((String::new(), None));
        entries.push((format!("  {label} ({})", group.len()), None));
        if category == TaskCategory::Agents {
            entries.push(("  @main".into(), Some(TaskHit::Main)));
        }
        for row in group {
            let label = if row.kind == TaskKind::Child {
                format!("  @{}: {}", safe(&row.label), row.status_label())
            } else {
                format!("  {} ({})", safe(&row.label), row.status_label())
            };
            entries.push((label, Some(TaskHit::Open(row.key.clone()))));
        }
    }
    if entries.is_empty() {
        entries.push(("No tasks currently running".into(), None));
    }
    let height = usize::from(body.height.saturating_sub(3));
    let focus = entries
        .iter()
        .position(|(_, hit)| match hit {
            Some(TaskHit::Main) => state.task_console.focus_main,
            Some(TaskHit::Open(key)) => {
                !state.task_console.focus_main && state.task_console.focused.as_ref() == Some(key)
            }
            _ => false,
        })
        .unwrap_or(0);
    let start = focus.saturating_sub(height.saturating_sub(1));
    for (offset, (label, hit)) in entries.into_iter().skip(start).take(height).enumerate() {
        let rect = Rect::new(body.x, body.y + 2 + offset as u16, body.width, 1);
        let selected = offset + start == focus && hit.is_some();
        let label = if selected {
            format!("› {}", label.trim_start())
        } else {
            label
        };
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(styles.text()).add_modifier(
                if selected {
                    ratatui::style::Modifier::BOLD
                } else {
                    ratatui::style::Modifier::empty()
                },
            )),
            rect,
        );
        if let Some(hit) = hit {
            state.task_console.hits.push((rect, hit));
        }
    }
    frame.render_widget(
        Paragraph::new("↑/↓ to select · Enter to view · Esc to close")
            .style(Style::default().fg(styles.dim())),
        Rect::new(body.x, area.bottom().saturating_sub(1), body.width, 1),
    );
}

pub(crate) fn foreground_strip_height(state: &AppState, available: u16) -> u16 {
    if state.task_console.preview || state.task_console.view == ConsoleView::List || available < 12
    {
        return 0;
    }
    let count = state.active_child_records().len();
    if count == 0 && !state.task_console.strip_focused {
        0
    } else {
        let hint = u16::from(state.task_console.active);
        (count.min(3) as u16 + u16::from(count > 3) + hint + 1).min(available / 3)
    }
}

/// Fill denotes the selected conversation; the ring color denotes its status.
fn selection_ring(selected: bool) -> &'static str {
    if selected { "●" } else { "○" }
}

fn strip_line(
    state: &AppState,
    status: TaskStatus,
    label: &str,
    elapsed: Option<u64>,
    focused: bool,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let width = usize::from(width);
    let timer_width = 12.min(width);
    let label = crate::terminal::truncate_to_width(
        &safe(label),
        width.saturating_sub(timer_width + 3).min(48),
    );
    let timer = elapsed.map_or_else(|| "—".into(), elapsed_label);
    let timer = crate::terminal::truncate_to_width(&timer, timer_width);
    let gap = width
        .saturating_sub(2 + crate::terminal::width_of(&label) + crate::terminal::width_of(&timer));
    let label_style = Style::default()
        .fg(if selected || focused {
            state.styles().text()
        } else {
            state.styles().dim()
        })
        .add_modifier(if focused {
            ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED
        } else if selected {
            ratatui::style::Modifier::BOLD
        } else {
            ratatui::style::Modifier::empty()
        });
    Line::from(vec![
        Span::styled(
            selection_ring(selected),
            Style::default().fg(color(state, status)),
        ),
        Span::raw(" "),
        Span::styled(label, label_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(timer, Style::default().fg(state.styles().dim())),
    ])
}

pub(crate) fn draw_strip(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    // Once the last child settles, a focused selector still owns its Main row.
    if area.height <= u16::from(state.task_console.active) {
        return;
    }
    let styles = state.styles();
    let hint_rows = u16::from(state.task_console.active);
    if state.task_console.active {
        frame.render_widget(
            Paragraph::new("↑/↓ to select · Enter to view")
                .style(Style::default().fg(styles.dim())),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }
    let main_focused = state.task_console.strip_focused && state.task_console.focus_main;
    let main_active = !state.task_console.active;
    let main = Rect::new(area.x, area.y + hint_rows, area.width, 1);
    let main_status = if state.cancellation_requested && state.has_active_turn() {
        TaskStatus::Cancelling
    } else if state.pending_ask.is_some() || state.pending_runtime_question.is_some() {
        TaskStatus::Waiting
    } else if state.has_active_turn() {
        TaskStatus::Running
    } else {
        TaskStatus::Idle
    };
    let main_elapsed = state
        .turn_started_at
        .map(|started| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    frame.render_widget(
        Paragraph::new(strip_line(
            state,
            main_status,
            "main",
            main_elapsed,
            main_focused,
            main_active,
            main.width,
        )),
        main,
    );
    state.task_console.hits.push((main, TaskHit::Main));
    let rows = state
        .active_child_records()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let available_rows = usize::from(area.height.saturating_sub(hint_rows + 1));
    let overflow = rows.len() > available_rows.min(3);
    let capacity = available_rows.saturating_sub(usize::from(overflow)).min(3);
    let total = rows.len();
    let target = if state.task_console.strip_focused {
        state.task_console.focused.as_ref()
    } else if state.task_console.active {
        state.task_console.selected.as_ref()
    } else {
        None
    };
    let target_index = target.and_then(|target| rows.iter().position(|row| &row.key == target));
    let start = target_index
        .map(|index| index.saturating_sub(capacity.saturating_sub(1)))
        .unwrap_or(0);
    for (index, row) in rows.into_iter().skip(start).take(capacity).enumerate() {
        let active =
            state.task_console.active && state.task_console.selected.as_ref() == Some(&row.key);
        let focused = state.task_console.strip_focused
            && !state.task_console.focus_main
            && state.task_console.focused.as_ref() == Some(&row.key);
        let rect = Rect::new(area.x, area.y + hint_rows + 1 + index as u16, area.width, 1);
        let ring_status = row.status;
        frame.render_widget(
            Paragraph::new(strip_line(
                state,
                ring_status,
                &row.label,
                row.telemetry.elapsed_ms,
                focused,
                active,
                rect.width,
            )),
            rect,
        );
        // Foreground switching stays one click; Background list opens the inspector first.
        state
            .task_console
            .hits
            .push((rect, TaskHit::Switch(row.key)));
    }
    if overflow && available_rows > 0 {
        let rect = Rect::new(
            area.x,
            area.y + hint_rows + 1 + capacity as u16,
            area.width,
            1,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "+{} more · /agents to view all",
                total.saturating_sub(capacity)
            ))
            .style(Style::default().fg(styles.dim())),
            rect,
        );
        state
            .task_console
            .hits
            .push((rect, TaskHit::Category(TaskCategory::Agents)));
    }
}

pub(crate) fn draw_surface(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    if area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(state.styles().border()));
    let body = block.inner(area);
    frame.render_widget(block, area);
    if body.height == 0 {
        return;
    }
    if state.task_console.view == ConsoleView::List {
        draw_list(frame, state, body);
    } else if background_inspector(state) {
        let controls = (6 + button_layout(state, body).1).min(body.height);
        let output = Rect::new(
            body.x,
            body.y,
            body.width,
            body.height.saturating_sub(controls),
        );
        draw_output(frame, state, output);
        draw_details(
            frame,
            state,
            Rect::new(body.x, output.bottom(), body.width, controls),
        );
    } else {
        draw_details(frame, state, body);
    }
}

pub(crate) fn draw_jobs_list(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .title(Span::styled(
            " Background shells and jobs ",
            Style::default().fg(state.styles().text()).bold(),
        ))
        .border_style(Style::default().fg(state.styles().border()));
    let body = block.inner(area);
    frame.render_widget(block, area);
    draw_list(frame, state, body);
}

fn draw_list(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let area = if state.task_console.category == TaskCategory::Jobs {
        area
    } else {
        let main = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {} Main conversation",
                if state.task_console.focus_main {
                    "▸"
                } else {
                    " "
                },
                selection_ring(!state.task_console.active)
            ))
            .style(Style::default().fg(state.styles().accent())),
            main,
        );
        state.task_console.hits.push((main, TaskHit::Main));
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        )
    };
    let row_count = usize::from(area.height.saturating_sub(2));
    let selected = state
        .task_console
        .visible_records()
        .iter()
        .position(|r| Some(&r.key) == state.task_console.focused.as_ref())
        .unwrap_or(0);
    let start = selected.saturating_sub(row_count.saturating_sub(1));
    if state.task_console.visible_records().is_empty() {
        frame.render_widget(
            Paragraph::new(if state.task_console.category == TaskCategory::All {
                "No activity yet.".into()
            } else {
                format!(
                    "No {} yet.",
                    state.task_console.category.label().to_lowercase()
                )
            })
            .style(Style::default().fg(state.styles().dim())),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }
    let rows = state
        .task_console
        .visible_records()
        .into_iter()
        .skip(start)
        .take(row_count)
        .cloned()
        .collect::<Vec<_>>();
    for (offset, row) in rows.into_iter().enumerate() {
        let selected =
            !state.task_console.focus_main && Some(&row.key) == state.task_console.focused.as_ref();
        let row_area = Rect::new(
            area.x,
            area.y + u16::try_from(offset).unwrap_or(0),
            area.width,
            1,
        );
        let style = Style::default().fg(color(state, row.status));
        let spans = vec![
            Span::styled(
                if selected { "▸ " } else { "  " },
                Style::default().fg(state.styles().accent()).bold(),
            ),
            Span::styled(
                format!(
                    "{} ",
                    selection_ring(
                        state.task_console.active
                            && state.task_console.selected.as_ref() == Some(&row.key)
                    )
                ),
                Style::default().fg(color(state, row.status)),
            ),
            Span::styled(format!("[{}] ", row.status_label()), style),
            Span::styled(
                safe(&row.label),
                if selected {
                    Style::default().fg(state.styles().text()).bold()
                } else {
                    Style::default().fg(state.styles().text())
                },
            ),
            Span::styled(
                format!(
                    " · {}",
                    match row.kind {
                        crate::task_console::TaskKind::Child => "conversation",
                        crate::task_console::TaskKind::Job => "background",
                        crate::task_console::TaskKind::Tool => "tool",
                        crate::task_console::TaskKind::Team => "team",
                        crate::task_console::TaskKind::Work => "work",
                    }
                ) + &row
                    .telemetry
                    .elapsed_ms
                    .map(|elapsed| format!(" · {}", elapsed_label(elapsed)))
                    .unwrap_or_default(),
                Style::default().fg(state.styles().dim()),
            ),
        ];
        frame.render_widget(Paragraph::new(Line::from(spans)), row_area);
        state
            .task_console
            .hits
            .push((row_area, TaskHit::Open(row.key)));
    }
    if area.height >= 2 {
        let message = state
            .task_console
            .notice
            .as_deref()
            .unwrap_or("↑↓ select · Enter/click open · Alt+R raw output · Esc return");
        frame.render_widget(
            Paragraph::new(safe(message)).style(Style::default().fg(state.styles().dim())),
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
        );
    }
}

fn draw_details(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let Some(row) = state.task_console.selected_record().cloned() else {
        frame.render_widget(
            Paragraph::new(
                "Task no longer in live inventory. Esc returns with your draft preserved.",
            ),
            area,
        );
        return;
    };
    if matches!(
        row.kind,
        crate::task_console::TaskKind::Child | crate::task_console::TaskKind::Job
    ) {
        let (buttons, _) = button_layout(state, area);
        for (label, hit, mut button) in buttons {
            button.y += area.y;
            if button.y >= area.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(format!("[{label}] "))
                    .style(Style::default().fg(state.styles().accent())),
                button,
            );
            state.task_console.hits.push((button, hit));
        }
        return;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(
            safe(&row.label),
            Style::default().fg(state.styles().text()).bold(),
        ),
        Span::styled(
            format!(" · {}", row.status_label()),
            Style::default().fg(color(state, row.status)),
        ),
    ])];
    if row.kind == crate::task_console::TaskKind::Work {
        lines.extend(row.detail_lines().into_iter().skip(1).map(Line::raw));
        if let Some(notice) = state.task_console.selected_notice() {
            lines.push(Line::raw(safe(notice)));
        }
    } else if row.kind == crate::task_console::TaskKind::Job {
        let telemetry = &row.telemetry;
        let elapsed = telemetry.elapsed_ms.map_or_else(
            || "unavailable".into(),
            |ms| format!("{}.{:01}s", ms / 1000, (ms % 1000) / 100),
        );
        lines.push(Line::raw(format!(
            "$ {}",
            safe(
                telemetry
                    .command
                    .as_deref()
                    .unwrap_or("Command unavailable")
            )
        )));
        lines.push(Line::styled(
            safe(
                telemetry
                    .workspace
                    .as_deref()
                    .unwrap_or("Directory unavailable"),
            ),
            Style::default().fg(state.styles().dim()),
        ));
        lines.push(Line::raw(format!(
            "Elapsed {elapsed} · time limit {}",
            telemetry.deadline.as_deref().unwrap_or("unavailable")
        )));
        lines.push(Line::styled(
            format!(
                "Started {}{}",
                telemetry.started.as_deref().unwrap_or("unavailable"),
                telemetry
                    .finished
                    .as_ref()
                    .map_or_else(String::new, |time| format!(" · ended {time}"))
            ),
            Style::default().fg(state.styles().dim()),
        ));
        lines.push(Line::raw(safe(
            row.detail
                .as_deref()
                .or(state.task_console.selected_notice())
                .unwrap_or(if state.task_console.follow {
                    "Following live output"
                } else {
                    "Reading retained output"
                }),
        )));
    } else {
        let telemetry = &row.telemetry;
        let elapsed = telemetry.elapsed_ms.map_or_else(
            || "unavailable".into(),
            |ms| format!("{}.{:01}s", ms / 1000, (ms % 1000) / 100),
        );
        let tokens = match (telemetry.input_tokens, telemetry.output_tokens) {
            (Some(input), Some(output)) => format!(" · {input} in / {output} out"),
            _ => String::new(),
        };
        lines.push(Line::styled(
            format!("Elapsed {elapsed}{tokens}"),
            Style::default().fg(state.styles().dim()),
        ));
        lines.push(Line::raw(safe(
            row.detail
                .as_deref()
                .or(state.task_console.selected_notice())
                .unwrap_or("Alt+↑↓ tool groups · Enter/click expand · Ctrl+T switch conversation"),
        )));
    }
    for line in &mut lines {
        if line.width() > usize::from(area.width) {
            *line = Line::raw(crate::terminal::truncate_to_width(
                &line.to_string(),
                usize::from(area.width),
            ));
        }
    }
    let (buttons, button_rows) = button_layout(state, area);
    let text_height = area.height.saturating_sub(button_rows);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(state.styles().text())),
        Rect::new(area.x, area.y, area.width, text_height),
    );
    for (label, hit, mut button) in buttons {
        button.y += area.y + text_height;
        if button.y >= area.bottom() {
            break;
        }
        frame.render_widget(
            Paragraph::new(format!("[{label}] "))
                .style(Style::default().fg(state.styles().accent())),
            button,
        );
        state.task_console.hits.push((button, hit));
    }
}

fn button_layout(state: &AppState, area: Rect) -> (Vec<(&'static str, TaskHit, Rect)>, u16) {
    let mut buttons = vec![
        ("Alt+← Back", TaskHit::Back),
        ("Alt+M Details", TaskHit::Metadata),
    ];
    if let Some(row) = state.task_console.selected_record() {
        if row.kind == crate::task_console::TaskKind::Child {
            buttons.push(("Alt+R Raw", TaskHit::Raw));
        } else if row.kind == crate::task_console::TaskKind::Job {
            buttons.push(("Alt+O Stream", TaskHit::Channel));
        } else if row.kind != crate::task_console::TaskKind::Work {
            buttons.push(("PgUp Older", TaskHit::Older));
            buttons.push(("Ctrl+End Live", TaskHit::Follow));
        }
        if row.capabilities.interrupt {
            buttons.push(("Alt+I Stop", TaskHit::Interrupt));
        }
        if row.capabilities.retry && row.status == TaskStatus::Failed {
            buttons.push(("Alt+T Retry", TaskHit::Retry));
        }
        if row.capabilities.close {
            buttons.push(("Alt+X Close", TaskHit::Close));
        }
        if row.capabilities.background {
            buttons.push(("Alt+B Background", TaskHit::Background));
        }
    }
    let mut result = Vec::new();
    let mut x = area.x;
    let mut y = 0;
    for (label, hit) in buttons {
        let width = u16::try_from(label.len() + 3)
            .unwrap_or(u16::MAX)
            .min(area.width);
        if x > area.x && x.saturating_add(width) > area.right() {
            y += 1;
            x = area.x;
        }
        result.push((label, hit, Rect::new(x, y, width, 1)));
        x = x.saturating_add(width);
    }
    (result, y + 1)
}

/// Replace only the visible transcript region. Parent transcript state and
/// active ingestion stay untouched while the task's own output is shown.
pub(crate) fn draw_output(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    state.transcript_area = area;
    let title = state.task_console.selected_record().map_or_else(
        || "Task output".to_owned(),
        |record| {
            if record.kind == crate::task_console::TaskKind::Child
                || state.task_console.channel == crate::task_console::TaskOutputChannel::Default
            {
                format!("{} · {}", safe(&record.label), record.status_label())
            } else {
                format!(
                    "{} · {}",
                    safe(&record.label),
                    state.task_console.channel.label()
                )
            }
        },
    );
    let child_conversation = state
        .task_console
        .selected_record()
        .is_some_and(|row| row.kind == crate::task_console::TaskKind::Child)
        && !state.task_console.show_metadata;
    let inner = if child_conversation {
        area
    } else {
        let block = Block::default()
            .borders(Borders::TOP)
            .title(Span::styled(
                title,
                Style::default().fg(state.styles().dim()),
            ))
            .border_style(Style::default().fg(state.styles().border()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    };
    let output_lines = if state.task_console.show_metadata {
        state.task_console.selected_record().map_or_else(
            || vec!["Task no longer available".into()],
            |record| record.detail_lines(),
        )
    } else {
        state.task_console.output_lines()
    };
    let child = state
        .task_console
        .selected_record()
        .is_some_and(|r| r.kind == crate::task_console::TaskKind::Child);
    let (lines, tool_rows): (Vec<Line<'static>>, Vec<(usize, String)>) =
        if child && !state.task_console.show_metadata && !state.task_console.raw_output {
            conversation_lines(state, usize::from(inner.width))
        } else {
            (
                wrap(&output_lines, usize::from(inner.width))
                    .into_iter()
                    .map(Line::raw)
                    .collect(),
                Vec::new(),
            )
        };
    state.task_console.output_rows = lines.len();
    state.task_console.output_height = usize::from(inner.height);
    let max_scroll = lines.len().saturating_sub(usize::from(inner.height));
    state.task_console.output_scroll = state.task_console.output_scroll.min(max_scroll);
    let start = if state.task_console.show_metadata {
        state.task_console.output_scroll.min(max_scroll)
    } else if state.task_console.follow {
        max_scroll
    } else {
        max_scroll.saturating_sub(state.task_console.output_scroll)
    };
    for (row, id) in tool_rows {
        if row >= start && row < start + usize::from(inner.height) {
            state.task_console.hits.push((
                Rect::new(inner.x, inner.y + (row - start) as u16, inner.width, 1),
                TaskHit::ToolDetails(id),
            ));
        }
    }
    let visible = lines
        .into_iter()
        .skip(start)
        .take(usize::from(inner.height))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(visible).style(Style::default().fg(state.styles().text())),
        inner,
    );
}

fn conversation_lines(
    state: &mut AppState,
    width: usize,
) -> (Vec<Line<'static>>, Vec<(usize, String)>) {
    use crate::{
        app::{Item, ToolViewState},
        task_console::TaskOutputKind,
    };
    let page = &state.task_console.page;
    let mut items = Vec::new();
    if page.truncated {
        items.push(Item::Info("Earlier messages expired from retention".into()));
    }
    if let Some(reason) = &page.unavailable {
        items.push(Item::Error(format!("Output unavailable: {reason}")));
    }
    let mut tool_indices = std::collections::BTreeMap::<String, usize>::new();
    let metadata = page
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            TaskOutputKind::ToolMetadata {
                call_id,
                name,
                args,
            } => Some((call_id.as_str(), (name, args))),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for event in &page.events {
        match &event.kind {
            TaskOutputKind::User(text) => items.push(Item::User(text.clone())),
            TaskOutputKind::Text(text) => items.push(Item::Assistant(text.clone())),
            TaskOutputKind::Reasoning(_) | TaskOutputKind::ToolMetadata { .. } => {}
            TaskOutputKind::ToolStarted { call_id, name } => {
                let (_, args) = metadata
                    .get(call_id.as_str())
                    .copied()
                    .unwrap_or((name, &serde_json::Value::Null));
                tool_indices.insert(call_id.clone(), items.len());
                items.push(Item::Tool {
                    call_id: Some(heycode_core::CallId::from_raw(call_id)),
                    name: name.clone(),
                    args: args.clone(),
                    result: None,
                    untrusted_content: None,
                    view: ToolViewState::default(),
                });
            }
            TaskOutputKind::ToolFinished { call_id, ok, text } => {
                let result = Some((
                    *ok,
                    serde_json::from_str(text)
                        .unwrap_or_else(|_| serde_json::Value::String(text.clone())),
                ));
                if let Some(index) = tool_indices.get(call_id) {
                    if let Item::Tool { result: target, .. } = &mut items[*index] {
                        *target = result;
                    }
                } else {
                    let (name, args) = metadata
                        .get(call_id.as_str())
                        .map(|(name, args)| ((*name).clone(), (*args).clone()))
                        .unwrap_or_else(|| ("tool".into(), serde_json::Value::Null));
                    tool_indices.insert(call_id.clone(), items.len());
                    items.push(Item::Tool {
                        call_id: Some(heycode_core::CallId::from_raw(call_id)),
                        name,
                        args,
                        result,
                        untrusted_content: None,
                        view: ToolViewState::default(),
                    });
                }
            }
            TaskOutputKind::Diagnostic {
                diagnostic,
                terminal,
            } => {
                let has_terminal = page.events.iter().any(|event| {
                    matches!(
                        event.kind,
                        TaskOutputKind::Diagnostic { terminal: true, .. }
                    )
                });
                if *terminal || !has_terminal {
                    items.push(Item::Error(
                        crate::task_console::diagnostic_lines(diagnostic).join("\n"),
                    ));
                }
            }
            TaskOutputKind::Status(_) => {}
        }
    }
    if let Some(key) = &state.task_console.selected {
        for (id, index) in &tool_indices {
            if let Item::Tool { view, .. } = &mut items[*index] {
                view.expanded = state
                    .task_console
                    .expanded_tools
                    .contains(&(key.clone(), id.clone()));
                view.focused = state.task_console.tool_focus.as_ref() == Some(id);
            }
        }
    }
    crate::app::tool_groups::group_items(&mut items, false);
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    state.task_console.rendered_tools.clear();
    for (index, item) in items.iter().enumerate() {
        let rendered = crate::render::render_transcript_item(
            item,
            crate::render::item_neighbors(&items, index),
            width,
            false,
            state.styles(),
        );
        if !rendered.is_empty()
            && let Item::Tool {
                call_id: Some(id),
                view,
                ..
            } = item
            && view.group_parent.is_none()
        {
            hits.push((lines.len(), id.as_str().to_owned()));
            state
                .task_console
                .rendered_tools
                .push(id.as_str().to_owned());
        }
        lines.extend(rendered);
    }
    if lines.is_empty() {
        lines.push(Line::raw("Waiting for conversation output…"));
    }
    (lines, hits)
}

fn wrap(lines: &[String], width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in lines {
        let mut row = String::new();
        let mut columns = 0;
        for ch in line.chars() {
            let count = ch.width().unwrap_or(0);
            if columns + count > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                columns = 0;
            }
            row.push(ch);
            columns += count;
        }
        rows.push(row);
    }
    rows
}
