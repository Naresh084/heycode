//! Persistent shell and composer-adjacent controls at real terminal sizes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use heycode_agent::{
    Command, CommandDescriptor, CommandRegistry, CommandSource, CommandTiming, UiEvent,
};
use heycode_tui::human_commands::{HumanCommandBridge, HumanCommandRequest};
use heycode_tui::{app::AppState, render};
use heycode_ui::preferences::{HeaderDensity, ShellChromePreferences};
use ratatui::{Terminal, backend::TestBackend};

struct MenuCommand(CommandDescriptor);

#[async_trait]
impl Command for MenuCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.0
    }
    async fn execute(&self, _: &heycode_agent::Agent, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn state() -> AppState {
    let mut state = AppState::new("model-a", std::env::temp_dir().join("heycode-ui-proof"));
    state.provider = "provider-a".to_owned();
    state.runtime = "native".to_owned();
    state.permission = "ask".to_owned();
    state.apply(&UiEvent::Info {
        text: "Existing conversation remains visible".to_owned(),
    });
    let mut commands = CommandRegistry::new();
    for (id, description) in [
        ("model", "Choose a model"),
        ("settings", "Configure heycode"),
        ("permissions", "Change approval mode"),
    ] {
        commands
            .register(Arc::new(MenuCommand(
                CommandDescriptor::new(
                    id,
                    description,
                    Vec::new(),
                    CommandTiming::Immediate,
                    CommandSource::from_plugin("commands").unwrap(),
                )
                .unwrap(),
            )))
            .unwrap();
    }
    state.set_commands(Arc::new(commands));
    state
}

fn frame(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render::draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn active_conversation_keeps_banner_first_and_actionable_footer() {
    let lines = frame(&mut state(), 120, 35);
    let composer = lines
        .iter()
        .position(|line| line.trim_start().starts_with('❯'))
        .unwrap();
    let banner = lines
        .iter()
        .position(|line| line.contains("HeyCode 0.1.0"))
        .unwrap();
    assert!(
        banner < composer
            && lines[..composer]
                .iter()
                .any(|line| line.contains("model-a"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Existing conversation remains visible"))
    );
    // Permission controls and the parent context each retain a footer row.
    assert!(
        lines[34].contains("⏸ manual mode on") && !lines[34].contains("/ commands"),
        "{:?}",
        lines[34]
    );
    assert!(
        lines[33].contains("context") || lines[33].contains("ctx:"),
        "{:?}",
        lines[33]
    );
    assert!(lines[32].trim_start().starts_with('─'), "{:?}", lines[32]);
}

#[test]
fn slash_choices_are_plain_rows_next_to_the_composer() {
    let mut state = state();
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('/'),
        KeyModifiers::NONE,
    )));
    let lines = frame(&mut state, 120, 35);
    let command_y = lines
        .iter()
        .position(|line| line.contains("Choose a model"))
        .unwrap();
    let composer = lines.iter().position(|line| line.trim() == "❯ /").unwrap();
    assert!(
        command_y < composer && composer - command_y <= 5,
        "menu must sit just above composer: {lines:#?}"
    );
    assert!(
        lines[command_y].contains("/model"),
        "command and explanation share one row"
    );
    assert!(!lines[command_y].contains('│'));
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("fuzzy search") || line.contains("commands · immediate"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Existing conversation remains visible"))
    );
}

#[test]
fn shell_and_open_menu_fit_tiny_and_wide_terminals() {
    for (width, height) in [(1, 1), (20, 5), (40, 12), (80, 24), (160, 48)] {
        let mut state = state();
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        )));
        let lines = frame(&mut state, width, height);
        assert_eq!(lines.len(), usize::from(height));
    }
}

#[test]
fn a_short_terminal_keeps_identity_and_transcript_before_secondary_hints() {
    let lines = frame(&mut state(), 30, 8);
    let composer = lines
        .iter()
        .position(|line| line.trim_start().starts_with('❯'))
        .unwrap();
    let banner = lines
        .iter()
        .position(|line| line.contains("HeyCode"))
        .unwrap();
    assert!(banner < composer && lines[banner].contains("model-a"));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Existing conversation")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("/settings")),
        "the hint row yields before transcript content: {lines:#?}"
    );
}

