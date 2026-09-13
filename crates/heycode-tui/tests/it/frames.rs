//! Frame tests over `ratatui::backend::TestBackend` — the UI contract pinned.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_tui::app::{AppState, Item, PendingRuntimeQuestionView};
use heycode_tui::render::draw;
use ratatui::{Terminal, backend::TestBackend};

fn key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    use crossterm::event::KeyEvent;
    crossterm::event::Event::Key(KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    })
}

fn frame_text(state: &mut AppState, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal_draw(&mut term, state);
    let buffer = term.backend().buffer().clone();
    buffer
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<Vec<_>>()
        .join("")
}

fn terminal_draw(terminal: &mut Terminal<TestBackend>, state: &mut AppState) {
    terminal.draw(|frame| draw(frame, state)).unwrap();
}

#[test]
fn user_lines_render_with_prompt_marker() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::User("fix the bug".into()));
    let text = frame_text(&mut state, 60, 12);
    assert!(text.contains('❯'), "prompt marker missing: {text}");
    assert!(text.contains("fix the bug"));
}

#[test]
fn conversation_rows_band_the_source_line_and_gutter_the_assistant() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::User("hi".into()));
    state.items.push(Item::Command("/memory".into()));
    state
        .items
        .push(Item::Assistant("Hello.\n\nHow can I help?".into()));
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal_draw(&mut terminal, &mut state);
    let buffer = terminal.backend().buffer();
    let user_y = (0..20)
        .find(|y| {
            (0..60)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("❯ hi")
        })
        .expect("user row");
    assert!(
        (0..60).all(|x| buffer[(x, user_y)].bg == state.styles().prompt_background()),
        "the selected user row background must reach both edges"
    );
    let command_y = (0..20)
        .find(|y| {
            (0..60)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("❯ /memory")
        })
        .expect("command row");
    // A command echo is banded to the width of its own text, which is what
    // the Claude Code 2.1.269 captures record for `❯ /memory` at 126 columns.
    let banded = (0..60)
        .filter(|x| buffer[(*x, command_y)].bg == state.styles().prompt_background())
        .count();
    assert_eq!(
        banded,
        "❯ /memory".chars().count() + 1,
        "band {banded} cells"
    );
    let text = frame_text(&mut state, 60, 20);
    assert!(text.contains("● Hello."), "{text}");
    assert!(text.contains("  How can I help?"), "{text}");
}

#[test]
fn lifecycle_wiring_stays_out_of_chat_but_remains_in_accessible_diagnostics() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::RuntimeLink {
        runtime: "claude".to_owned(),
    });
    state.items.push(Item::RouteChange {
        provider: "anthropic".to_owned(),
        model: "opus".to_owned(),
    });

    let text = frame_text(&mut state, 80, 18);
    assert!(!text.contains("runtime linked"), "{text}");
    assert!(!text.contains("route changed"), "{text}");
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("== Session diagnostics =="), "{flat}");
    assert!(flat.contains("runtime linked: claude"), "{flat}");
    assert!(flat.contains("route changed: anthropic/opus"), "{flat}");
}

#[test]
fn header_companion_is_visible_by_default_and_can_be_disabled() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    let default = frame_text(&mut state, 80, 18);
    assert!(default.contains("▟██▅▅▅██▙"), "{default}");
    assert!(default.contains("m  ·  Unconfigured"), "{default}");
    assert!(
        !default.contains("Message") && !default.contains("effort · /effort"),
        "the idle banner does not invent effort metadata: {default}"
    );
    state.set_shell_preferences(
        heycode_ui::preferences::ShellChromePreferences::default().with_header_pet(false),
    );
    assert!(!frame_text(&mut state, 80, 18).contains("▟██▅▅▅██▙"));
}

#[test]
fn header_companion_has_deterministic_idle_and_working_phases() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    assert!(frame_text(&mut state, 80, 18).contains("● ᴗ ●"));

    state.pet_frame = 5;
    assert!(frame_text(&mut state, 80, 18).contains("─ ᴗ ─"));

    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.spinner = 2;
    assert!(frame_text(&mut state, 80, 18).contains("◕ ᴗ ●"));
}

#[test]
fn task_summary_recognizes_host_qualified_todo_write_results() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "mcp__heycode__todo_write".to_owned(),
        args: serde_json::json!({}),
    });
    state.apply(&heycode_agent::UiEvent::ToolFinished {
        name: "mcp__heycode__todo_write".to_owned(),
        ok: true,
        value: serde_json::json!([
            {"content":"repair composer","status":"completed"},
            {"content":"verify live terminal","status":"in_progress"}
        ]),
        untrusted_content: None,
    });
    let text = frame_text(&mut state, 100, 30);
    assert!(text.contains("Plan 1/2 complete"), "{text}");
    assert!(text.contains("● verify live terminal"), "{text}");
    assert!(text.contains("✓ repair composer"), "{text}");
}

