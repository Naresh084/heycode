//! Product-neutral flat rendering over the live application state.
//!
//! This is a second renderer, not a second application: key routing, modal
//! priority, command discovery, panel state, and lifecycle outcomes remain
//! owned by [`super::AppState`]. The projection only names the state already
//! visible through that one reducer in a linear, color-independent form.

use std::io::Write;

use heycode_core::UntrustedContentSource;

use super::{
    AppState, Item, ModelPickerLoadState, PendingRuntimeQuestionView, WelcomeHealth,
    WorkspaceTrustView,
};

const MAX_FLAT_LINES: usize = 512;
const MAX_FLAT_LINE_CHARS: usize = 2_048;
const MAX_TRANSCRIPT_ITEMS: usize = 100;

/// One bounded, plain-text screen-reader projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReaderSnapshot {
    text: String,
}

impl ScreenReaderSnapshot {
    /// Project the live state using the same modal priority as the visual UI.
    #[must_use]
    pub fn from_state(state: &AppState) -> Self {
        let mut builder = FlatBuilder::default();
        if state.workspace_trust_is_foreground()
            && let Some(view) = state.workspace_trust.as_ref()
        {
            render_workspace_trust(&mut builder, view);
            return builder.finish();
        }
        if let Some(secret) = state.pending_secret.as_ref() {
            builder.section("Secret input");
            builder.line(format!("prompt: {}", secret.prompt));
            builder.line(format!("reference: {}", secret.reference));
            if let Some(error) = &secret.error {
                builder.line(error);
            }
            builder.line(format!(
                "input: masked ({} characters)",
                secret.secret.chars().count()
            ));
            builder.line("keys: Type enters the secret; Enter submits; Escape cancels.");
            return builder.finish();
        }
        if let Some(onboarding) = state
            .onboarding
            .as_ref()
            .filter(|onboarding| onboarding.active)
        {
            builder.section("Setup");
            builder.line(onboarding.title);
            builder.line(onboarding.body);
            if let Some(input) = onboarding.input.as_ref() {
                builder.line(format!(
                    "field {}/{}: {} — {}",
                    input.position,
                    input.total,
                    input.label,
                    trim_sentence(&input.description)
                ));
                builder.line(format!("value: {}", input.value));
            }
            if let Some(notice) = state.onboarding_notice.as_deref() {
                builder.line(format!("notice: {notice}"));
            }
            for (index, option) in onboarding.options.iter().enumerate() {
                builder.line(format!(
                    "{} {} — {}.",
                    selected(index == onboarding.selected),
                    option.label,
                    trim_sentence(&option.description)
                ));
            }
            builder.line(
                "keys: Up or Down chooses; Tab moves forward; Enter confirms; Escape cancels; Control+C twice exits.",
            );
            return builder.finish();
        }
        if let Some(view) = state.pending_plan_review.as_ref() {
            for line in view.accessible_lines() {
                builder.line(line);
            }
            return builder.finish();
        }

        if state.optional_question_panel_visible() {
            builder.section("Optional question");
            for line in super::optional_questions::panel_lines(state, 110, 30) {
                builder.line(
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>(),
                );
            }
            builder.line("Closing preserves this question. Dismiss question sends no answer.");
            return builder.finish();
        }
        if !state.optional_questions.rows.is_empty() {
            builder.line(format!(
                "{} optional questions pending. Alt+Q opens them without changing your draft.",
                state.optional_questions.rows.len()
            ));
        }
        if state.workflow_console.expanded()
            && !state.high_priority_modal_open()
            && state.pending_mcp_elicitation.is_none()
        {
            for line in state.workflow_console.accessible_lines() {
                builder.line(line);
            }
            if let Some(caption) = state.voice.caption() {
                builder.line(caption);
            }
            return builder.finish();
        }
        render_base(&mut builder, state);
        if let Some(run) = state.workflow_console.selected() {
            let (done, total) = run.progress();
            builder.line(format!(
                "Workflow: {} · {} · {done}/{total} phases. {} opens the workspace.",
                run.title,
                run.status.label(),
                state.workflow_toggle_label()
            ));
        }

        if let Some(elicitation) = state.pending_mcp_elicitation.as_ref() {
            builder.section("MCP elicitation");
            builder.line(format!("server: {}", elicitation.server));
            builder.line(format!("message: {}", elicitation.message));
            match &elicitation.mode {
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
                    builder.line(format!("fields: {fields}"));
                    builder.line(format!("JSON: {input}"));
                    if let Some(error) = error {
                        builder.line(format!("error: {error}"));
                    }
                    builder.line("keys: type one JSON object; Enter accepts; Escape declines.");
                }
                crate::app::PendingMcpElicitationMode::Url { url } => {
                    builder.line(format!("URL: {url}"));
                    builder.line("keys: Enter acknowledges completion; Escape declines.");
                }
            }
        } else if let Some(question) = state.pending_runtime_question.as_ref() {
            render_runtime_question(&mut builder, question);
        } else if let Some(ask) = state.pending_ask.as_ref() {
            builder.section("Permission requested");
            if let Some(preview) = &ask.edit_preview {
                builder.line("tool: Edit file");
                builder.line(format!("path: {}", preview.path()));
                for row in preview.rows() {
                    match row.kind() {
                        crate::approval_preview::EditApprovalRowKind::Context => {
                            builder.line(format!(
                                "  line {} unchanged: {}",
                                row.old_line().unwrap_or_default(),
                                row.text()
                            ))
                        }
                        crate::approval_preview::EditApprovalRowKind::Removed => {
                            builder.line(format!(
                                "  old line {} removed: {}",
                                row.old_line().unwrap_or_default(),
                                row.text()
                            ))
                        }
                        crate::approval_preview::EditApprovalRowKind::Added => {
                            builder.line(format!(
                                "  new line {} added: {}",
                                row.new_line().unwrap_or_default(),
                                row.text()
                            ))
                        }
                    }
                }
            } else {
                builder.line(format!("tool: {}", ask.name));
                for row in ask.args_preview.lines() {
                    builder.line(format!("  {row}"));
                }
            }
            let waiting = state.queued_ask_count();
            if waiting > 0 {
                builder.line(format!("{waiting} more permission requests waiting"));
            }
            if let Some(reason) = ask.reason() {
                builder.line(format!(
                    "reason: {}",
                    if reason.is_empty() { "empty" } else { reason }
                ));
                builder.line(
                    "keys: type the reason; Enter denies with it; Escape returns to the choices.",
                );
            } else {
                let choices = state.approval_choices();
                let remember = choices.len() == crate::app::ASK_CHOICES.len();
                for (index, label) in choices.iter().enumerate() {
                    builder.line(format!("{} {label}", selected(ask.selection == index)));
                }
                builder.line(
                    if state.approval_grants_edits() { "Allow edits for this session; commands still need approval. Up or Down chooses; Enter confirms; y accepts once; a accepts and allows edits; n or Escape rejects." } else if remember { "Future approvals apply to the same tool and inputs only. Up or Down chooses; Enter confirms; y accepts; a accepts future; n or Escape rejects." } else { "keys: Up or Down chooses; Enter confirms; y accepts; n or Escape rejects." },
                );
            }
        } else if let Some(confirm) = state.pending_command_confirmation.as_ref() {
            builder.section("Command confirmation");
            builder.line(format!("command: {}", confirm.synopsis));
            builder.line(format!("{} Cancel", selected(confirm.selection == 0)));
            builder.line(format!(
                "{} Interrupt and run",
                selected(confirm.selection == 1)
            ));
            builder.line("keys: Left or Right chooses; Enter confirms; Escape cancels.");
        } else if let Some(dialog) = state.add_directory_dialog.as_ref() {
            for line in dialog.plain_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.copy_panel.as_ref() {
            for line in panel.plain_lines() {
                builder.line(line);
            }
        } else if let Some(picker) = state.rewind_picker.as_ref() {
            for line in picker.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.advisor_panel.as_ref() {
            builder.section("Advisor");
            for line in panel.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(picker) = state.theme_picker.as_ref() {
            builder.section("Theme");
            for (index, theme) in picker.themes.iter().enumerate() {
                builder.line(format!(
                    "{} {} — {}",
                    selected(index == picker.selected),
                    theme.title(),
                    theme.id().as_str()
                ));
            }
            builder.line(
                "keys: Up or Down previews; Tab moves forward; Enter persists; Escape restores.",
            );
        } else if let Some(picker) = state.scroll_speed_picker.as_ref() {
            builder.section("Scroll speed");
            builder.line(format!("selected: {}x", picker.speed()));
            builder.line(format!("preview offset: {} rows", picker.preview_offset()));
            builder.line(format!("preference revision: {}", picker.revision()));
            builder.line(
                "keys: Left or Down slows; Right, Up, or Tab speeds up; mouse wheel previews; r resets; Enter persists; Escape closes.",
            );
        } else if let Some(help) = state.help_panel.as_ref() {
            for line in help.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.skill_doctor_panel.as_ref() {
            for line in panel.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.memory_panel.as_ref() {
            for line in panel.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(picker) = state.keymap_picker.as_ref() {
            builder.section("Keymap");
            builder.line(format!("revision: {}", picker.revision));
            for (index, (action, chord)) in picker.rows.iter().enumerate() {
                builder.line(format!(
                    "{} {} — {}",
                    selected(index == picker.selected),
                    action.as_str(),
                    chord
                ));
            }
            builder
                .line("keys: Up or Down chooses; Tab moves forward; Enter edits; Escape closes.");
        } else if let Some(profile) = state.profile_picker.as_ref() {
            builder.section("Profile picker");
            for (index, row) in profile.rows.iter().enumerate() {
                builder.line(format!(
                    "{} {}{}",
                    selected(index == profile.selected),
                    row.label(),
                    if row.current { " (current)" } else { "" }
                ));
            }
            builder.line(
                "keys: Left or Right chooses; Up or Down also chooses; Enter selects; Escape closes.",
            );
        } else if let Some(browser) = state.session_browser.as_ref() {
            render_sessions(&mut builder, browser);
        } else if let Some(panel) = state.sandbox_panel.as_ref() {
            builder.section("Sandbox");
            for line in panel.lines(100, state.styles()) {
                builder.line(line.to_string());
            }
        } else if let Some(panel) = state.autocompact_panel.as_ref() {
            builder.section("Auto-compact window");
            for line in panel.lines(100, state.styles()) {
                builder.line(line.to_string());
            }
        } else if let Some(permission) = state.permission_picker.as_ref() {
            builder.section("Permissions");
            for (index, row) in permission.rows.iter().enumerate() {
                builder.line(format!(
                    "{} {}. {} {}{}",
                    selected(index == permission.selected),
                    row.label,
                    row.description,
                    if row.current { "Current. " } else { "" },
                    row.unavailable_reason.unwrap_or("")
                ));
            }
            builder.line("keys: Up or Down chooses; Enter selects; Escape closes.");
        } else if let Some(route) = state.route_picker.as_ref() {
            render_routes(&mut builder, route);
        } else if let Some(progress) = state.export_progress.as_ref() {
            builder.section("Conversation export progress");
            for line in progress.plain_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.export_panel.as_ref() {
            builder.section("Export conversation");
            for line in panel.plain_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.skills_panel.as_ref() {
            builder.section("Skills");
            for line in panel.accessible_lines() {
                builder.line(line);
            }
        } else if let Some(panel) = state.capability_catalog.as_ref() {
            builder.section(panel.title());
            builder.line(panel.summary());
            if panel.rows().is_empty() {
                builder.line("No contributions are registered.");
            } else {
                for (index, row) in panel.rows().iter().enumerate() {
                    builder.line(format!(
                        "{} {} — {}",
                        selected(index == panel.selected()),
                        row.name(),
                        row.detail()
                    ));
                }
            }
            builder.line(if panel.panel() == crate::panel_commands::CapabilityPanel::Agents { "keys: Up or Down chooses; R rechecks readiness; C cancels probes; Escape closes." } else { "keys: Up or Down chooses; Escape closes." });
        } else if let Some(settings) = state.settings_panel.as_ref() {
            builder.section("Settings");
            builder.line(format!("active tab: {}", settings.tab().label()));
            builder.line("tabs: Status, Config, Usage, Stats");
            if let Some(stats) = settings
                .stats_view()
                .filter(|_| settings.tab() == heycode_agent::ui::SettingsShellTab::Stats)
            {
                for line in stats.accessible_lines() {
                    builder.line(line);
                }
            } else if let Some(section) = settings.section() {
                builder.multiline(settings.tab().label(), section.plain_text());
            } else {
                builder.line(format!("search: {}", settings.query()));
                render_settings(&mut builder, settings.config());
            }
            builder.line(settings.footer());
        } else if let Some(plugins) = state.plugin_panel.as_ref() {
            builder.section("Plugins");
            let mut lines = plugins.lines();
            let _hint = lines.pop();
            for line in lines {
                builder.line(line.text);
            }
            builder.line(
                "keys: Up or Down moves; Tab changes section; Enter activates; Escape closes.",
            );
        } else if let Some(mcp) = state.mcp_panel.as_ref() {
            builder.section("MCP");
            let mut lines = mcp.lines();
            let _hint = lines.pop();
            for line in lines {
                builder.line(line.text);
            }
            builder.line(
                "keys: Up or Down moves between servers; Tab changes section; Enter opens actions; Escape closes.",
            );
        } else if let Some(effort) = state.effort_picker.as_ref() {
            builder.section("Reasoning effort");
            builder.line(format!("backend: {}", effort.owner().id()));
            for (index, choice) in effort.choices().iter().enumerate() {
                let current = if effort.current_effort() == Some(choice.as_str()) {
                    "; current"
                } else {
                    ""
                };
                let default = if effort.default_effort() == Some(choice.as_str()) {
                    "; default"
                } else {
                    ""
                };
                builder.line(format!(
                    "{} {}{}{}",
                    selected(index == effort.selected()),
                    choice,
                    current,
                    default
                ));
            }
            builder.line("keys: Up or Down chooses; Enter selects; Escape closes.");
        } else if let Some(model) = state.model_picker.as_ref() {
            render_models(&mut builder, model);
        } else if let Some(palette) = state.command_palette.as_ref() {
            builder.section("Command palette");
            builder.line(format!(
                "query: {}",
                if palette.query.is_empty() {
                    "empty"
                } else {
                    palette.query.as_str()
                }
            ));
            for (index, row) in palette.matches.iter().enumerate() {
                let descriptor = &row.entry.descriptor;
                let availability = if row.entry.availability.is_available() {
                    "available".to_owned()
                } else {
                    format!(
                        "unavailable: {}",
                        row.entry
                            .availability
                            .reason()
                            .unwrap_or("unknown prerequisite")
                    )
                };
                builder.line(format!(
                    "{} {} — {}; source: {}; timing: {}; {}.",
                    selected(index == palette.selected),
                    descriptor.synopsis(),
                    trim_sentence(descriptor.description()),
                    descriptor.source().plugin(),
                    descriptor.timing().as_str(),
                    availability
                ));
            }
            builder.line(
                "keys: Up or Down chooses; Enter runs, or completes a command that takes arguments; Escape closes.",
            );
        } else if state.task_console.view != crate::task_console::ConsoleView::Collapsed {
            builder.section("Task console");
            builder.line(state.task_console.summary());
            if state.task_console.view == crate::task_console::ConsoleView::List {
                for record in &state.task_console.records {
                    builder.line(format!(
                        "{} {} · {} · {}",
                        selected(Some(&record.key) == state.task_console.selected.as_ref()),
                        record.key.0,
                        record.status_label(),
                        record.label
                    ));
                }
                builder.line("keys: Up/Down select; Enter opens; Escape returns. Available controls depend on the selected record.");
            } else if let Some(record) = state.task_console.selected_record() {
                builder.line(format!(
                    "selected task: {} · {} · {}",
                    record.key.0,
                    record.label,
                    record.status_label()
                ));
                if record.kind == crate::task_console::TaskKind::Work {
                    for line in record.detail_lines() {
                        builder.line(line);
                    }
                    for line in state.task_console.output_lines() {
                        builder.line(line);
                    }
                    builder.line("keys: scroll reads work details; Alt+M shows metadata; Escape returns with parent draft preserved.");
                } else {
                    builder.line(format!("elapsed milliseconds: {:?}; input tokens: {:?}; output tokens: {:?}; current tool: {}", record.telemetry.elapsed_ms, record.telemetry.input_tokens, record.telemetry.output_tokens, record.telemetry.current_tool.as_deref().unwrap_or("unavailable")));
                    builder.line(format!(
                        "actions: message {}; interrupt {}; close {}; background {}",
                        record.capabilities.steer,
                        record.capabilities.interrupt,
                        record.capabilities.close,
                        record.capabilities.background
                    ));
                    for line in record.detail_lines() {
                        builder.line(line);
                    }
                    builder.line(
                        "Alt+M shows all task metadata; Alt+O changes the process output stream.",
                    );
                    builder.line("keys: Enter sends to selected task; Alt+I interrupts; Alt+X closes; Alt+B backgrounds; PageUp/PageDown reads output; Control+End follows; Escape returns with parent draft preserved.");
                }
            }
            if let Some(notice) = state.task_console.selected_notice() {
                builder.line(notice);
            }
        } else if let Some(panel) = state.side_panel_snapshot() {
            builder.section(panel.kind().title());
            builder.line(panel.summary());
            if panel.rows().is_empty() {
                builder.line("No rows are available.");
            } else {
                for row in panel.rows() {
                    builder.line(row.text());
                }
            }
            builder.line("keys: Control+B cycles Diff, Jobs, Agents, then closes.");
        }
        builder.finish()
    }

    /// Borrow the stable plain-text frame without a trailing newline.
    #[must_use]
    pub fn as_text(&self) -> &str {
        &self.text
    }

    /// Consume the frame into its stable plain-text representation.
    #[must_use]
    pub fn into_text(self) -> String {
        self.text
    }
}

/// Changed-frame writer for flat/no-alternate-screen sessions.
///
/// The writer receives plain UTF-8 only. Repeated loop iterations over an
/// unchanged state produce no output, so a screen reader does not hear the
/// same frame on every event-loop pass.
pub struct FlatOutput<W> {
    writer: W,
    previous: Option<ScreenReaderSnapshot>,
}

impl<W> FlatOutput<W>
where
    W: Write,
{
    /// Wrap a plain byte writer.
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self {
            writer,
            previous: None,
        }
    }

    /// Emit the current frame when it differs from the last emitted frame.
    ///
    /// # Errors
    /// Returns the underlying writer's failure.
    pub fn render(&mut self, state: &AppState) -> std::io::Result<bool> {
        let snapshot = ScreenReaderSnapshot::from_state(state);
        if self.previous.as_ref() == Some(&snapshot) {
            return Ok(false);
        }
        if self.previous.is_some() {
            self.writer.write_all(b"\r\n")?;
        }
        for line in snapshot.as_text().lines() {
            self.writer.write_all(line.as_bytes())?;
            self.writer.write_all(b"\r\n")?;
        }
        self.writer.flush()?;
        self.previous = Some(snapshot);
        Ok(true)
    }

    /// Recover the wrapped writer after rendering.
    #[must_use]
    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[derive(Default)]
struct FlatBuilder {
    lines: Vec<String>,
    truncated: bool,
}

impl FlatBuilder {
    fn section(&mut self, title: &str) {
        self.line(format!("== {title} =="));
    }

    fn line(&mut self, value: impl AsRef<str>) {
        if self.truncated {
            return;
        }
        let line = sanitize(value.as_ref());
        if line.is_empty() {
            return;
        }
        if self.lines.len() >= MAX_FLAT_LINES.saturating_sub(1) {
            self.lines
                .push("output truncated for screen reader".to_owned());
            self.truncated = true;
            return;
        }
        self.lines.push(line);
    }

    fn multiline(&mut self, label: &str, value: &str) {
        let mut any = false;
        for line in value.lines() {
            any = true;
            self.line(format!("{label}: {line}"));
        }
        if !any {
            self.line(format!("{label}: empty"));
        }
    }

    fn finish(self) -> ScreenReaderSnapshot {
        ScreenReaderSnapshot {
            text: self.lines.join("\n"),
        }
    }
}

fn sanitize(value: &str) -> String {
    let mut output = String::new();
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if character.is_control() {
            continue;
        }
        if pending_space && output.chars().count() < MAX_FLAT_LINE_CHARS {
            output.push(' ');
        }
        pending_space = false;
        if output.chars().count() >= MAX_FLAT_LINE_CHARS {
            if !output.ends_with('…') {
                output.push('…');
            }
            break;
        }
        output.push(character);
    }
    output
}

fn trim_sentence(value: &str) -> &str {
    value.trim().trim_end_matches('.')
}

fn selected(is_selected: bool) -> &'static str {
    if is_selected { "[selected]" } else { "[ ]" }
}

fn render_workspace_trust(builder: &mut FlatBuilder, view: &WorkspaceTrustView) {
    let dialog = view.state();
    builder.section("Workspace trust");
    builder.line("Trust this workspace?");
    builder.line(format!("workspace: {}", dialog.canonical_root().display()));
    for (label, access) in [
        ("project instructions", dialog.instructions()),
        ("project settings", dialog.settings()),
        (
            "project plugins, MCP, and hooks",
            dialog.project_executables(),
        ),
    ] {
        builder.line(format!(
            "{label}: {}",
            if access.is_allowed() {
                "allowed"
            } else {
                "blocked"
            }
        ));
    }
    for (index, action) in dialog.actions().iter().copied().enumerate() {
        let (label, detail) = match action {
            heycode_trust::WorkspaceTrustAction::TrustOnce => (
                "Trust once",
                "Enable project executable contributions for this process.",
            ),
            heycode_trust::WorkspaceTrustAction::TrustWorkspace => (
                "Trust this workspace",
                "Save trust for this canonical workspace identity.",
            ),
            heycode_trust::WorkspaceTrustAction::OpenRestricted => (
                "Open read-only",
                "Keep project executable contributions disabled.",
            ),
            heycode_trust::WorkspaceTrustAction::Exit => {
                ("Exit", "Leave without changing workspace trust.")
            }
        };
        builder.line(format!(
            "{} {label} — {detail}",
            selected(index == view.selected)
        ));
    }
    if let Some(error) = view.error.as_deref() {
        builder.line(format!("error: {error}"));
    }
    builder.line("keys: Up or Down chooses; Enter confirms; Escape exits; Control+C twice exits.");
}

fn render_base(builder: &mut FlatBuilder, state: &AppState) {
    builder.section("heycode");
    builder.line(format!("version: {}", env!("CARGO_PKG_VERSION")));
    let backend = if !state.runtime.is_empty() && state.runtime != "native" {
        state.runtime.as_str()
    } else if !state.provider.is_empty() {
        state.provider.as_str()
    } else if !state.runtime.is_empty() {
        state.runtime.as_str()
    } else {
        "unconfigured"
    };
    builder.line(if state.model.is_empty() {
        format!("route: {backend}")
    } else if state.active_model_label() != state.model {
        format!(
            "route: {backend}/{} (model id: {})",
            state.active_model_label(),
            state.model
        )
    } else {
        format!("route: {backend}/{}", state.model)
    });
    builder.line(match state.shell_preferences.header_density() {
        heycode_ui::preferences::HeaderDensity::Full => {
            format!("workspace: {}", state.cwd.display())
        }
        heycode_ui::preferences::HeaderDensity::Compact => format!(
            "workspace: {}",
            state.cwd.file_name().map_or_else(
                || state.cwd.display().to_string(),
                |name| { name.to_string_lossy().into_owned() }
            )
        ),
    });
    match state.workspace_context() {
        crate::workspace_context::WorkspaceContextState::Loading => {}
        crate::workspace_context::WorkspaceContextState::NotRepository => {
            builder.line("repository: not a Git repository");
        }
        crate::workspace_context::WorkspaceContextState::Unavailable(reason) => {
            builder.line(format!("repository: {reason}"));
        }
        crate::workspace_context::WorkspaceContextState::Ready(context) => {
            builder.line(if context.dirty {
                format!("branch: {} (uncommitted changes)", context.branch)
            } else {
                format!("branch: {}", context.branch)
            });
            match &context.pull_request {
                crate::workspace_context::PullRequestContext::Found {
                    number,
                    state,
                    title,
                    url,
                } => builder.line(format!("pull request: #{number} {state} — {title} — {url}")),
                crate::workspace_context::PullRequestContext::None => {
                    builder.line("pull request: none for this checkout");
                }
                crate::workspace_context::PullRequestContext::Unavailable(reason) => {
                    builder.line(format!("pull request: {reason}"));
                }
            }
        }
    }

    if state.task_console.active {
        builder.section("Task output");
        for line in state.task_console.output_lines() {
            builder.line(line);
        }
    } else if state.items.is_empty() {
        if let Some(welcome) = state.welcome.as_ref() {
            builder.section("Welcome");
            builder.line("heycode");
            builder.line(format!("runtime: {}", welcome.runtime));
            builder.line(format!("route: {}/{}", welcome.provider, welcome.model));
            builder.line(format!("permission: {}", welcome.permission));
            builder.line(format!("workspace: {}", welcome.workspace.display()));
            builder.line(format!("health: {}", health_text(&welcome.health)));
        } else {
            builder.section("Transcript");
            builder.line("empty");
        }
    } else if state.focus_view() {
        render_focus_transcript(builder, state);
    } else {
        builder.section("Transcript");
        let visible_items = state
            .items
            .iter()
            .filter(|item| {
                !is_lifecycle_diagnostic(item)
                    && !item.is_merged_tool()
                    && !item.is_group_hidden()
                    && !item.is_quiet_running_tool()
                    && !crate::transcript::quiet_orchestration(item)
            })
            .collect::<Vec<_>>();
        let omitted = visible_items.len().saturating_sub(MAX_TRANSCRIPT_ITEMS);
        if omitted > 0 {
            builder.line(format!("{omitted} earlier transcript items omitted"));
        }
        for item in visible_items.into_iter().skip(omitted) {
            render_item(builder, item, state.show_reasoning);
        }
    }

    let issues = state.task_console.pending_issue_count();
    if issues > 0 {
        builder.line(format!("{issues} unread agent issue(s). Open the issue affordance or /agents to review; d dismisses attention in the failure inspector without deleting evidence."));
    }
    if state.task_strip_visible() {
        builder.section("Task strip");
        builder.line(state.task_strip_summary());
        if state.task_console.strip_focused {
            let target = if state.task_console.focus_main {
                "Main".to_owned()
            } else {
                state
                    .task_console
                    .focused
                    .as_ref()
                    .and_then(|key| {
                        state
                            .task_console
                            .records
                            .iter()
                            .find(|row| &row.key == key)
                    })
                    .map_or_else(
                        || "Main".into(),
                        |row| format!("{} ({})", row.label, row.status_label()),
                    )
            };
            builder.line(format!("Agent navigation focused: {target}. Up and Down move; Enter opens; Escape returns to composer."));
        }
        if let Some(error) = &state.task_console.inventory_error {
            builder.line(format!("{error}. Showing last known agents."));
        }
        builder.line(format!(
            "{} expands or collapses tasks.",
            state.task_toggle_key_label()
        ));
    }
    let diagnostics = state
        .items
        .iter()
        .filter(|item| is_lifecycle_diagnostic(item))
        .collect::<Vec<_>>();
    if !diagnostics.is_empty() {
        builder.section("Session diagnostics");
        for item in diagnostics.into_iter().rev().take(8).rev() {
            render_item(builder, item, state.show_reasoning);
        }
    }

    if state.shell_preferences.footer_status() {
        builder.section("Status");
        builder.line(format!(
            "approval policy: {}",
            if state.permission.is_empty() {
                "unavailable"
            } else {
                &state.permission
            }
        ));
        builder.line(match state.context_meter() {
            Some(meter) => {
                let tilde = if meter.estimated { "~" } else { "" };
                match meter.percent {
                    Some(percent) => format!(
                        "context: {tilde}{} of {} ({percent} percent){}",
                        meter.tokens,
                        state.context_window.unwrap_or_default(),
                        if meter.warn {
                            " — compaction soon"
                        } else {
                            ""
                        }
                    ),
                    None => format!("context: {tilde}{} tokens", meter.tokens),
                }
            }
            None => "context: unavailable".to_owned(),
        });
        // The visible idle footer is one permission line, so accounted token
        // totals are reported here (and by `/usage` and `/stats`) rather than
        // only on a row a sighted reader can see.
        let (input_tokens, output_tokens) = state.usage.as_ref().map_or((0, 0), |usage| {
            (usage.prompt_tokens, usage.completion_tokens)
        });
        builder.line(format!("tokens: in {input_tokens} out {output_tokens}"));
        if let Some(budget) = &state.context_budget {
            builder.line(format!("context evidence: {:?}; capacity source: {:?}; usable tokens left: {:?}; auto compaction: {}; trigger: {:?}; state: {:?}", budget.confidence, budget.limit_source, budget.remaining(), budget.auto_compact, budget.compact_at, budget.activity));
        }
        if let Some(notice) = state.copy_notice_text() {
            builder.line(notice);
        }
        builder.line(format!("activity: {}", accessible_activity(state)));
        let inbox = state.inbox_pending();
        if !inbox.is_empty() {
            builder.line(format!(
                "inbox: {} follow-up; {} steer",
                inbox.next_turn, inbox.next_step
            ));
        }
    }

    builder.section("Composer");
    if state.task_console.strip_focused
        || state.task_console.view == crate::task_console::ConsoleView::List
        || state.task_console.preview
    {
        builder.line("Composer not focused; draft preserved.");
    }
    if let Some(title) = state.current_session_title() {
        builder.line(format!("session title: {title}"));
    }
    if let Some(color) = state.prompt_color() {
        builder.line(format!("session color: {}", color.as_str()));
    }
    if let Some(caption) = state.voice.caption() {
        builder.line(caption);
    }
    let input = state.input.lines().join("\n");
    if input.trim().is_empty() {
        builder.line("input: empty");
    } else {
        builder.multiline("input", &input);
    }
    builder.line(format!("attachments: {}", state.pending_attachments.len()));
    if state.vim_enabled {
        builder.line(format!(
            "composer mode: vim {}",
            if state.vim_insert { "insert" } else { "normal" }
        ));
    }
    if let Some(hint) = state.quit_hint() {
        builder.line(format!("quit: {hint}"));
    }
    if state.shell_preferences.footer_hints()
        && !state.task_console.strip_focused
        && state.task_console.view != crate::task_console::ConsoleView::List
        && !state.task_console.preview
    {
        builder.line(if state.has_active_turn() && state.native_inbox_available() {
            "keys: Enter steers; Tab queues follow-up; Escape interrupts; Control+P opens commands."
        } else if state.has_active_turn() {
            "keys: Enter and Tab controls are unavailable for this runtime; Escape interrupts; Control+P opens commands."
        } else {
            "keys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll."
        });
    }
}

fn render_focus_transcript(builder: &mut FlatBuilder, state: &AppState) {
    builder.section("Focus transcript");
    let start = state
        .items
        .iter()
        .rposition(|item| matches!(item, Item::User(_)))
        .unwrap_or(0);
    let turn = &state.items[start..];
    if let Some(user) = turn.iter().find(|item| matches!(item, Item::User(_))) {
        render_item(builder, user, false);
    }
    let tools = turn
        .iter()
        .filter_map(|item| match item {
            Item::Tool {
                name, result, view, ..
            } if !view.group_hidden && !view.merged => Some(format!(
                "{} {}",
                name.strip_prefix("mcp__heycode__").unwrap_or(name),
                result.as_ref().map_or("running", |(ok, _)| if *ok {
                    "succeeded"
                } else {
                    "failed"
                })
            )),
            Item::ServerTool {
                logical, result, ..
            } => Some(format!(
                "{logical} {}",
                result.as_ref().map_or("running", |result| {
                    if result.outcome() == heycode_core::ServerToolOutcome::Success {
                        "succeeded"
                    } else {
                        "failed"
                    }
                })
            )),
            _ => None,
        })
        .take(16)
        .collect::<Vec<_>>();
    if !tools.is_empty() {
        builder.line(format!("tools: {}", tools.join(", ")));
    }
    if let Some(answer) = turn
        .iter()
        .rev()
        .find(|item| matches!(item, Item::Assistant(_) | Item::Error(_)))
    {
        render_item(builder, answer, false);
    }
    for command in turn.iter().filter(|item| matches!(item, Item::Command(_))) {
        render_item(builder, command, false);
    }
    builder.line("focus view enabled; /focus restores the full transcript");
}

fn is_lifecycle_diagnostic(item: &Item) -> bool {
    item.is_lifecycle_diagnostic()
}

fn accessible_activity(state: &AppState) -> String {
    crate::render::current_activity(state)
        .trim_end_matches('…')
        .to_lowercase()
}

fn render_item(builder: &mut FlatBuilder, item: &Item, show_reasoning: bool) {
    if crate::transcript::quiet_orchestration(item) {
        return;
    }
    match item {
        Item::User(text) => builder.multiline("user", text),
        Item::Command(text) => builder.multiline("local command", text),
        Item::Attachments {
            attachments,
            document_routes,
        } => builder.line(format!(
            "attachments: {} selected; {} document routes",
            attachments.len(),
            document_routes.len()
        )),
        Item::AudioOutput { attachments } => {
            for attachment in attachments {
                let Some(audio) = attachment.audio() else {
                    continue;
                };
                builder.line(format!(
                    "assistant audio: {} — {} — {:.2} seconds — {} hertz — {} channels — {} bits",
                    attachment.display_name().unwrap_or("audio"),
                    attachment.media_type().as_str(),
                    std::time::Duration::from_millis(audio.duration_ms()).as_secs_f64(),
                    audio.sample_rate_hz(),
                    audio.channels(),
                    audio.bits_per_sample()
                ));
            }
        }
        Item::Assistant(text) => builder.multiline("assistant", text),
        Item::Reasoning { text, done, view } => {
            let expanded = view.expanded.unwrap_or(show_reasoning) || view.group_details;
            builder.line(view.label(*done, expanded));
            if expanded {
                builder.multiline("reasoning", text);
            } else if !done {
                if text.is_empty() {
                    builder.line("Provider has not supplied readable thinking text");
                } else {
                    let tail = text.lines().rev().take(2).collect::<Vec<_>>();
                    for line in tail.into_iter().rev() {
                        builder.line(line);
                    }
                }
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
            if name.strip_prefix("mcp__heycode__").unwrap_or(name) == "ask_user_question"
                && !view.expanded
                && !view.group_details
                && let Some((true, value)) = result
            {
                for line in crate::render::question_answer_lines(args, value)
                    .unwrap_or_else(|| vec!["Answer recorded; expand for details".into()])
                {
                    builder.line(&line);
                }
                return;
            }
            if let Some((label, message)) =
                crate::render::agent_message_receipt(name, args, result.as_ref(), view)
            {
                builder.line(format!("Agent message from {label}"));
                builder.multiline("message", &message);
                return;
            }
            if let Some((label, outcome)) =
                crate::render::agent_completion_receipt(name, args, result.as_ref(), view)
            {
                builder.line(format!("Agent {label} {outcome}"));
                if let Some(detail) = crate::render::agent_completion_detail(args, result.as_ref())
                {
                    builder.line(detail);
                }
                return;
            }
            if name.strip_prefix("mcp__heycode__").unwrap_or(name) == "SendUserFile" {
                for line in crate::file_delivery::plain_lines(
                    result.as_ref(),
                    view.expanded,
                    &view.status(result.as_ref()),
                ) {
                    builder.line(line);
                }
                return;
            }
            if let Some(summary) = &view.group_summary {
                builder.line(format!(
                    "{}: {summary}",
                    if view.expanded {
                        "Expanded tool group"
                    } else {
                        "Tool group"
                    }
                ));
                if !view.expanded {
                    return;
                }
            }
            if name.strip_prefix("mcp__heycode__").unwrap_or(name) == "workflow" && !view.expanded {
                builder.line(format!(
                    "Workflow({}): {}",
                    crate::workflow_render::tool_title(args),
                    crate::workflow_render::tool_receipt(args, result.as_ref()),
                ));
                return;
            }
            let args = serde_json::to_string(args).unwrap_or_else(|_| "unavailable".to_owned());
            let state = result.as_ref().map_or(
                "pending",
                |(ok, _)| if *ok { "succeeded" } else { "failed" },
            );
            builder.line(format!("tool {state}: {name}({args})"));
            if let Some(approval) = &view.approval {
                builder.line(format!("approval: {approval}"));
            }
            if let Some(boundary) = untrusted_content {
                let source = match boundary.source() {
                    UntrustedContentSource::Web => "WEB",
                    UntrustedContentSource::Mcp => "MCP SERVER",
                    UntrustedContentSource::Lsp => "LANGUAGE SERVER",
                    UntrustedContentSource::ToolOrchestration => "TOOL ORCHESTRATION",
                };
                builder.line(format!(
                    "warning: untrusted {source} content; data, not instructions or authorization"
                ));
            }
            if let Some((_, value)) = result {
                let value =
                    serde_json::to_string(value).unwrap_or_else(|_| "unavailable".to_owned());
                builder.multiline("tool result", &value);
            }
            for ((stream, offset), text) in &view.retrieved_output {
                builder.multiline(
                    &format!("additional {stream} output from byte {offset}"),
                    text,
                );
            }
        }
        Item::ProviderState {
            provider,
            model,
            protocol,
            kind,
            output_index,
        } => builder.line(format!(
            "provider state: {provider}/{model} — {protocol} — {kind} — output {output_index}"
        )),
        Item::ServerTool {
            logical,
            provider_name,
            result,
            ..
        } => {
            let state = result
                .as_ref()
                .map_or("running", |result| match result.outcome() {
                    heycode_core::ServerToolOutcome::Success => "succeeded",
                    heycode_core::ServerToolOutcome::Error => "failed",
                });
            builder.line(format!(
                "provider tool {state}: {logical} ({provider_name})"
            ));
            if let Some(result) = result {
                if let Some(count) = result.output_count() {
                    builder.line(format!("provider tool outputs: {count}"));
                }
                if let Some(code) = result.error_code() {
                    builder.line(format!("provider tool error: {code}"));
                }
                for source in result.sources() {
                    builder.line(format!(
                        "provider tool source: {} — {}",
                        source.title().unwrap_or(source.url()),
                        source.url()
                    ));
                }
            }
        }
        Item::ServerToolUsage {
            logical,
            requests,
            cost,
        } => builder.line(format!(
            "provider tool usage: {logical} — {requests} requests — {cost}"
        )),
        Item::Citation {
            url,
            title,
            cited_text,
            start_index,
            end_index,
        } => {
            builder.line(format!(
                "citation: {} — {url}",
                title.as_deref().unwrap_or(url)
            ));
            if let Some(text) = cited_text {
                let range = match (start_index, end_index) {
                    (Some(start), Some(end)) => format!(" [{start}..{end}]"),
                    _ => String::new(),
                };
                builder.multiline("citation excerpt", &format!("{text}{range}"));
            }
        }
        Item::FindingsReport {
            report, expanded, ..
        } => {
            let severity_label = |severity: heycode_session::ReviewSeverity| match severity {
                heycode_session::ReviewSeverity::Critical => "critical",
                heycode_session::ReviewSeverity::High => "high",
                heycode_session::ReviewSeverity::Medium => "medium",
                heycode_session::ReviewSeverity::Low => "low",
            };
            let severity_rank = |severity: heycode_session::ReviewSeverity| match severity {
                heycode_session::ReviewSeverity::Critical => 4,
                heycode_session::ReviewSeverity::High => 3,
                heycode_session::ReviewSeverity::Medium => 2,
                heycode_session::ReviewSeverity::Low => 1,
            };
            let highest = report
                .findings()
                .iter()
                .map(heycode_session::ReportedFinding::severity)
                .max_by_key(|severity| severity_rank(*severity))
                .unwrap_or(heycode_session::ReviewSeverity::Low);
            builder.line(format!(
                "{} findings report: {} finding{}; highest {}; workspace revision {}; local, not externally published",
                if *expanded { "expanded" } else { "collapsed" },
                report.findings().len(),
                if report.findings().len() == 1 { "" } else { "s" },
                severity_label(highest),
                report.source().workspace_revision(),
            ));
            let visible = if *expanded { 12 } else { 3 };
            for (index, finding) in report.findings().iter().take(visible).enumerate() {
                let location = if finding.line_start() == finding.line_end() {
                    format!("{}:{}", finding.path(), finding.line_start())
                } else {
                    format!(
                        "{}:{}-{}",
                        finding.path(),
                        finding.line_start(),
                        finding.line_end()
                    )
                };
                builder.line(format!(
                    "finding {}: {} — {} — {}",
                    index + 1,
                    severity_label(finding.severity()),
                    location,
                    finding.title()
                ));
                if *expanded {
                    builder.multiline("trigger", finding.trigger());
                    builder.multiline("failure", finding.failure());
                    builder.multiline("impact", finding.impact());
                    builder.line(format!(
                        "revision: {}",
                        finding.revision().chars().take(12).collect::<String>()
                    ));
                }
            }
            if report.findings().len() > visible {
                builder.line(format!(
                    "{} more findings retained in session history",
                    report.findings().len() - visible
                ));
            }
        }
        Item::Compaction {
            native,
            strategy,
            replaced_upto_seq,
            summary,
            provider_items,
            ..
        } => {
            if *native {
                builder.line(format!(
                    "native compaction: {} through event {replaced_upto_seq}; {provider_items} provider items",
                    strategy.as_deref().unwrap_or("provider-native")
                ));
            } else {
                builder.line(format!(
                    "portable compaction: through event {replaced_upto_seq}"
                ));
                if let Some(summary) = summary {
                    builder.multiline("compaction summary", summary);
                }
            }
        }
        Item::RuntimeLink { runtime } => builder.line(format!("runtime linked: {runtime}")),
        Item::RouteChange { provider, model } => {
            builder.line(format!("route changed: {provider}/{model}"));
        }
        Item::PlanMode { active } => builder.line(if *active {
            "plan mode: enabled"
        } else {
            "plan mode: disabled"
        }),
        Item::Goal {
            action,
            phase,
            objective,
            revision,
        } => {
            builder.line(format!(
                "goal {action}: revision {revision}{}",
                phase
                    .as_deref()
                    .map_or_else(String::new, |phase| format!("; {phase}"))
            ));
            if let Some(objective) = objective {
                builder.multiline("goal objective", objective);
            }
        }
        Item::Workflow { action, summary } => {
            builder.line(format!("workflow {action}: {summary}"));
        }
        Item::Schedule { action, summary } => {
            builder.line(format!("schedule {action}: {summary}"));
        }
        Item::Info(text) => builder.multiline("info", text),
        Item::Notice(text) => builder.multiline("notice", text),
        Item::Error(text) => builder.multiline("error", text),
    }
}

fn health_text(health: &WelcomeHealth) -> String {
    match health {
        WelcomeHealth::Checking => "checking".to_owned(),
        WelcomeHealth::Healthy { passed, warnings } => {
            format!("healthy; {passed} passed; {warnings} warnings")
        }
        WelcomeHealth::Unhealthy { failed, skipped } => {
            format!("unhealthy; {failed} failed; {skipped} skipped")
        }
        WelcomeHealth::Unavailable => "unavailable".to_owned(),
    }
}

fn render_runtime_question(builder: &mut FlatBuilder, question: &PendingRuntimeQuestionView) {
    builder.section(question.header.as_deref().unwrap_or("Question"));
    builder.line(format!("prompt: {}", question.prompt));
    if question.choices.is_empty() {
        builder.line(format!(
            "answer: {}",
            if question.input.is_empty() {
                "empty"
            } else {
                &question.input
            }
        ));
        builder.line("keys: Type enters an answer; Enter submits; Escape cancels.");
    } else {
        for (index, choice) in question.choices.iter().enumerate() {
            builder.line(format!(
                "{} {choice}",
                selected(index == question.selection)
            ));
            if let Some(description) = question
                .choice_descriptions
                .get(index)
                .and_then(Option::as_deref)
            {
                builder.line(format!("  {description}"));
            }
        }
        builder.line(format!(
            "{} Other: {}",
            selected(question.selection == question.choices.len()),
            if question.input.is_empty() {
                "empty"
            } else {
                question.input.as_str()
            }
        ));
        builder
            .line("keys: Up or Down chooses; typing edits Other; Enter submits; Escape cancels.");
    }
}

fn render_routes(builder: &mut FlatBuilder, route: &super::RoutePickerView) {
    builder.section("Provider and runtime");
    builder.line(format!("filter: {}", route.filter.as_str()));
    builder.line(format!(
        "query: {}",
        if route.query.is_empty() {
            "empty"
        } else {
            route.query.as_str()
        }
    ));
    if route.loading {
        builder.line("loading registry rows");
    } else if let Some(error) = route.error.as_deref() {
        builder.line(format!("error: {error}"));
    }
    for (index, matched) in route.matches.iter().enumerate() {
        let row = &matched.row;
        let availability = row.unavailable_reason.as_deref().map_or_else(
            || "available".to_owned(),
            |reason| format!("unavailable: {reason}"),
        );
        builder.line(format!(
            "{} {} {} — {} — {} — {}{}.",
            selected(index == route.selected),
            row.class.badge(),
            row.id,
            row.display_name,
            row.detail,
            if row.current { "current — " } else { "" },
            availability
        ));
    }
    builder.line("keys: Tab changes filter; Up or Down chooses; Enter selects; Escape closes.");
}

fn render_models(builder: &mut FlatBuilder, model: &super::ModelPickerView) {
    builder.section("Model picker");
    builder.line(format!("backend: {}", model.provider()));
    builder.line(format!("filter: {}", model.filter.as_str()));
    builder.line(format!(
        "query: {}",
        if model.query.is_empty() {
            "empty"
        } else {
            model.query.as_str()
        }
    ));
    match &model.state {
        ModelPickerLoadState::Loading => builder.line("catalog: loading"),
        ModelPickerLoadState::Ready { warning, .. } => {
            builder.line("catalog: ready");
            if let Some(warning) = warning {
                builder.line(format!("warning: {warning}"));
            }
        }
        ModelPickerLoadState::Error { message } => builder.line(format!("error: {message}")),
    }
    for (index, matched) in model.matches.iter().enumerate() {
        builder.line(format!(
            "{} {} — {}{}.",
            selected(index == model.selected),
            matched.model.id,
            matched.model.display_name,
            if matched.model.id == model.current_model {
                " — current"
            } else {
                ""
            }
        ));
    }
    for id in &model.unmatched_overrides {
        builder.line(format!("unmatched assertion: {id}"));
    }
    if let Some(effort) = model.effort_picker() {
        builder.line(format!(
            "effort preview: {}; saved default: {}; current: {}",
            effort
                .choices()
                .get(effort.selected())
                .map_or("unavailable", String::as_str),
            effort.default_effort().unwrap_or("backend default"),
            effort.current_effort().unwrap_or("backend default")
        ));
    } else if let Some(reason) = model.effort_error() {
        builder.line(format!("effort unavailable: {reason}"));
    }
    builder.line(
        "keys: Tab changes filter; Up or Down chooses; Left or Right adjusts effort; Control+R refreshes; Enter saves the default; s uses this session; slash searches; Escape closes without changing model or effort.",
    );
}

fn render_settings(builder: &mut FlatBuilder, panel: &crate::settings_panel::SettingsPanelView) {
    builder.section("Settings");
    for (index, row) in panel.rows().iter().enumerate() {
        let path = if row.path().is_empty() {
            row.namespace().to_owned()
        } else {
            format!("{}.{}", row.namespace(), row.path())
        };
        let value = match row.value() {
            crate::settings_panel::SettingsPanelValue::Toggle(value) => value.to_string(),
            crate::settings_panel::SettingsPanelValue::Text(value)
            | crate::settings_panel::SettingsPanelValue::Number(value) => value.clone(),
            crate::settings_panel::SettingsPanelValue::Choice { selected, .. } => selected
                .clone()
                .unwrap_or_else(|| "no valid selection".to_owned()),
            crate::settings_panel::SettingsPanelValue::Secret { configured } => {
                if *configured {
                    "secret configured".to_owned()
                } else {
                    "secret not configured".to_owned()
                }
            }
            crate::settings_panel::SettingsPanelValue::Unrenderable { reason } => reason.clone(),
            crate::settings_panel::SettingsPanelValue::Custom { panel } => {
                format!("custom panel {panel}")
            }
        };
        let mut detail = if row.editable() {
            "editable".to_owned()
        } else {
            row.explanation().unwrap_or("read only").to_owned()
        };
        if let Some(buffer) = panel.edit_buffer().filter(|_| index == panel.selected()) {
            detail = format!("editing {buffer}");
        }
        builder.line(format!(
            "{} {path}: {value} — {detail}.",
            selected(index == panel.selected())
        ));
    }
    if let Some(notice) = panel.notice() {
        builder.line(format!("notice: {notice}"));
    }
    builder.line("keys: Up or Down chooses; Enter edits; Escape closes.");
}

fn render_sessions(
    builder: &mut FlatBuilder,
    browser: &crate::session_browser::SessionBrowserView,
) {
    builder.section("Sessions");
    builder.line(format!(
        "page: {}; matches: {}",
        browser.page_index().saturating_add(1),
        browser.total_matches()
    ));
    if let Some(error) = browser.error() {
        builder.line(format!("error: {error}"));
    }
    for row in browser.rows() {
        let summary = row.summary();
        let label = summary.title().unwrap_or(summary.id().as_str());
        builder.line(format!(
            "workspace: {}",
            summary
                .cwd()
                .map_or_else(|| "unknown".to_owned(), |path| path.display().to_string())
        ));
        builder.line(format!(
            "{} {} — {}{}{}{}.",
            selected(browser.selected() == Some(row)),
            label,
            summary.id().as_str(),
            if row.is_current() { " — current" } else { "" },
            if row.is_latest() { " — latest" } else { "" },
            if summary.is_readable() {
                ""
            } else {
                " — unreadable"
            }
        ));
    }
    if let Some(notice) = browser.notice() {
        builder.line(format!("notice: {notice}"));
    }
    if let Some(confirm) = browser.delete_confirmation() {
        builder.line(format!(
            "delete confirmation for {}; selected: {:?}",
            confirm.session_id().as_str(),
            confirm.choice()
        ));
    }
    builder.line(format!(
        "keys: {}; Page Up or Page Down changes page.",
        browser.hint().trim()
    ));
}