#[test]
fn persisted_shell_preferences_control_header_density_and_each_footer_row() {
    let mut state = state();
    let bridge = HumanCommandBridge::new();
    state.set_human_commands(bridge.clone());
    bridge.request(HumanCommandRequest::ApplyShell(
        ShellChromePreferences::new(HeaderDensity::Compact, false, false),
    ));
    state.poll_human_command();

    let lines = frame(&mut state, 120, 35);
    assert!(
        lines.iter().any(|line| line.contains("HeyCode"))
            && lines.iter().any(|line| line.contains("heycode-ui-proof"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("/model choose model"))
    );
    assert!(!lines.iter().any(|line| line.contains(" · ask")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Existing conversation remains visible"))
    );
}

#[test]
fn effort_surface_keeps_selected_value_visible_in_short_terminals() {
    let mut state = state();
    state.open_effort_picker(
        heycode_agent::BackendControlOwner::NativeInference {
            provider: "provider-a".to_owned(),
        },
        1,
        Some("effort-29".to_owned()),
        (0..30).map(|index| format!("effort-{index}")).collect(),
        Some("effort-0".to_owned()),
    );
    let lines = frame(&mut state, 80, 14);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("effort-29") && line.contains("current")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Existing conversation"))
    );
    assert!(
        lines.iter().any(|line| line.contains("Effort")),
        "{lines:#?}"
    );
    assert!(lines.iter().any(|line| line.contains("Esc")), "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("shift+tab to cycle")),
        "{lines:#?}"
    );
}

#[test]
fn status_shows_active_effort_with_full_and_compact_banners() {
    for density in [HeaderDensity::Full, HeaderDensity::Compact] {
        let mut state = state();
        state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
        state.reasoning_effort = Some("xhigh".to_owned());
        state.set_shell_preferences(ShellChromePreferences::new(density, true, true));
        let lines = frame(&mut state, 100, 30);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("xhigh effort") || line.contains("xhigh · /effort")),
            "{lines:#?}"
        );
    }
}

#[test]
fn short_effort_lists_replace_the_composer_with_the_source_shaped_scale() {
    let mut state = state();
    state.input.insert_str("preserved draft");
    state.open_effort_picker(
        heycode_agent::BackendControlOwner::NativeInference {
            provider: "provider-a".to_owned(),
        },
        1,
        Some("high".to_owned()),
        vec!["low".to_owned(), "high".to_owned()],
        None,
    );
    let lines = frame(&mut state, 100, 28);
    let title = lines
        .iter()
        .position(|line| line.trim() == "Effort")
        .unwrap();
    let choices = lines
        .iter()
        .position(|line| line.contains("low") && line.contains("high"))
        .unwrap();
    let footer = lines
        .iter()
        .position(|line| line.contains("←/→ to adjust"))
        .unwrap();
    assert!(title < choices && choices < footer, "{lines:#?}");
    assert!(
        !lines.iter().any(|line| line.contains("preserved draft")),
        "{lines:#?}"
    );
    assert_eq!(state.input.lines(), ["preserved draft"]);
}

#[test]
fn shift_tab_cycles_real_permission_commands_without_submitting_the_draft() {
    let mut state = state();
    state.input.insert_str("keep this draft");
    for (current, next, key) in [
        (
            "full_access",
            "accepted_edits",
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        ),
        (
            "accepted_edits",
            "ask",
            KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
        ),
        (
            "ask",
            "plan",
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        ),
    ] {
        state.permission = current.to_owned();
        state.handle_terminal_event(&Event::Key(key));
        assert_eq!(
            state.pending_send.take(),
            Some(format!("/permissions {next}"))
        );
        assert_eq!(
            state.permission, current,
            "wait for the policy owner to commit"
        );
        assert_eq!(state.input.lines(), &["keep this draft"]);
    }
    let before = state.items.len();
    state.apply(&UiEvent::Info {
        text: "Permissions: Plan. Inspect and plan; review before making changes.".to_owned(),
    });
    assert_eq!(
        state.items.len(),
        before,
        "shortcut confirmation belongs in the footer"
    );
    state.apply(&UiEvent::Info {
        text: "Permissions: Default. We'll ask for permission each time.".to_owned(),
    });
    assert_eq!(
        state.items.len(),
        before + 1,
        "a typed command still has a visible result"
    );
    state.open_effort_picker(
        heycode_agent::BackendControlOwner::NativeInference {
            provider: "provider-a".to_owned(),
        },
        1,
        None,
        vec!["low".to_owned()],
        None,
    );
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )));
    assert!(
        state.pending_send.is_none(),
        "an open dialog owns the shortcut"
    );
}

#[test]
fn committed_logout_settles_a_receipt_and_leaves_instead_of_reopening_setup() {
    let mut state = state();
    state.input.insert_str("old draft");
    state.apply(&UiEvent::LoggedOut {
        target: "claude".to_owned(),
        cleanup_warning: Some("Saved credential could not be removed".to_owned()),
    });
    // A disconnect answers with what it disconnected and exits; dropping the
    // session into the setup chooser would answer it with a demand to
    // reconnect, and the cleanup failure must not be swallowed on the way out.
    let snapshot = heycode_tui::ScreenReaderSnapshot::from_state(&state);
    let rendered = snapshot.as_text();
    assert!(rendered.contains("Disconnected claude"), "{rendered}");
    assert!(
        rendered.contains("Saved credential could not be removed"),
        "{rendered}"
    );
    assert!(state.quit_requested);
    assert_eq!(state.take_run_outcome(), None);
}