#[test]
fn replayed_todo_json_is_structured_for_the_same_task_summary() {
    let call_id = heycode_core::CallId::from_raw("todo-call");
    let events = vec![
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 0,
            time_ms: 0,
            kind: heycode_session::SessionEventKind::ToolCall {
                turn: 0,
                call_id: call_id.clone(),
                name: "todo_write".to_owned(),
                args: serde_json::json!({}),
            },
        },
        heycode_session::SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 1,
            time_ms: 1,
            kind: heycode_session::SessionEventKind::ToolResult {
                call_id,
                content: serde_json::json!([
                    {"content":"restore task projection","status":"completed"},
                    {"content":"inspect resumed frame","status":"pending"}
                ])
                .to_string(),
                is_error: false,
                untrusted_content: None,
            },
        },
    ];
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.replay(&events);
    let text = frame_text(&mut state, 100, 30);
    assert!(text.contains("Plan 1/2 complete"), "{text}");
    assert!(text.contains("○ inspect resumed frame"), "{text}");
    assert!(text.contains("✓ restore task projection"), "{text}");
}

#[test]
fn workspace_header_shows_cached_branch_and_pr_without_rendering_probe_failures() {
    use heycode_tui::workspace_context::{
        PullRequestContext, WorkspaceContext, WorkspaceContextState,
    };

    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.set_workspace_context(WorkspaceContextState::Ready(WorkspaceContext {
        branch: "feature/ui".to_owned(),
        dirty: false,
        pull_request: PullRequestContext::Found {
            number: 42,
            state: "OPEN".to_owned(),
            title: "Repair the terminal".to_owned(),
            url: "https://example.test/pull/42".to_owned(),
        },
    }));
    // Cached branch and PR facts remain visible while idle.
    let text = frame_text(&mut state, 120, 16);
    assert_eq!(text.matches("feature/ui").count(), 1, "{text}");
    assert!(text.contains("PR #42 open"), "{text}");
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("branch: feature/ui"), "{flat}");
    assert!(
        flat.contains("pull request: #42 OPEN — Repair the terminal"),
        "{flat}"
    );

    state.set_workspace_context(WorkspaceContextState::Unavailable("git unavailable"));
    assert!(!frame_text(&mut state, 120, 16).contains("git unavailable"));
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("repository: git unavailable"), "{flat}");
}

#[test]
fn tool_cards_render_bullet_result_and_error_colors() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: heycode_tui::app::ToolViewState {
            expanded: true,
            ..Default::default()
        },
        call_id: None,
        name: "write".into(),
        args: serde_json::json!({"path": "a.rs"}),
        result: Some((
            true,
            serde_json::json!({
                "message": "Wrote 1 line to /tmp/a.rs",
                "diff": "-old line\n+new line"
            }),
        )),
        untrusted_content: None,
    });
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "bash".into(),
        args: serde_json::json!({"command": "exit 1"}),
        result: Some((false, serde_json::json!("boom\n[exit code: 1]"))),
        untrusted_content: None,
    });
    let text = frame_text(&mut state, 70, 35);
    assert!(text.contains("⏺"), "{text}");
    assert!(text.contains("⎿"), "{text}");
    assert!(text.contains("Write(a.rs)"), "{text}");
    assert!(text.contains("+new line"), "diff rendered: {text}");
    assert!(
        text.contains("Error: Exit code 1"),
        "failure receipt: {text}"
    );
}

#[test]
fn failed_search_is_an_error_and_read_preview_is_a_summary() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    for (name, args, ok, output) in [
        (
            "glob",
            serde_json::json!({"pattern":"**/*.rs"}),
            false,
            "Search failed: too many open files",
        ),
        (
            "read",
            serde_json::json!({"path":"main.rs"}),
            true,
            "1\tfn main() {\n2\t    run();\n3\t}",
        ),
    ] {
        state.items.push(Item::Tool {
            view: Default::default(),
            call_id: None,
            name: name.into(),
            args,
            result: Some((ok, serde_json::json!(output))),
            untrusted_content: None,
        });
    }
    let text = frame_text(&mut state, 100, 24);
    assert!(
        text.contains("Search failed: too many open files"),
        "{text}"
    );
    assert!(!text.contains("matches"), "{text}");
    assert!(text.contains("Read 1 file"), "{text}");
    assert!(!text.contains("run();"), "{text}");
    if let Item::Tool { view, .. } = &mut state.items[1] {
        view.expanded = true;
    }
    let expanded = frame_text(&mut state, 100, 24);
    assert!(expanded.contains("run();"));
    assert!(expanded.contains("1    fn main()"), "{expanded}");
}

