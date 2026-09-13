//! Workflow presentation: named progress rail, phase workspace, agent summary.

use crate::{app::AppState, task_console::safe, terminal::Styles, workflow_console::*};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn tool_title(args: &serde_json::Value) -> String {
    let title = args
        .pointer("/definition/title")
        .or_else(|| args.pointer("/definition/name"))
        .or_else(|| args.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(
            || match args.get("action").and_then(serde_json::Value::as_str) {
                Some("pause") => "Pause workflow",
                Some("resume") => "Resume workflow",
                Some("cancel") => "Cancel workflow",
                Some("list") => "List workflows",
                Some("saved") => "Saved workflows",
                _ => "Workflow",
            },
        );
    safe(title)
}

/// Tool success acknowledges the request. A dispatched job does not prove that
/// the workflow itself has finished; that outcome belongs to the live rail.
pub(crate) fn tool_receipt(
    args: &serde_json::Value,
    result: Option<&(bool, serde_json::Value)>,
) -> String {
    let Some((ok, value)) = result else {
        return "…".to_owned();
    };
    if !ok {
        return "Failed".to_owned();
    }
    let action = args.get("action").and_then(serde_json::Value::as_str);
    let dispatched = value
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && value
            .get("job_id")
            .and_then(serde_json::Value::as_str)
            .is_some();
    match action {
        Some("start" | "run_saved") if dispatched => "Started in background".to_owned(),
        Some("resume") if dispatched => "Resumed in background".to_owned(),
        Some("save")
            if value
                .get("saved")
                .and_then(serde_json::Value::as_str)
                .is_some() =>
        {
            "Saved workflow".to_owned()
        }
        Some("pause")
            if value
                .get("pause_requested")
                .and_then(serde_json::Value::as_bool)
                == Some(true) =>
        {
            "Pause requested".to_owned()
        }
        Some("cancel") => match value
            .get("cancel_requested")
            .and_then(serde_json::Value::as_bool)
        {
            Some(true) => "Cancellation requested".to_owned(),
            Some(false) => "No cancellation requested".to_owned(),
            None => "Request completed".to_owned(),
        },
        Some("list" | "saved") if value.is_array() => {
            let count = value.as_array().map_or(0, Vec::len);
            format!(
                "Found {count} workflow{}",
                if count == 1 { "" } else { "s" }
            )
        }
        _ => "Request completed".to_owned(),
    }
}

fn tone(styles: &Styles, state: WorkflowStatus) -> Color {
    match state {
        WorkflowStatus::Completed => styles.success(),
        WorkflowStatus::Failed | WorkflowStatus::Interrupted => styles.error(),
        WorkflowStatus::Waiting | WorkflowStatus::Paused | WorkflowStatus::Pausing => styles.warn(),
        WorkflowStatus::Running => styles.accent(),
        _ => styles.dim(),
    }
}
fn line(frame: &mut Frame<'_>, text: impl Into<String>, style: Style, area: Rect) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(Paragraph::new(text.into()).style(style), area);
    }
}
fn at(area: Rect, y: u16, height: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(y).min(area.bottom()),
        area.width,
        height.min(area.height.saturating_sub(y)),
    )
}
fn inset(area: Rect, x: u16, y: u16) -> Rect {
    Rect::new(
        area.x.saturating_add(x),
        area.y.saturating_add(y),
        area.width.saturating_sub(x * 2),
        area.height.saturating_sub(y * 2),
    )
}
fn clipped(text: &str, width: usize) -> String {
    let text = safe(text);
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut cols = 0;
    for c in text.chars() {
        let count = c.width().unwrap_or(0);
        if cols + count >= width {
            break;
        }
        out.push(c);
        cols += count;
    }
    out.push('…');
    out
}
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds / 60) % 60)
    }
}
fn tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}
fn stats(stats: &WorkflowStats) -> String {
    let mut parts = Vec::new();
    if let Some(elapsed) = stats.elapsed_ms {
        parts.push(duration(elapsed));
    }
    if let (Some(input), Some(output)) = (stats.input_tokens, stats.output_tokens) {
        parts.push(format!(
            "{} tokens{}",
            tokens(input.saturating_add(output)),
            if stats.partial_usage { " reported" } else { "" }
        ));
    }
    parts.join("  ·  ")
}
fn progress(done: usize, total: usize, width: usize) -> String {
    let filled = done.saturating_mul(width).checked_div(total).unwrap_or(0);
    format!(
        "{}{}",
        "━".repeat(filled),
        "─".repeat(width.saturating_sub(filled))
    )
}