#[test]
fn shift_tab_leaves_plan_even_during_a_live_turn_or_pending_review() {
    for review in [false, true] {
        let mut state = state();
        state.permission = "plan".into();
        state.input.insert_str("preserve this draft");
        state.apply(&UiEvent::TurnStarted { turn: 1 });
        if review {
            state.apply(&UiEvent::PlanReviewRequested {
                id: 7,
                plan: "# Proposed changes".into(),
            });
        }
        let count = state.items.len();
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        )));
        assert_eq!(
            state.pending_send.take().as_deref(),
            Some("/permissions full_access")
        );
        assert_eq!(
            state.permission, "plan",
            "only the owner may commit the mode"
        );
        assert_eq!(
            state.items.len(),
            count,
            "mode cycling cannot spam the transcript"
        );
        assert_eq!(state.input.lines(), &["preserve this draft"]);
    }
}

#[tokio::test]
async fn approval_can_enter_edit_mode_and_waits_for_the_actual_policy_commit() {
    use heycode_agent::{
        ApprovalPolicy, ApprovalPolicyKind, InteractiveApproval, SwitchableApproval,
    };
    let mut state = state();
    let interactive = Arc::new(InteractiveApproval::new(heycode_core::EventBus::default()));
    let mut requests = interactive.take_subscription().unwrap();
    let policy = Arc::new(SwitchableApproval::new(
        interactive.clone(),
        Some(interactive.clone()),
    ));
    state.approvals = Some(interactive.clone());
    let pending = tokio::spawn({
        let policy = policy.clone();
        async move {
            policy
                .decide(&heycode_tools::ToolCallInput {
                    name: "read".into(),
                    args: serde_json::json!({"path":"README.md"}),
                })
                .await
        }
    });
    let request = requests.recv().await.unwrap();
    state.apply(&UiEvent::ToolStarted {
        name: "read".into(),
        args: serde_json::json!({"path":"README.md"}),
    });
    state.apply(&UiEvent::ApprovalRequested {
        owner_session: None,
        id: request.id,
        name: request.name,
        args_preview: request.args_preview,
    });
    assert!(
        frame(&mut state, 110, 30)
            .join("\n")
            .contains("Yes, and allow file edits this session")
    );
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
    )));
    assert_eq!(
        state.pending_send.take().as_deref(),
        Some("/permissions accepted_edits")
    );
    assert!(!pending.is_finished());
    assert_eq!(policy.kind(), ApprovalPolicyKind::Ask);
    policy
        .switch_by_user(ApprovalPolicyKind::AcceptedEdits)
        .unwrap();
    state.apply(&UiEvent::PermissionModeChanged {
        mode: ApprovalPolicyKind::AcceptedEdits,
    });
    assert!(matches!(
        pending.await.unwrap(),
        heycode_tools::Verdict::Allow
    ));
    assert!(state.pending_ask.is_none());
    assert_eq!(state.permission, "accepted_edits");
    assert!(
        matches!(state.items.last(), Some(heycode_tui::app::Item::Tool { view, .. }) if view.approval.as_deref() == Some("approved"))
    );
}

#[test]
fn compact_tool_cards_expand_by_click_and_keyboard_without_losing_output_or_draft() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut state = state();
    let output = (0..80)
        .map(|index| format!("RETAINED_LINE_{index:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    state.items.push(heycode_tui::app::Item::Tool {
        call_id: None,
        name: "bash".into(),
        args: serde_json::json!({"command":"print fixture"}),
        result: Some((true, serde_json::json!(output))),
        untrusted_content: None,
        view: Default::default(),
    });
    state.input.insert_str("draft stays here");
    let lines = frame(&mut state, 120, 24);
    assert!(!lines.join("\n").contains("RETAINED_LINE_00"));
    let row = lines
        .iter()
        .position(|line| line.contains("  Ran 1 shell command"))
        .unwrap() as u16;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind,
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    let expanded = frame(&mut state, 120, 24).join("\n");
    assert!(expanded.contains("RETAINED_LINE_00"), "{expanded}");
    assert!(expanded.contains("Bash(print fixture)"), "{expanded}");
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let collapsed = frame(&mut state, 80, 14).join("\n");
    assert!(collapsed.contains("  Ran 1 shell command"), "{collapsed}");
    assert!(!collapsed.contains("RETAINED_LINE_00"));
    assert_eq!(state.input.lines(), &["draft stays here"]);
    assert!(
        matches!(state.items.last(), Some(heycode_tui::app::Item::Tool { result: Some((true, value)), .. }) if value.as_str() == Some(output.as_str()))
    );
}