#[test]
fn expanded_retained_output_shows_text_instead_of_json_escapes() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool { view: heycode_tui::app::ToolViewState { expanded: true, ..Default::default() },
        call_id: None, name: "job_output".into(), args: serde_json::json!({"job_id":"job-1"}),
        result: Some((true, serde_json::json!({"job_id":"job-1", "page":{"text":"FIRST_LINE\nSECOND_LINE", "offset":0}}))),
        untrusted_content: None });
    let text = frame_text(&mut state, 100, 24);
    assert!(text.contains("FIRST_LINE"));
    assert!(text.contains("SECOND_LINE"));
    assert!(!text.contains("\\n"), "{text}");
    assert!(!text.contains("\"page\""), "{text}");
}

#[test]
fn external_web_provenance_is_in_expanded_details_not_the_conversation() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "web_fetch".to_owned(),
        args: serde_json::json!({"url":"https://example.test"}),
        result: Some((true, serde_json::json!("external text"))),
        untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
    });
    let text = frame_text(&mut state, 72, 12);
    assert!(!text.contains("UNTRUSTED"), "{text}");
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    let text = frame_text(&mut state, 100, 20);
    assert!(
        text.contains("Source: Web (external tool content)"),
        "{text}"
    );
    assert!(text.contains("external text"), "{text}");
}

#[test]
fn untrusted_mcp_tool_result_names_its_actual_source() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "mcp__fixture__weather".to_owned(),
        args: serde_json::json!({}),
        result: Some((true, serde_json::json!({"schemaVersion":1}))),
        untrusted_content: Some(heycode_core::UntrustedContentBoundary::mcp()),
    });
    let text = frame_text(&mut state, 78, 12);
    assert!(!text.contains("UNTRUSTED"), "{text}");
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    let text = frame_text(&mut state, 100, 20);
    assert!(
        text.contains("Source: Mcp (external tool content)"),
        "{text}"
    );
}

#[test]
fn untrusted_lsp_tool_result_names_its_actual_source() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "lsp_diagnostics".to_owned(),
        args: serde_json::json!({"server":"rust"}),
        result: Some((true, serde_json::json!([{"message":"external"}]))),
        untrusted_content: Some(heycode_core::UntrustedContentBoundary::lsp()),
    });
    let text = frame_text(&mut state, 82, 12);
    assert!(!text.contains("UNTRUSTED"), "{text}");
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    let text = frame_text(&mut state, 100, 20);
    assert!(
        text.contains("Source: Lsp (external tool content)"),
        "{text}"
    );
}

#[test]
fn status_line_shows_model_usage_spinner_and_active_native_key_meaning() {
    let mut state = AppState::new("deepseek-chat", std::path::PathBuf::from("/proj"));
    state.runtime = "native".to_owned();
    state.usage = Some(heycode_core::TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 20,
    });
    state.context_window = Some(1000);
    state.context_tokens = Some(120);
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&heycode_agent::UiEvent::Status {
        verb: "Forging…".into(),
    });
    let text = frame_text(&mut state, 180, 10);
    assert!(text.contains("deepseek-chat"), "{text}");
    assert!(text.contains("proj"), "{text}");
    // The parent footer retains its own context and measured usage.
    assert!(text.contains("ctx:"), "{text}");
    assert!(text.contains("in:100 out:20"), "{text}");
    let flat = heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(
        flat.contains("context: ~120 of 1000 (12 percent)"),
        "{flat}"
    );
    assert!(
        !text.contains("last output"),
        "output counts are labelled only after the turn settles: {text}"
    );
    assert!(text.contains("Working…"), "{text}");
    assert!(!text.contains("/ commands"), "{text}");
}

#[test]
fn context_meter_only_uses_an_explicit_current_context_measurement() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/proj"));
    state.context_window = Some(1000);
    state.context_warn_ratio = 0.7;
    // Prompt usage can be cumulative across model calls, so it must not be
    // presented as the live context size without an explicit measurement.
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "stop".to_owned(),
        usage: Some(heycode_core::TokenUsage {
            prompt_tokens: 650,
            completion_tokens: 20,
        }),
        context_tokens: None,
    });
    assert_eq!(state.context_tokens, None);
    let text = frame_text(&mut state, 180, 10);
    assert!(!text.contains("context 650"), "{text}");
    let flat = heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(flat.contains("context: unavailable"), "{flat}");

    // An explicit estimate still wins, and a turn without usage keeps the last value.
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "stop".to_owned(),
        usage: None,
        context_tokens: Some(300),
    });
    assert_eq!(state.context_tokens, Some(300));
    assert!(
        state.context_tokens_estimated,
        "the native estimate must stay visibly an estimate"
    );
    let text = frame_text(&mut state, 180, 10);
    assert!(
        text.contains("ctx:~300/1k"),
        "the parent footer preserves the explicit estimate: {text}"
    );
    let flat = heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(flat.contains("context: ~300"), "{flat}");
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "error".to_owned(),
        usage: None,
        context_tokens: None,
    });
    assert_eq!(state.context_tokens, Some(300));
}