pub(crate) fn rail_height(state: &AppState, height: u16) -> u16 {
    if !state.workflow_console.expanded() && !state.workflow_console.runs.is_empty() && height >= 8
    {
        2
    } else {
        0
    }
}

pub(crate) fn draw_rail(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    if area.height == 0 {
        return;
    }
    let Some(run) = state.workflow_console.selected().cloned() else {
        return;
    };
    let styles = state.styles();
    let (done, total) = run.agent_progress();
    let (phase_done, phase_total) = run.progress();
    let glyph = if run.status.active() {
        state.spinner_glyph()
    } else {
        run.status.glyph()
    };
    let title_width = usize::from(
        area.width
            .saturating_sub(if area.width >= 80 { 47 } else { 7 }),
    );
    let title = clipped(&run.title, title_width);
    let end = if area.width >= 80 {
        format!(
            "  {done}/{total} agents  {}  ↓ Enter open",
            progress(done, total, 9)
        )
    } else {
        String::new()
    };
    line(
        frame,
        format!(
            "{} {glyph} {title}{end}",
            if state.workflow_console.rail_focused {
                "›"
            } else {
                " "
            }
        ),
        Style::default().fg(tone(&styles, run.status)).bold(),
        at(area, 0, 1),
    );
    let phase = run.current_phase().map_or("", |p| p.title.as_str());
    let text = if area.width >= 80 {
        format!(
            "    {}  ·  {}  ·  {phase_done}/{phase_total} phases{}",
            run.status.label(),
            phase,
            run.stats
                .elapsed_ms
                .map_or_else(String::new, |ms| format!("  ·  {}", duration(ms)))
        )
    } else {
        format!(
            "    {done}/{total} agents · {} · ↓ Enter",
            run.status.label()
        )
    };
    line(
        frame,
        clipped(&text, usize::from(area.width)),
        Style::default().fg(styles.dim()),
        at(area, 1, 1),
    );
    state
        .workflow_console
        .hits
        .push((area, WorkflowHit::Toggle));
}

pub(crate) fn draw_workspace(frame: &mut Frame<'_>, state: &mut AppState, area: Rect) {
    let styles = state.styles();
    state.workflow_console.body = area;
    if area.width < 8 || area.height < 4 {
        return;
    }
    let body = inset(area, if area.width >= 60 { 2 } else { 1 }, 0);
    if state.workflow_console.runs.is_empty() {
        // The reference presents an empty workflow inventory as one bottom
        // panel, not a full-height workspace.
        let mut rows = vec![
            crate::panel_frame::note("No workflows in this session.", styles),
            crate::panel_frame::note(
                "Started workflows appear here with their phases and agents.",
                styles,
            ),
        ];
        // The voice caption and a workflow notice are live state; an empty
        // inventory must not be a place where they stop being shown.
        for warning in [
            state.voice.caption().map(str::to_owned),
            state.workflow_console.notice.clone(),
        ]
        .into_iter()
        .flatten()
        {
            rows.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                format!("{}{warning}", crate::panel_frame::INDENT),
                Style::default().fg(styles.warn()).bold(),
            )));
        }
        let mut lines = vec![
            ratatui::text::Line::styled(
                format!("{}Workflows", crate::panel_frame::INDENT),
                Style::default()
                    .fg(styles.panel_title())
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            crate::panel_frame::blank(),
        ];
        lines.extend(rows);
        lines.push(crate::panel_frame::blank());
        lines.push(crate::panel_frame::hint("Esc to close", styles));
        crate::command_panel_frame::render_anchored(frame, area, styles, lines);
        return;
    }
    match state.workflow_console.view {
        WorkflowView::Picker => draw_picker(frame, state, body, &styles),
        WorkflowView::Agent => draw_agent_detail(frame, state, body, &styles),
        _ => draw_phases(frame, state, body, &styles),
    }
}