#[test]
fn composer_keywords_are_colored_without_underlining_and_clear_with_the_draft() {
    use ratatui::style::{Color, Modifier};
    let mut state = state();
    state
        .input
        .insert_str("please use workflow to inspect this");
    let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
    terminal.draw(|f| render::draw(f, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..28)
        .map(|y| (0..110).map(|x| buffer[(x, y)].symbol()).collect())
        .collect();
    let row = rows
        .iter()
        .position(|line| line.contains("please use workflow"))
        .unwrap();
    let start = rows[row][..rows[row].find("workflow").unwrap()]
        .chars()
        .count();
    assert!(
        rows.iter()
            .any(|line| line.contains("Workflow requested for this turn"))
    );
    for x in start..start + 8 {
        assert_eq!(buffer[(x as u16, row as u16)].fg, Color::Rgb(167, 139, 250));
    }
    assert!((0..110).all(|x| {
        !buffer[(x, row as u16)]
            .modifier
            .contains(Modifier::UNDERLINED)
    }));
    state.input = tui_textarea::TextArea::default();
    state.input.insert_str("ordinary request");
    assert!(
        !frame(&mut state, 110, 28)
            .join("\n")
            .contains("Workflow requested")
    );
}

#[test]
fn task_output_preview_shows_text_and_keeps_metadata_in_expansion() {
    let mut state = state();
    state.apply(&UiEvent::ToolStarted {
        name: "job_output".into(),
        args: serde_json::json!({"job_id":"job-1"}),
    });
    state.apply(&UiEvent::ToolFinished { name: "job_output".into(), ok: true, value: serde_json::json!({"job_id":"job-1","page":{"text":"architecture.md\ngetting-started.md\nFULL_TAIL","total_bytes":128}}), untrusted_content: None });
    let text = frame(&mut state, 110, 28).join("\n");
    assert!(text.contains("Task output"));
    assert!(text.contains("architecture.md"));
    assert!(!text.contains("total_bytes"));
    assert!(!text.contains("FULL_TAIL"));
}

#[test]
fn retrieved_pages_merge_into_the_exact_original_command_and_keep_its_status() {
    use heycode_tui::app::Item;
    let mut state = state();
    state.apply(&UiEvent::ToolStarted {
        name: "bash".into(),
        args: serde_json::json!({"command":"inspect project"}),
    });
    state.apply(&UiEvent::ToolFinished {
        name: "bash".into(),
        ok: true,
        value: serde_json::json!("[inline output capped; use job_output job-9]\nretained tail"),
        untrusted_content: None,
    });
    for text in ["first page", "updated first page"] {
        state.apply(&UiEvent::ToolStarted {
            name: "job_output".into(),
            args: serde_json::json!({"job_id":"job-9"}),
        });
        state.apply(&UiEvent::ToolFinished {
            name: "job_output".into(),
            ok: true,
            value: serde_json::json!({"job_id":"job-9","page":{"offset":0,"text":text}}),
            untrusted_content: None,
        });
    }
    let original = state
        .items
        .iter()
        .find(|item| matches!(item, Item::Tool { name, .. } if name == "bash"))
        .unwrap();
    assert!(
        matches!(original, Item::Tool { result: Some((true,_)), view, .. } if view.retrieved_output.len() == 1 && view.retrieved_output.values().next().unwrap() == "updated first page")
    );
    assert_eq!(
        state
            .items
            .iter()
            .filter(|item| matches!(item, Item::Tool { view, .. } if view.merged))
            .count(),
        2
    );
    let text = frame(&mut state, 110, 28).join("\n");
    assert!(text.contains("Ran 1 shell command"));
    assert!(!text.contains("updated first page"));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let expanded = frame(&mut state, 110, 28).join("\n");
    assert!(expanded.contains("Bash(inspect project)"));
    assert!(expanded.contains("updated first page"));
    assert!(!text.contains("Task output"));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)));
    assert!(state.items.iter().any(
        |item| matches!(item, Item::Tool { name, view, .. } if name == "bash" && view.focused)
    ));
    assert!(
        !state
            .items
            .iter()
            .any(|item| matches!(item, Item::Tool { view, .. } if view.merged && view.focused))
    );
    state.apply(&UiEvent::ToolStarted {
        name: "job_output".into(),
        args: serde_json::json!({"job_id":"job-9"}),
    });
    state.apply(&UiEvent::ToolFinished {
        name: "job_output".into(),
        ok: false,
        value: serde_json::json!("retrieval failed"),
        untrusted_content: None,
    });
    assert!(
        frame(&mut state, 110, 28)
            .join("\n")
            .contains("retrieval failed")
    );
}