#[test]
fn assistant_markdown_renders_inside_transcript() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state
        .items
        .push(Item::Assistant("# Heading\ncode `x`".into()));
    let text = frame_text(&mut state, 40, 10);
    assert!(
        text.contains("● Heading") && !text.contains("## Heading"),
        "{text}"
    );
    assert!(text.contains("code x"), "{text}");
}

#[test]
fn scroll_from_bottom_offsets_the_viewport() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    for i in 0..30 {
        state.items.push(Item::Info(format!("line-{i:02}")));
    }
    let bottom = frame_text(&mut state, 30, 8);
    assert!(
        bottom.contains("line-29"),
        "follow mode shows newest: {bottom}"
    );

    state.scroll_from_bottom = 500; // far past the top clamps to history start
    let top = frame_text(&mut state, 30, 8);
    assert!(top.contains("line-00"), "scrolled up shows history: {top}");
    assert!(!top.contains("line-29"), "{top}");
}

#[test]
fn brief_completed_reasoning_stays_hidden_and_expands_with_ctrl_r() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    for line in ["step one", "step two", "step three"] {
        state.apply(&heycode_agent::UiEvent::ReasoningDelta {
            text: format!("{line}\n"),
        });
    }
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    });

    let collapsed = frame_text(&mut state, 60, 12);
    assert!(!collapsed.contains("Thought for"), "{collapsed}");
    assert!(
        !collapsed.contains("step one"),
        "hidden while collapsed: {collapsed}"
    );

    let ev = crossterm::event::Event::Key(crossterm::event::KeyEvent {
        code: crossterm::event::KeyCode::Char('r'),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
    state.handle_terminal_event(&ev);
    let expanded = frame_text(&mut state, 60, 14);
    assert!(
        expanded.contains("step one"),
        "expanded shows text: {expanded}"
    );
}

#[test]
fn todo_write_renders_checklist_glyphs() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "todo_write".into(),
        args: serde_json::json!({}),
        result: Some((
            true,
            serde_json::json!([
                {"content": "done thing",  "status": "completed"},
                {"content": "active thing","status": "in_progress"},
                {"content": "later thing", "status": "pending"}
            ]),
        )),
        untrusted_content: None,
    });
    let text = frame_text(&mut state, 60, 12);
    assert!(text.contains("Plan updated · 1/3 complete"), "{text}");
    assert!(text.contains("active thing"), "{text}");
    assert!(
        !text.contains("later thing"),
        "collapsed cards stay compact: {text}"
    );
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    assert!(frame_text(&mut state, 80, 20).contains("later thing"));
}

#[test]
fn question_card_shows_descriptions_custom_choice_and_no_competing_composer() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.pending_runtime_question = Some(PendingRuntimeQuestionView {
        mode: heycode_core::QuestionMode::SingleChoice,
        progress: (1, 1),
        selected_choices: Default::default(),
        request_id: "question-1".to_owned(),
        header: Some("Intent".to_owned()),
        prompt: "Which route should I use?".to_owned(),
        choices: vec!["Amber".to_owned(), "Blue".to_owned()],
        choice_descriptions: vec![
            Some("Use the stable route".to_owned()),
            Some("Try the experimental route".to_owned()),
        ],
        selection: 0,
        input: String::new(),
    });

    let text = frame_text(&mut state, 80, 24);
    assert!(text.contains("Intent"), "{text}");
    assert!(text.contains("Which route should I use?"), "{text}");
    assert!(text.contains("Use the stable route"), "{text}");
    assert!(text.contains("Try the experimental route"), "{text}");
    assert!(text.contains("Type something."), "{text}");
    assert!(!text.contains("Message"), "{text}");
}

#[test]
fn ask_user_question_tool_result_is_a_plain_language_transcript() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "ask_user_question".to_owned(),
        args: serde_json::json!({"question":"Which route?"}),
        result: Some((true, serde_json::json!({"answer":"Amber"}))),
        untrusted_content: None,
    });

    let text = frame_text(&mut state, 80, 16);
    assert!(text.contains("ask_user_question(Which route?)"), "{text}");
    assert!(text.contains("You answered: Amber"), "{text}");
    assert!(!text.contains("{\"answer\""), "{text}");
}

#[test]
fn edit_diff_renders_plus_minus_lines() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::Tool {
        view: heycode_tui::app::ToolViewState {
            expanded: true,
            ..Default::default()
        },
        call_id: None,
        name: "edit".into(),
        args: serde_json::json!({"path": "src/x.rs"}),
        result: Some((
            true,
            serde_json::json!({
                "message": "Edited src/x.rs (1 replacement(s))",
                "diff": "-old line\n+new line"
            }),
        )),
        untrusted_content: None,
    });
    let text = frame_text(&mut state, 80, 24);
    assert!(text.contains("-old line"), "{text}");
    assert!(text.contains("+new line"), "{text}");
}