fn draw_phases(frame: &mut Frame<'_>, state: &mut AppState, area: Rect, styles: &Styles) {
    let Some(run) = state.workflow_console.selected().cloned() else {
        return;
    };
    let (done, total) = run.progress();
    let short = area.width < 70;
    line(
        frame,
        clipped(&run.title, usize::from(area.width)),
        Style::default().fg(styles.text()).bold(),
        at(area, 0, 1),
    );
    let title_width = area.width.saturating_sub(if short { 0 } else { 18 });
    line(
        frame,
        clipped(&run.description, usize::from(title_width)),
        Style::default().fg(styles.dim()),
        Rect::new(area.x, area.y + 1, title_width, 1),
    );
    if !short {
        line(
            frame,
            format!("{} {}", run.status.glyph(), run.status.label()),
            Style::default().fg(tone(styles, run.status)),
            Rect::new(area.right().saturating_sub(17), area.y + 1, 17, 1),
        );
    }
    let subtitle = if short {
        format!(
            "{} · {done}/{total} phases · {} agents",
            run.status.label(),
            run.agent_count()
        )
    } else {
        format!(
            "{}  {} / {} phases  ·  {} agents{}",
            progress(done, total, 18),
            done,
            total,
            run.agent_count(),
            run.stats
                .elapsed_ms
                .map_or_else(String::new, |ms| format!("  ·  {}", duration(ms)))
        )
    };
    line(
        frame,
        clipped(&subtitle, usize::from(area.width)),
        Style::default().fg(styles.dim()),
        at(area, 2, 1),
    );
    let footer_rows = footer_height(state, area.width, false);
    let content = Rect::new(
        area.x,
        area.y + 4,
        area.width,
        area.height.saturating_sub(4 + footer_rows),
    );
    if area.width >= 96 {
        let border = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(styles.border()));
        let inner = border.inner(content);
        frame.render_widget(border, content);
        let left_width = (inner.width * 29 / 100).min(36);
        let left = Rect::new(
            inner.x + 1,
            inner.y + 1,
            left_width.saturating_sub(1),
            inner.height.saturating_sub(2),
        );
        let right = Rect::new(
            inner.x + left_width + 2,
            inner.y + 1,
            inner.width.saturating_sub(left_width + 3),
            inner.height.saturating_sub(2),
        );
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(styles.border())),
            Rect::new(inner.x + left_width, inner.y, 1, inner.height),
        );
        draw_timeline(frame, state, left, styles, &run);
        draw_agents(frame, state, right, styles);
    } else {
        draw_compact_phases(frame, state, at(content, 0, 3), styles, &run);
        draw_agents(
            frame,
            state,
            Rect::new(
                content.x,
                content.y + 3,
                content.width,
                content.height.saturating_sub(3),
            ),
            styles,
        );
    }
    let buttons = workspace_buttons(&run, state.workflow_console.runs.len());
    draw_footer(frame, state, area, &buttons);
}

fn draw_timeline(
    frame: &mut Frame<'_>,
    state: &mut AppState,
    area: Rect,
    styles: &Styles,
    run: &WorkflowRun,
) {
    line(
        frame,
        "PHASES",
        Style::default().fg(if state.workflow_console.focus == WorkflowFocus::Phases {
            styles.accent()
        } else {
            styles.dim()
        }),
        at(area, 0, 1),
    );
    let selected = state.workflow_console.phase().map(|p| p.id.clone());
    let selected_index = run
        .phases
        .iter()
        .position(|p| Some(&p.id) == selected.as_ref())
        .unwrap_or(0);
    let row_height = 3;
    let count = usize::from(area.height.saturating_sub(2) / row_height).max(1);
    let start = selected_index.saturating_sub(count.saturating_sub(1));
    for (index, phase) in run.phases.iter().enumerate().skip(start).take(count) {
        let y = 2 + u16::try_from(index - start).unwrap_or(0) * row_height;
        let target = at(area, y, row_height);
        let active = Some(&phase.id) == selected.as_ref();
        let style = Style::default().fg(if active {
            styles.accent()
        } else {
            tone(styles, phase.status)
        });
        let (done, total) = phase.progress();
        line(
            frame,
            format!(
                "{}  {} {}",
                if active { "›" } else { " " },
                phase.status.glyph(),
                clipped(&phase.title, usize::from(area.width.saturating_sub(6)))
            ),
            if active { style.bold() } else { style },
            at(target, 0, 1),
        );
        line(
            frame,
            format!("     {} · {done}/{total} steps", phase.status.label()),
            Style::default().fg(styles.dim()),
            at(target, 1, 1),
        );
        if row_height >= 4 {
            line(
                frame,
                format!("     {}", stats(&phase.stats)),
                Style::default().fg(styles.dim()),
                at(target, 2, 1),
            );
        }
        state
            .workflow_console
            .hits
            .push((target, WorkflowHit::Phase(phase.id.clone())));
    }
}

fn draw_compact_phases(
    frame: &mut Frame<'_>,
    state: &mut AppState,
    area: Rect,
    styles: &Styles,
    run: &WorkflowRun,
) {
    let current = state.workflow_console.phase().map(|p| p.id.clone());
    let index = run
        .phases
        .iter()
        .position(|p| Some(&p.id) == current.as_ref())
        .unwrap_or(0);
    if area.width >= 64 {
        let width = area.width / u16::try_from(run.phases.len().clamp(1, 4)).unwrap_or(1);
        let start = index.saturating_sub(3);
        for (offset, phase) in run.phases.iter().skip(start).take(4).enumerate() {
            let target = Rect::new(
                area.x + u16::try_from(offset).unwrap_or(0) * width,
                area.y,
                width,
                area.height,
            );
            let selected = Some(&phase.id) == current.as_ref();
            line(
                frame,
                clipped(
                    &format!("{} {}", phase.status.glyph(), phase.title),
                    usize::from(width.saturating_sub(1)),
                ),
                Style::default()
                    .fg(if selected {
                        styles.accent()
                    } else {
                        styles.dim()
                    })
                    .bold(),
                at(target, 0, 1),
            );
            let (done, total) = phase.progress();
            line(
                frame,
                format!("  {} · {done}/{total}", phase.status.label()),
                Style::default().fg(styles.dim()),
                at(target, 1, 1),
            );
            state
                .workflow_console
                .hits
                .push((target, WorkflowHit::Phase(phase.id.clone())));
        }
    } else if let Some(phase) = state.workflow_console.phase().cloned() {
        line(
            frame,
            format!("PHASE {} OF {}", index + 1, run.phases.len()),
            Style::default().fg(styles.dim()),
            at(area, 0, 1),
        );
        let text =
            format!(
                "‹ {} · {}",
                clipped(
                    &phase.title,
                    usize::from(area.width.saturating_sub(
                        u16::try_from(phase.status.label().len() + 7).unwrap_or(18)
                    ))
                ),
                phase.status.label()
            );
        line(
            frame,
            text,
            Style::default().fg(styles.accent()).bold(),
            at(area, 1, 1),
        );
        line(
            frame,
            "›",
            Style::default().fg(styles.accent()).bold(),
            Rect::new(area.right().saturating_sub(2), area.y + 1, 2, 1),
        );
        if index > 0 {
            state.workflow_console.hits.push((
                Rect::new(area.x, area.y + 1, 3, 1),
                WorkflowHit::Phase(run.phases[index - 1].id.clone()),
            ));
        }
        if index + 1 < run.phases.len() {
            state.workflow_console.hits.push((
                Rect::new(area.right().saturating_sub(4), area.y + 1, 4, 1),
                WorkflowHit::Phase(run.phases[index + 1].id.clone()),
            ));
        }
    }
}