#[test]
fn enter_while_busy_shows_hint_not_silent_drop() {
    use heycode_tui::app::{AppEvent, AppState as S};
    let _ = AppEvent::Ui(heycode_agent::UiEvent::Info {
        text: String::new(),
    }); // touch export
    let mut state = S::new("m", std::path::PathBuf::from("/p"));
    // simulate busy
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.pending_send.is_none(), "must not enqueue while busy");
}

#[test]
fn interrupt_handle_remains_reusable_across_settled_turns() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let sink = hits.clone();
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.interrupt_fn = Some(Box::new(move || {
        sink.fetch_add(1, Ordering::SeqCst);
    }));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "aborted".to_owned(),
        usage: None,
        context_tokens: None,
    });
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 2 });
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Esc));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(state.interrupt_fn.is_some());
}

#[test]
fn session_browser_frame_exposes_lineage_state_markers_and_actions() {
    let root = tempfile::tempdir().unwrap();
    let service = std::sync::Arc::new(heycode_session::SessionQueryService::local(
        root.path().to_path_buf(),
    ));
    let metadata = heycode_session::SessionCreationMetadata::new(
        Some(std::path::PathBuf::from("/work/project")),
        Some("native".to_owned()),
        heycode_session::SessionSource::Interactive,
    )
    .unwrap();
    let mut current = service
        .create(
            &heycode_session::SessionCreateRequest::new(metadata.clone())
                .with_title(heycode_session::SessionTitle::new("Current work").unwrap()),
        )
        .unwrap();
    current
        .append(heycode_session::SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    current
        .append(heycode_session::SessionEventKind::TurnEnd {
            turn: 1,
            reason: heycode_session::TurnEndReason::Stop,
        })
        .unwrap();
    let child = service
        .fork(current.id(), heycode_session::ForkBoundary::Latest)
        .unwrap();
    let child_id = child.id().clone();
    drop(child);
    service
        .archive(&child_id, heycode_session::SessionArchiveAction::Archive)
        .unwrap();

    let mut state = AppState::new("m", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service,
        std::sync::Arc::new(std::sync::Mutex::new(current)),
        metadata,
    );
    state.open_session_browser();
    state
        .session_browser_mut()
        .unwrap()
        .set_storage(heycode_tui::session_browser::SessionStorageView::All);
    state.refresh_session_browser();
    assert_eq!(state.session_browser().unwrap().rows().len(), 1);
    assert_eq!(
        state.session_browser().unwrap().rows()[0].summary().id(),
        &child_id
    );
    let text = frame_text(&mut state, 120, 32);
    assert!(text.contains("Resume any saved session"), "{text}");
    assert!(text.contains("Current work"), "{text}");
    assert!(!text.contains(" · current"), "{text}");
    assert!(!text.contains(" · latest"), "{text}");
    assert!(text.contains("archived"), "{text}");
    assert!(text.contains("fork · idle · native"), "{text}");
    assert!(text.contains("/work/project"), "{text}");
    assert!(text.contains("branch of"), "{text}");
    assert!(text.contains("PgUp/PgDn"), "{text}");
    assert!(text.contains("Ctrl+R to rename"), "{text}");
    assert!(text.contains("n/f/a/d/e actions"), "{text}");
}

fn settled_bash_card(index: usize, body: &str) -> Item {
    Item::Tool {
        view: Default::default(),
        call_id: None,
        name: "bash".to_owned(),
        args: serde_json::json!({ "command": format!("cmd-{index}") }),
        result: Some((
            true,
            serde_json::Value::String(format!("{body}\n[exit code: 0]")),
        )),
        untrusted_content: None,
    }
}

#[test]
fn follow_mode_shows_the_newest_answer_after_settled_tool_cards() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.items.push(Item::User("run the tests".into()));
    state
        .items
        .push(Item::Assistant("Running them now.".into()));
    for index in 0..3 {
        let body = (0..9)
            .map(|line| format!("out-{index}-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.items.push(settled_bash_card(index, &body));
    }
    state
        .items
        .push(Item::Assistant("All 3757 tests passed.".into()));
    assert_eq!(state.scroll_from_bottom, 0, "follow mode is the default");
    let text = frame_text(&mut state, 70, 16);
    assert!(
        text.contains("All 3757 tests passed."),
        "follow mode must show the newest item: {text}"
    );
}

/// Rows the transcript area draws, trailing blank rows trimmed.
fn transcript_rows(state: &mut AppState, w: u16, h: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal_draw(&mut term, state);
    let buffer = term.backend().buffer().clone();
    // These tall test frames retain the five-row banner. Locate the composer
    // to exclude the status and optional controls below it.
    let transcript_start = 5;
    let transcript_end = (0..h)
        .rev()
        .find(|y| {
            (0..w)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .trim_start()
                .starts_with('❯')
        })
        .expect("composer row")
        .saturating_sub(1);
    let mut rows = (transcript_start..transcript_end)
        .map(|y| {
            (0..w)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

/// Where `window` sits inside the whole rendered transcript.
fn window_start(full: &[String], window: &[String]) -> usize {
    let matches = full
        .windows(window.len())
        .enumerate()
        .filter(|(_, candidate)| *candidate == window)
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "the window must sit at exactly one place in the transcript: {matches:?}\n{window:#?}"
    );
    matches[0]
}

#[test]
fn every_transcript_item_is_reachable_by_scrolling_past_tool_cards() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    for index in 0..6 {
        let body = (0..9)
            .map(|line| format!("out-{index}-{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.items.push(settled_bash_card(index, &body));
    }
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    state.items.push(Item::Info("NEWEST-MARKER".into()));

    // A terminal tall enough for the whole transcript is the row sequence
    // every scrolled window is a slice of.
    let full = transcript_rows(&mut state, 70, 160);
    assert!(
        full.iter().any(|row| row.contains("NEWEST-MARKER")),
        "the tall frame must hold the whole transcript: {full:#?}"
    );
    assert!(full.iter().any(|row| row.contains("out-0-0")), "{full:#?}");

    let bottom = transcript_rows(&mut state, 70, 24);
    assert!(
        bottom.iter().any(|row| row.contains("NEWEST-MARKER")),
        "follow mode must show the newest item: {bottom:#?}"
    );
    let bottom_start = window_start(&full, &bottom);
    assert!(
        bottom_start > 0,
        "the transcript must overflow the window for this to be a scroll test"
    );

    // One offset unit is one rendered row until the oldest row is on screen,
    // and no offset is a no-op before that.
    let mut starts = Vec::new();
    for offset in 0..=(bottom_start + 6) {
        state.scroll_from_bottom = offset;
        let window = transcript_rows(&mut state, 70, 24);
        let start = window_start(&full, &window);
        assert_eq!(
            start,
            bottom_start.saturating_sub(offset),
            "offset {offset} moved the window to row {start}"
        );
        starts.push(start);
    }
    assert!(
        starts.windows(2).all(|pair| pair[1] <= pair[0]),
        "scrolling up must never move the window down: {starts:?}"
    );
    assert_eq!(starts.last().copied(), Some(0), "{starts:?}");

    state.scroll_from_bottom = 0;
    let follow_again = transcript_rows(&mut state, 70, 24);
    assert!(
        follow_again.iter().any(|row| row.contains("NEWEST-MARKER")),
        "returning to offset 0 must show the newest item again: {follow_again:#?}"
    );

    state.scroll_from_bottom = usize::MAX;
    let oldest = transcript_rows(&mut state, 70, 24);
    assert_eq!(window_start(&full, &oldest), 0, "{oldest:#?}");
    assert!(
        oldest.iter().any(|row| row.contains("out-0-0")),
        "the oldest row must be reachable at the oldest-position sentinel: {oldest:#?}"
    );
}

#[test]
fn control_bytes_from_untrusted_items_never_reach_terminal_cells() {
    let payload = "A\u{1b}[31mRED\u{1b}[0m B\u{7}C\u{9b}5nD";
    let samples = vec![
        Item::User(payload.to_owned()),
        Item::Info(format!("hook contribution\n{payload}")),
        Item::Error(payload.to_owned()),
        Item::Citation {
            url: format!("https://example.invalid/{payload}"),
            title: Some(payload.to_owned()),
            cited_text: Some(payload.to_owned()),
            start_index: Some(2),
            end_index: Some(8),
        },
        Item::Workflow {
            action: payload.to_owned(),
            summary: payload.to_owned(),
        },
        Item::Schedule {
            action: payload.to_owned(),
            summary: payload.to_owned(),
        },
        settled_bash_card(0, payload),
        Item::Tool {
            view: heycode_tui::app::ToolViewState {
                expanded: true,
                ..Default::default()
            },
            call_id: None,
            name: "web_fetch".to_owned(),
            args: serde_json::json!({ "url": payload }),
            result: Some((true, serde_json::Value::String(payload.to_owned()))),
            untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
        },
        Item::Assistant(payload.to_owned()),
    ];
    for sample in samples {
        let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
        state.items.push(sample.clone());
        if let Some(Item::Tool { view, .. }) = state.items.last_mut() {
            view.expanded = true;
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal_draw(&mut terminal, &mut state);
        let cells = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect::<Vec<_>>();
        for cell in &cells {
            assert!(
                cell.chars().all(|character| !character.is_control()),
                "control byte reached a terminal cell for {sample:?}: {cell:?}"
            );
        }
        if matches!(sample, Item::Schedule { .. }) {
            assert!(
                !cells.join("").contains("RED"),
                "internal schedule diagnostics stay out of the conversation"
            );
        } else {
            assert!(
                cells.join("").contains("RED"),
                "sanitizing must keep the text: {sample:?}"
            );
        }
    }
}

fn error_rows(state: &AppState) -> Vec<String> {
    state
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Error(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A failed turn shows ONE error row carrying the cause, whichever order the
/// specific error and the generic turn settlement arrive in.
#[test]
fn a_failed_turn_renders_exactly_one_error_row_with_its_cause() {
    let cause = "app-server is unavailable: Codex CLI is unavailable";
    let finished = heycode_agent::UiEvent::TurnFinished {
        reason: "error".to_owned(),
        usage: None,
        context_tokens: None,
    };

    // Specific error first, then the generic settlement.
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&heycode_agent::UiEvent::Error {
        message: cause.to_owned(),
    });
    state.apply(&finished);
    assert_eq!(error_rows(&state), vec![cause.to_owned()]);

    // Generic settlement first, then the specific error replaces it.
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&finished);
    state.apply(&heycode_agent::UiEvent::Error {
        message: cause.to_owned(),
    });
    assert_eq!(error_rows(&state), vec![cause.to_owned()]);

    // A failure with no specific cause still gets one row.
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&finished);
    assert_eq!(
        error_rows(&state),
        vec!["turn ended with an error".to_owned()]
    );

    // The next turn starts clean: its own error is its own row.
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 2 });
    state.apply(&heycode_agent::UiEvent::Error {
        message: "second".to_owned(),
    });
    state.apply(&finished);
    assert_eq!(
        error_rows(&state),
        vec!["turn ended with an error".to_owned(), "second".to_owned()]
    );
}

#[test]
fn a_replayed_edit_result_shows_its_diff_lines_like_the_live_card() {
    use heycode_session::{CURRENT_SESSION_LOG_VERSION, SessionEvent, SessionEventKind};
    let event = |seq: u64, kind: SessionEventKind| SessionEvent {
        v: CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 1_730_000_000_000,
        kind,
    };
    let call_id = heycode_core::CallId::from_raw("call-edit-1");
    let events = vec![
        event(0, SessionEventKind::TurnStart { turn: 1 }),
        event(
            1,
            SessionEventKind::ToolCall {
                turn: 1,
                call_id: call_id.clone(),
                name: "edit".to_owned(),
                args: serde_json::json!({"path": "src/main.rs", "old": "a", "new": "b"}),
            },
        ),
        event(
            2,
            SessionEventKind::ToolResult {
                call_id,
                // What the edit tool committed: its JSON result as text.
                content:
                    "{\"diff\":\"-let a = 1;\\n+let b = 1;\",\"message\":\"edited src/main.rs\"}"
                        .to_owned(),
                is_error: false,
                untrusted_content: None,
            },
        ),
    ];
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.replay(&events);
    for item in &mut state.items {
        if let Item::Tool { view, .. } = item {
            view.expanded = true;
        }
    }
    let text = frame_text(&mut state, 80, 30);
    assert!(text.contains("-let a = 1;"), "{text}");
    assert!(text.contains("+let b = 1;"), "{text}");
    assert!(text.contains("Update(src/main.rs)"), "{text}");
    assert!(
        !text.contains("{\"diff\""),
        "the raw JSON blob is never what the user sees: {text}"
    );
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    for item in &mut state.items {
        if let Item::Tool { view, .. } = item {
            view.expanded = true;
        }
    }
    let text = frame_text(&mut state, 100, 24);
    assert!(text.contains("edited src/main.rs"), "{text}");
}

#[test]
fn working_activity_follows_the_transcript_and_tracks_real_turn_phases() {
    let mut state = AppState::new("opus", std::path::PathBuf::from("/workspace"));
    state.apply(&heycode_agent::UiEvent::UserEcho {
        text: "Investigate this".into(),
    });
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    let text = frame_text(&mut state, 110, 30);
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal_draw(&mut terminal, &mut state);
    let lines = (0..30)
        .map(|y| {
            (0..110)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let activity = lines
        .iter()
        .position(|line| line.contains("Working…"))
        .unwrap();
    let prompt = lines.iter().rposition(|line| line.contains('❯')).unwrap();
    let user = lines
        .iter()
        .position(|line| line.contains("Investigate this"))
        .unwrap();
    assert!(activity > user && activity + 2 == prompt, "{text}");
    assert!(!text.contains("Waiting for the model"), "{text}");
    state.apply(&heycode_agent::UiEvent::ReasoningDelta {
        text: "Considering the code".into(),
    });
    state.reasoning_effort = Some("high".into());
    let thinking = frame_text(&mut state, 110, 30);
    assert!(
        thinking.contains("Thinking…") && thinking.contains("thinking with high effort"),
        "{thinking}"
    );
    assert!(thinking.contains("Considering the code"), "{thinking}");
    state.apply(&heycode_agent::UiEvent::AssistantDelta {
        text: "Here is what I found".into(),
    });
    assert!(frame_text(&mut state, 110, 30).contains("Responding…"));
}

#[test]
fn successful_response_has_no_completion_or_reasoning_summary() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&heycode_agent::UiEvent::ReasoningDelta {
        text: "internal deliberation".into(),
    });
    state.apply(&heycode_agent::UiEvent::AssistantDelta {
        text: "Hello there.".into(),
    });
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    });
    let rendered = frame_text(&mut state, 100, 24);
    assert!(rendered.contains("Hello there."));
    for unwanted in [
        "thought",
        "internal deliberation",
        "Completed in",
        "provider state",
        "ChatAssistantMessage",
    ] {
        assert!(!rendered.contains(unwanted), "{rendered}");
    }
}

#[test]
fn opaque_reasoning_reports_activity_without_inventing_text_or_effort() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    state.apply(&heycode_agent::UiEvent::ReasoningDelta {
        text: String::new(),
    });
    let rendered = frame_text(&mut state, 110, 24);
    assert!(rendered.contains("Working…"), "{rendered}");
    assert!(
        !rendered.contains("Provider has not supplied readable thinking text")
            && !rendered.contains("ctrl+r"),
        "{rendered}"
    );
    assert!(!rendered.contains("high effort"), "{rendered}");
}

#[test]
fn actionable_error_wraps_to_keep_the_recovery_instruction_visible() {
    let mut state = AppState::new("m", "/p".into());
    state.apply(&heycode_agent::UiEvent::Error { message: "This model requires 18+ age confirmation. Complete it at https://openrouter.ai/settings/preferences, or choose another model with /model.".into() });
    let rendered = frame_text(&mut state, 50, 24);
    assert!(rendered.contains("/model"), "{rendered}");
    assert_eq!(rendered.matches('✗').count(), 1, "{rendered}");
}

#[test]
fn workspace_status_marks_a_dirty_checkout_and_leaves_a_clean_one_unmarked() {
    use heycode_tui::workspace_context::{
        PullRequestContext, WorkspaceContext, WorkspaceContextState,
    };

    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.set_workspace_context(WorkspaceContextState::Ready(WorkspaceContext {
        branch: "master".to_owned(),
        dirty: true,
        pull_request: PullRequestContext::None,
    }));
    let text = frame_text(&mut state, 120, 16);
    assert!(
        text.contains("master *"),
        "the footer must preserve the dirty branch: {text}"
    );
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(
        flat.contains("branch: master (uncommitted changes)"),
        "{flat}"
    );

    state.set_workspace_context(WorkspaceContextState::Ready(WorkspaceContext {
        branch: "master".to_owned(),
        dirty: false,
        pull_request: PullRequestContext::None,
    }));
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("branch: master"), "{flat}");
    assert!(!flat.contains("uncommitted"), "{flat}");
    let clean = frame_text(&mut state, 120, 16);
    assert!(
        clean.contains("master") && !clean.contains("master *"),
        "{clean}"
    );
}