fn draw_agents(frame: &mut Frame<'_>, state: &mut AppState, area: Rect, styles: &Styles) {
    let Some(phase) = state.workflow_console.phase().cloned() else {
        return;
    };
    let selected = state.workflow_console.agent().map(|a| a.task_id.clone());
    let title = if phase.agents.is_empty() {
        format!("{} · steps", phase.title)
    } else {
        format!("{} · {} agents", phase.title, phase.agents.len())
    };
    line(
        frame,
        clipped(&title, usize::from(area.width)),
        Style::default().fg(styles.text()).bold(),
        at(area, 0, 1),
    );
    let card_height = 3;
    let count = usize::from(area.height.saturating_sub(2) / card_height).max(1);
    let index = phase
        .agents
        .iter()
        .position(|a| Some(&a.task_id) == selected.as_ref())
        .unwrap_or(0);
    let start = index.saturating_sub(count.saturating_sub(1));
    for (offset, agent) in phase.agents.iter().skip(start).take(count).enumerate() {
        let target = at(
            area,
            2 + u16::try_from(offset).unwrap_or(0) * card_height,
            card_height,
        );
        let active = Some(&agent.task_id) == selected.as_ref()
            && state.workflow_console.focus == WorkflowFocus::Agents;
        let inner = target;
        let status_width = u16::try_from(agent.status.label().len())
            .unwrap_or(10)
            .max(10)
            .min(inner.width);
        let label_width = inner.width.saturating_sub(status_width);
        line(
            frame,
            clipped(
                &format!(
                    "{} {} {}",
                    if active { "›" } else { " " },
                    agent.status.glyph(),
                    agent.label
                ),
                usize::from(label_width),
            ),
            Style::default()
                .fg(if active {
                    styles.accent()
                } else {
                    styles.text()
                })
                .bold(),
            Rect::new(inner.x, inner.y, label_width, 1),
        );
        line(
            frame,
            agent.status.label(),
            Style::default().fg(tone(styles, agent.status)),
            Rect::new(
                inner.right().saturating_sub(status_width),
                inner.y,
                status_width,
                1,
            ),
        );
        let activity = agent
            .activity
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| match agent.status {
                WorkflowStatus::Running => "Working on this step",
                WorkflowStatus::Completed => "Result ready to review",
                WorkflowStatus::Waiting => "Waiting for approval or input",
                WorkflowStatus::Queued => "Waiting to start",
                _ => agent.status.label(),
            });
        let metrics = stats(&agent.stats);
        let secondary = if metrics.is_empty() {
            activity.to_owned()
        } else if inner.width >= 60 {
            format!(
                "{}  ·  {metrics}",
                clipped(
                    activity,
                    usize::from(
                        inner
                            .width
                            .saturating_sub(u16::try_from(metrics.len() + 5).unwrap_or(0))
                    )
                )
            )
        } else {
            metrics
        };
        line(
            frame,
            clipped(&format!("    {secondary}"), usize::from(inner.width)),
            Style::default().fg(styles.dim()),
            at(inner, 1, 1),
        );
        state
            .workflow_console
            .hits
            .push((target, WorkflowHit::Agent(agent.task_id.clone())));
    }
    if phase.agents.is_empty() {
        for (index, node) in phase
            .nodes
            .iter()
            .take(usize::from(area.height.saturating_sub(2) / 3))
            .enumerate()
        {
            let target = at(area, 2 + u16::try_from(index).unwrap_or(0) * 3, 3);
            line(
                frame,
                clipped(
                    &format!("{}  {}", node.status.glyph(), node.label),
                    usize::from(area.width),
                ),
                Style::default().fg(tone(styles, node.status)).bold(),
                at(target, 0, 1),
            );
            let detail = node.result.as_deref().unwrap_or_else(|| {
                if node.kind == "agent" {
                    "Agent will appear when this step starts"
                } else {
                    node.status.label()
                }
            });
            line(
                frame,
                clipped(detail, usize::from(area.width)),
                Style::default().fg(styles.dim()),
                at(target, 1, 1),
            );
        }
    } else if phase.agents.len() > count {
        let hint = format!(
            "{}–{} of {} · ↑↓ select",
            start + 1,
            (start + count).min(phase.agents.len()),
            phase.agents.len()
        );
        line(
            frame,
            hint,
            Style::default().fg(styles.dim()),
            at(area, area.height.saturating_sub(1), 1),
        );
    }
}

fn draw_agent_detail(frame: &mut Frame<'_>, state: &mut AppState, area: Rect, styles: &Styles) {
    let Some(agent) = state.workflow_console.agent().cloned() else {
        state.workflow_console.view = WorkflowView::Workspace;
        return;
    };
    let phase = state
        .workflow_console
        .phase()
        .map_or("Phase", |p| p.title.as_str());
    let back = format!("‹  {phase}");
    line(
        frame,
        clipped(&back, usize::from(area.width)),
        Style::default().fg(styles.accent()),
        at(area, 0, 1),
    );
    state
        .workflow_console
        .hits
        .push((at(area, 0, 1), WorkflowHit::Back));
    line(
        frame,
        clipped(&agent.label, usize::from(area.width)),
        Style::default().fg(styles.text()).bold(),
        at(area, 2, 1),
    );
    line(
        frame,
        format!(
            "{} {}{}",
            agent.status.glyph(),
            agent.status.label(),
            if stats(&agent.stats).is_empty() {
                String::new()
            } else {
                format!("  ·  {}", stats(&agent.stats))
            }
        ),
        Style::default().fg(tone(styles, agent.status)),
        at(area, 3, 1),
    );
    let mut lines = vec![("ASSIGNMENT".to_owned(), true)];
    lines.extend(
        wrap(&agent.assignment, usize::from(area.width))
            .into_iter()
            .map(|line| (line, false)),
    );
    lines.push((String::new(), false));
    lines.push((
        (if agent.status.finished() {
            "RESULT"
        } else {
            "LATEST UPDATE"
        })
        .into(),
        true,
    ));
    let summary = if agent.summary.trim().is_empty() {
        "Waiting for the first update."
    } else {
        &agent.summary
    };
    lines.extend(
        wrap(summary, usize::from(area.width))
            .into_iter()
            .map(|line| (line, false)),
    );
    if agent.output_truncated {
        lines.push((
            "Open the conversation to read more retained output.".into(),
            false,
        ));
    }
    let content = Rect::new(
        area.x,
        area.y + 5,
        area.width,
        area.height.saturating_sub(7),
    );
    state.workflow_console.detail_height = usize::from(content.height);
    state.workflow_console.detail_rows = lines.len();
    let scroll = state
        .workflow_console
        .scroll()
        .min(lines.len().saturating_sub(usize::from(content.height)));
    for (index, (text, heading)) in lines
        .into_iter()
        .skip(scroll)
        .take(usize::from(content.height))
        .enumerate()
    {
        line(
            frame,
            text,
            if heading {
                Style::default().fg(styles.dim()).bold()
            } else {
                Style::default().fg(styles.text())
            },
            at(content, u16::try_from(index).unwrap_or(0), 1),
        );
    }
    draw_footer(
        frame,
        state,
        area,
        &[
            ("C Conversation", WorkflowHit::Conversation),
            ("Esc Back", WorkflowHit::Back),
        ],
    );
}

fn draw_picker(frame: &mut Frame<'_>, state: &mut AppState, area: Rect, styles: &Styles) {
    line(
        frame,
        "WORKFLOWS",
        Style::default().fg(styles.dim()),
        at(area, 0, 1),
    );
    line(
        frame,
        "Choose a workflow",
        Style::default().fg(styles.text()).bold(),
        at(area, 2, 1),
    );
    let count = usize::from(area.height.saturating_sub(6) / 3).max(1);
    let selected = state
        .workflow_console
        .runs
        .iter()
        .position(|r| Some(&r.id) == state.workflow_console.selected_run.as_ref())
        .unwrap_or(0);
    let start = selected.saturating_sub(count.saturating_sub(1));
    let rows = state
        .workflow_console
        .runs
        .iter()
        .skip(start)
        .take(count)
        .cloned()
        .collect::<Vec<_>>();
    for (index, run) in rows.into_iter().enumerate() {
        let target = at(area, 4 + u16::try_from(index).unwrap_or(0) * 3, 3);
        let (done, total) = run.progress();
        let active = Some(&run.id) == state.workflow_console.selected_run.as_ref();
        line(
            frame,
            clipped(
                &format!(
                    "{}  {}",
                    if active { "›" } else { run.status.glyph() },
                    run.title
                ),
                usize::from(area.width),
            ),
            Style::default()
                .fg(if active {
                    styles.accent()
                } else {
                    styles.text()
                })
                .bold(),
            at(target, 0, 1),
        );
        line(
            frame,
            format!(
                "   {} · {done}/{total} phases · {} agents",
                run.status.label(),
                run.agent_count()
            ),
            Style::default().fg(styles.dim()),
            at(target, 1, 1),
        );
        state
            .workflow_console
            .hits
            .push((target, WorkflowHit::PickRun(run.id)));
    }
    draw_footer(
        frame,
        state,
        area,
        &[
            ("Enter Open", WorkflowHit::Toggle),
            ("Esc Back", WorkflowHit::Back),
        ],
    );
}