#[test]
fn status_reports_zeroed_token_usage_before_the_first_turn() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    let text = frame_text(&mut state, 120, 16);
    assert!(
        !text.contains("in:0 out:0"),
        "token counts left the one-line idle footer: {text}"
    );
    let flat =
        heycode_tui::app::accessibility::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(
        flat.contains("tokens: in 0 out 0"),
        "an untouched session still accounts for its tokens: {flat}"
    );
}

/// Foreground colour of the approval-mode label on the bottom controls row.
fn mode_label_color(state: &mut AppState, w: u16, h: u16) -> ratatui::style::Color {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal_draw(&mut term, state);
    let buffer = term.backend().buffer().clone();
    let row = h - 1;
    (0..w)
        .map(|x| buffer.cell((x, row)).unwrap())
        .find(|cell| cell.symbol().trim() != "")
        .unwrap_or_else(|| panic!("controls row is blank"))
        .fg
}

#[test]
fn each_approval_mode_carries_its_own_colour() {
    let modes = ["default", "accepted_edits", "plan", "full_access"];
    let mut seen: Vec<(&str, ratatui::style::Color)> = Vec::new();
    for mode in modes {
        let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
        state.permission = mode.to_owned();
        let color = mode_label_color(&mut state, 100, 20);
        assert!(
            !seen.iter().any(|(_, seen)| *seen == color),
            "`{mode}` reuses a colour already spent on {seen:?}"
        );
        seen.push((mode, color));
    }
}

#[test]
fn detailed_transcript_help_is_visible_and_keeps_the_unsent_draft_hidden() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.handle_terminal_event(&Event::Paste("unsent private draft".to_owned()));
    state.items.push(Item::Assistant("retained answer".into()));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
    )));
    state.handle_terminal_event(&key(KeyCode::Char('?')));
    let text = frame_text(&mut state, 60, 24);
    assert!(text.contains("Showing detailed transcript"));
    assert!(text.contains("esc close"));
    assert!(text.contains("oldest/newest"));
    assert!(!text.contains("unsent private draft"));
    state.handle_terminal_event(&key(KeyCode::Backspace));
    assert!(!frame_text(&mut state, 60, 24).contains("oldest/newest"));
    state.handle_terminal_event(&key(KeyCode::Esc));
    assert!(frame_text(&mut state, 60, 24).contains("unsent private draft"));
}