fn workspace_buttons(run: &WorkflowRun, count: usize) -> Vec<(&'static str, WorkflowHit)> {
    let mut out = Vec::new();
    if run.controls.pause {
        out.push(("P Pause", WorkflowHit::Action(WorkflowActionKind::Pause)));
    }
    if run.controls.resume {
        out.push(("R Resume", WorkflowHit::Action(WorkflowActionKind::Resume)));
    }
    if run.controls.stop {
        out.push(("X Stop", WorkflowHit::Action(WorkflowActionKind::Stop)));
    }
    if count > 1 {
        out.push(("W Workflows", WorkflowHit::Picker));
    }
    out.push(("Esc Close", WorkflowHit::Back));
    out
}
fn footer_height(state: &AppState, width: u16, agent: bool) -> u16 {
    let buttons = if agent {
        vec![
            ("C Conversation", WorkflowHit::Conversation),
            ("Esc Back", WorkflowHit::Back),
        ]
    } else {
        state
            .workflow_console
            .selected()
            .map_or_else(Vec::new, |run| {
                workspace_buttons(run, state.workflow_console.runs.len())
            })
    };
    let mut x = 0;
    let mut rows = 1;
    for (label, _) in buttons {
        let size = u16::try_from(label.len() + 3)
            .unwrap_or(u16::MAX)
            .min(width);
        if x > 0 && x + size > width {
            rows += 1;
            x = 0;
        }
        x += size;
    }
    rows + 1 + u16::from(state.voice.caption().is_some())
}
fn draw_footer(
    frame: &mut Frame<'_>,
    state: &mut AppState,
    area: Rect,
    buttons: &[(&'static str, WorkflowHit)],
) {
    let styles = state.styles();
    let mut placed = Vec::new();
    let mut x = area.x;
    let mut rows = 1;
    for (label, hit) in buttons {
        let width = u16::try_from(label.len() + 3)
            .unwrap_or(u16::MAX)
            .min(area.width);
        if x > area.x && x + width > area.right() {
            rows += 1;
            x = area.x;
        }
        placed.push((*label, hit.clone(), x, rows - 1, width));
        x += width;
    }
    let y = area.bottom().saturating_sub(rows);
    if let Some(caption) = state.voice.caption() {
        line(
            frame,
            clipped(caption, usize::from(area.width)),
            Style::default().fg(styles.warn()).bold(),
            Rect::new(area.x, y.saturating_sub(2), area.width, 1),
        );
    }
    if let Some(notice) = &state.workflow_console.notice {
        line(
            frame,
            clipped(notice, usize::from(area.width)),
            Style::default().fg(styles.warn()),
            Rect::new(area.x, y.saturating_sub(1), area.width, 1),
        );
    } else if area.width >= 70 && state.workflow_console.view == WorkflowView::Workspace {
        line(
            frame,
            "Tab switch pane  ·  ↑↓ select  ·  Enter inspect",
            Style::default().fg(styles.dim()),
            Rect::new(area.x, y.saturating_sub(1), area.width, 1),
        );
    }
    for (label, hit, x, row, width) in placed {
        let target = Rect::new(x, y + row, width, 1);
        line(
            frame,
            format!("[{label}] "),
            Style::default().fg(styles.accent()),
            target,
        );
        state.workflow_console.hits.push((target, hit));
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let paragraph = safe(paragraph);
        let mut row = String::new();
        for word in paragraph.split_whitespace() {
            if !row.is_empty() && row.width() + 1 + word.width() > width {
                lines.push(std::mem::take(&mut row));
            }
            if !row.is_empty() {
                row.push(' ');
            }
            if word.width() > width {
                for c in word.chars() {
                    if row.width() + c.width().unwrap_or(0) > width {
                        lines.push(std::mem::take(&mut row));
                    }
                    row.push(c);
                }
            } else {
                row.push_str(word);
            }
        }
        lines.push(row);
    }
    lines
}
