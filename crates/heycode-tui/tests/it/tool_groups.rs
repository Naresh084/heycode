#![allow(clippy::unwrap_used)]

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use heycode_tui::app::{AppState, Item, ReasoningView, ToolViewState};
use serde_json::json;

fn tool(name: &str, args: serde_json::Value, result: Option<(bool, serde_json::Value)>) -> Item {
    Item::Tool {
        name: name.into(),
        args,
        result,
        call_id: None,
        untrusted_content: None,
        view: ToolViewState::default(),
    }
}

fn frame(state: &mut AppState, width: u16, height: u16) -> Vec<String> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect()
        })
        .collect()
}

#[test]
fn routine_calls_group_and_failures_stay_visible_while_running_tools_stay_quiet() {
    let mut state = AppState::new("m", "/project".into());
    state.items = vec![
        Item::User("Inspect the project".into()),
        tool(
            "read",
            json!({"path":"a.rs"}),
            Some((true, json!("FILE_A_CONTENT"))),
        ),
        Item::Reasoning {
            text: "supplied reasoning".into(),
            done: true,
            view: ReasoningView::default(),
        },
        tool(
            "mcp__heycode__read",
            json!({"path":"b.rs"}),
            Some((true, json!("FILE_B_CONTENT"))),
        ),
        tool(
            "grep",
            json!({"pattern":"target"}),
            Some((true, json!("a.rs:1:target"))),
        ),
        tool(
            "bash",
            json!({"command":"check"}),
            Some((true, json!("SHELL_OUTPUT\n[exit code: 0]"))),
        ),
        Item::Assistant("I found the cause.".into()),
        tool(
            "bash",
            json!({"command":"broken"}),
            Some((false, json!("VISIBLE_FAILURE\n[exit code: 1]"))),
        ),
        tool("read", json!({"path":"pending.rs"}), None),
    ];
    let lines = frame(&mut state, 120, 30);
    let text = lines.join("\n");
    assert!(
        text.contains("Read 2 files, searched for 1 pattern, ran 1 shell command"),
        "{text}"
    );
    assert!(!text.contains("FILE_A_CONTENT") && !text.contains("supplied reasoning"));
    assert!(text.contains("I found the cause.") && text.contains("VISIBLE_FAILURE"));
    assert!(!text.contains("read(pending.rs)"));
    let row = lines
        .iter()
        .position(|line| line.contains("Read 2 files"))
        .unwrap() as u16;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind,
            column: 3,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    let expanded = frame(&mut state, 120, 60).join("\n");
    for content in [
        "FILE_A_CONTENT",
        "FILE_B_CONTENT",
        "SHELL_OUTPUT",
        "VISIBLE_FAILURE",
    ] {
        assert!(expanded.contains(content), "{expanded}");
    }
    let accessible = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(accessible.contains("Expanded tool group") && accessible.contains("FILE_B_CONTENT"));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    let collapsed = frame(&mut state, 60, 30).join("\n");
    assert!(!collapsed.contains("FILE_A_CONTENT"));
    assert!(collapsed.contains("VISIBLE_FAILURE"));
    assert_eq!(
        state.items.len(),
        9,
        "grouping must not replace durable calls"
    );
}

#[test]
fn nonzero_shell_status_is_not_hidden_even_when_the_tool_transport_succeeded() {
    let mut state = AppState::new("m", "/project".into());
    state.items.push(tool(
        "bash",
        json!({"command":"false"}),
        Some((true, json!("[exit code: 1]"))),
    ));
    let text = frame(&mut state, 80, 16).join("\n");
    assert!(!text.contains("Ran 1 shell command"));
    assert!(text.contains("Error: Exit code 1"), "{text}");
}

#[test]
fn search_and_edit_summaries_use_call_facts_and_do_not_group_across_an_answer() {
    let mut state = AppState::new("m", "/project".into());
    state.items = vec![
        tool(
            "grep",
            json!({"pattern":"first"}),
            Some((true, json!("match"))),
        ),
        tool(
            "glob",
            json!({"pattern":"*.rs"}),
            Some((true, json!("a.rs"))),
        ),
        Item::Assistant("I will update the implementation.".into()),
        tool(
            "edit",
            json!({"path":"a.rs"}),
            Some((
                true,
                json!({"diff":"--- a/a.rs\n+++ b/a.rs\n-old\n+new\n+extra"}),
            )),
        ),
        tool(
            "bash",
            json!({"command":"check"}),
            Some((true, json!("ok"))),
        ),
    ];
    let text = frame(&mut state, 100, 24).join("\n");
    assert!(text.contains("Searched for 2 patterns"), "{text}");
    assert!(
        text.contains("Update(a.rs)") && text.contains("Ran 1 shell command"),
        "{text}"
    );
    // Grouped summaries are bare indented rows like the source; no disclosure glyph.
    assert_eq!(text.matches("▸").count(), 0, "{text}");
    assert!(text.contains("⏺ Update(a.rs)"), "{text}");
}

#[test]
fn a_late_result_outside_the_recent_window_reveals_its_completed_group() {
    let mut state = AppState::new("m", "/project".into());
    state.apply(&heycode_agent::UiEvent::ToolStarted {
        name: "read".into(),
        args: json!({"path":"slow.rs"}),
    });
    for index in 0..40 {
        state
            .items
            .push(Item::Assistant(format!("Progress {index}")));
    }
    let before = frame(&mut state, 90, 120).join("\n");
    assert!(!before.contains("read(slow.rs)"));
    state.apply(&heycode_agent::UiEvent::ToolFinished {
        name: "read".into(),
        ok: true,
        value: json!("retained source"),
        untrusted_content: None,
    });
    let after = frame(&mut state, 90, 120).join("\n");
    assert!(after.contains("Read 1 file"));
    assert!(!after.contains("read(slow.rs) · running"));
}

#[test]
fn replay_groups_original_calls_without_rewriting_their_results() {
    use heycode_session::{CURRENT_SESSION_LOG_VERSION, SessionEvent, SessionEventKind};
    let mut events = Vec::new();
    for index in 0..2 {
        let call_id = heycode_core::CallId::from_raw(format!("read-{index}"));
        events.push(SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: index * 2,
            time_ms: 0,
            kind: SessionEventKind::ToolCall {
                turn: 1,
                call_id: call_id.clone(),
                name: "read".into(),
                args: json!({"path":format!("file-{index}.rs")}),
            },
        });
        events.push(SessionEvent {
            v: CURRENT_SESSION_LOG_VERSION,
            seq: index * 2 + 1,
            time_ms: 0,
            kind: SessionEventKind::ToolResult {
                call_id,
                content: format!("source-{index}"),
                is_error: false,
                untrusted_content: None,
            },
        });
    }
    let mut state = AppState::new("m", "/project".into());
    state.replay(&events);
    let text = frame(&mut state, 90, 24).join("\n");
    assert!(text.contains("Read 2 files"), "{text}");
    assert!(!text.contains("source-0"));
    assert_eq!(
        state
            .items
            .iter()
            .filter(|item| matches!(
                item,
                Item::Tool {
                    result: Some((true, _)),
                    ..
                }
            ))
            .count(),
        2
    );
    let flat = heycode_tui::ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(flat.contains("Tool group: Read 2 files"));
    assert!(!flat.contains("source-0"));
}

#[test]
fn paginated_read_displays_content_and_remaining_extent_in_expanded_view() {
    let mut state = AppState::new("m", "/project".into());
    let mut read = tool(
        "read",
        json!({"path":"large.rs"}),
        Some((
            true,
            json!({
                "content":"   1\tFIRST_LINE", "lines_returned":1,"total_lines":437,
                "lines_remaining":436,"total_bytes":12000,"truncated":true
            }),
        )),
    );
    if let Item::Tool { view, .. } = &mut read {
        view.expanded = true;
    }
    state.items = vec![read];
    let text = frame(&mut state, 120, 30).join("\n");
    assert!(text.contains("FIRST_LINE"), "{text}");
    assert!(text.contains("437 total · 436 remaining"), "{text}");
    assert!(text.contains("More content is available"), "{text}");
}

#[test]
fn partial_batch_reads_and_edit_previews_do_not_claim_successful_group_completion() {
    let mut state = AppState::new("m", "/project".into());
    state.items = vec![
        tool(
            "read_many",
            json!({"files":[{"path":"a"},{"path":"b"}]}),
            Some((
                true,
                json!({"files":[
                    {"path":"a","status":"read","content":"A","total_lines":1,"lines_remaining":0},
                    {"path":"b","status":"deferred","reason":"Shared budget exhausted"}
                ]}),
            )),
        ),
        tool(
            "multi_edit",
            json!({"path":"a","dry_run":true}),
            Some((
                true,
                json!({
                    "dry_run":true,"changed":true,"message":"Previewed 1 edit","diff":"-A\n+B"
                }),
            )),
        ),
    ];
    let text = frame(&mut state, 120, 30).join("\n");
    assert!(text.contains("1 deferred"), "{text}");
    assert!(!text.contains("Changed 1 file"), "{text}");
    assert!(text.contains("Update(a) · preview"), "{text}");
    assert!(
        text.contains("Previewed 1 edit") && text.contains("+B"),
        "{text}"
    );
    for item in &state.items {
        if let Item::Tool { view, .. } = item {
            assert!(view.group_summary.is_none());
        }
    }
}

#[test]
fn partial_search_keeps_coverage_notice_visible_and_out_of_success_groups() {
    let mut state = AppState::new("m", "/project".into());
    state.items = vec![tool(
        "grep",
        json!({"pattern":"needle"}),
        Some((
            true,
            json!(
                "a:2: needle\n(Partial search; match count is a lower bound: 1 oversized lines skipped. Narrow the path/include or read the matching file.)"
            ),
        )),
    )];
    let text = frame(&mut state, 100, 30).join("\n");
    assert!(text.contains("Found at least 1 line"), "{text}");
    assert!(
        text.contains("Partial search; match count is a lower bound"),
        "{text}"
    );
    assert!(!text.contains("Searched for 1 pattern"), "{text}");
}

#[test]
fn structured_update_renders_numbered_context_in_compact_and_expanded_views() {
    for expanded in [false, true] {
        let mut state = AppState::new("m", "/project".into());
        let mut item = tool(
            "multi_edit",
            json!({"path":"sample.txt"}),
            Some((
                true,
                json!({
                    "changed":true,"dry_run":false,"diff_previews":[{
                        "inserted_lines":1,"removed_lines":1,"truncated":false,
                        "rows":[{"kind":"context","line":1,"text":"alpha"},
                            {"kind":"removed","line":2,"text":"beta"},
                            {"kind":"added","line":2,"text":"delta"},
                            {"kind":"context","line":3,"text":"gamma"}]
                    }]
                }),
            )),
        );
        if let Item::Tool { view, .. } = &mut item {
            view.expanded = expanded;
        }
        state.items = vec![item];
        let text = frame(&mut state, 100, 30).join("\n");
        assert!(text.contains("⏺ Update(sample.txt)"), "{text}");
        assert!(text.contains("Added 1 line, removed 1 line"), "{text}");
        for row in ["1  alpha", "2 -beta", "2 +delta", "3  gamma"] {
            assert!(text.contains(row), "{text}");
        }
    }
}

#[test]
fn command_information_wraps_long_descriptions_without_losing_their_tail() {
    let mut state = AppState::new("m", "/project".into());
    state.items = vec![Item::Info("Read parameters: use expected_revision to reject a continuation after the source file changed; REQUIRED_TAIL stays visible.".into())];
    let text = frame(&mut state, 50, 30).join("\n");
    assert!(text.contains("REQUIRED_TAIL"), "{text}");
    assert!(text.contains("expected_revision"), "{text}");
}

#[test]
fn replayed_context_uses_retained_text_and_marks_continuation_as_uncounted() {
    use heycode_session::{SessionEvent, SessionEventKind as K};
    let mut state = AppState::new("m", "/project".into());
    let budget = heycode_llm::context_budget(
        "p".into(),
        "m".into(),
        &heycode_llm::EnvelopeTotal::Exact(100),
        Some(100_000),
        0,
        1000,
        0.8,
        true,
    );
    let mut context =
        heycode_session::RequestContextSnapshot::new(None, None, None, None, 1).unwrap();
    context.budget = Some(Box::new(budget));
    let request_id = heycode_core::RequestId::from_raw("request");
    let mut events = vec![
        SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 1,
            time_ms: 1,
            kind: K::RequestContext {
                request_id: request_id.clone(),
                context,
            },
        },
        SessionEvent {
            v: heycode_session::CURRENT_SESSION_LOG_VERSION,
            seq: 2,
            time_ms: 2,
            kind: K::AssistantMessage {
                turn: 1,
                step: 1,
                content: "hello".into(),
                reasoning: Some("hidden reasoning".repeat(1000)),
                tool_calls: None,
                usage: Some(heycode_core::TokenUsage {
                    prompt_tokens: 100,
                    completion_tokens: 50_000,
                }),
            },
        },
    ];
    state.replay(&events);
    assert_eq!(state.context_tokens, Some(102));
    assert_eq!(state.usage.unwrap().completion_tokens, 50_000);
    events.push(SessionEvent { v:heycode_session::CURRENT_SESSION_LOG_VERSION,seq:3,time_ms:3,kind:K::AssistantProviderItem {
        turn:1,step:1,request_id,output_index:0,item:Box::new(heycode_core::ProviderStateItem::new("p","m",heycode_core::ProviderProtocol::OpenAiChatCompletions,heycode_core::ProviderStateKind::ChatAssistantMessage,
            json!({"role":"assistant","content":"hello","reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]})).unwrap()),
    } });
    let mut resumed = AppState::new("m", "/project".into());
    resumed.replay(&events);
    assert_eq!(resumed.context_tokens, Some(102));
    assert_eq!(
        resumed.context_budget.unwrap().confidence,
        heycode_llm::ContextConfidence::AtLeast
    );
}

#[test]
fn aborted_partial_reasoning_is_interrupted_live_and_on_replay_without_relabeling_completed_phases()
{
    use heycode_session::{SessionEvent, SessionEventKind as K, TurnEndReason};
    for content in ["", "Completed answer"] {
        let events = vec![
            SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq: 1,
                time_ms: 1,
                kind: K::AssistantMessage {
                    turn: 1,
                    step: 1,
                    content: content.into(),
                    reasoning: Some("Readable reasoning".into()),
                    tool_calls: None,
                    usage: None,
                },
            },
            SessionEvent {
                v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                seq: 2,
                time_ms: 2,
                kind: K::TurnEnd {
                    turn: 1,
                    reason: TurnEndReason::Aborted,
                },
            },
        ];
        for live in [true, false] {
            let mut state = AppState::new("m", "/project".into());
            if live {
                for event in &events {
                    state.apply_session_event(event);
                }
            } else {
                state.replay(&events);
            }
            let view = state
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Reasoning { view, .. } => Some(view),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                view.interrupted,
                content.is_empty(),
                "live={live}, content={content}"
            );
            let text = frame(&mut state, 100, 30).join("\n");
            assert_eq!(
                text.contains("Thinking interrupted"),
                content.is_empty(),
                "{text}"
            );
        }
    }
}

#[test]
fn replay_keeps_incomplete_readable_chunks_without_duplication_or_resurrecting_activity() {
    use heycode_session::{SessionEvent, SessionEventKind as K, TurnEndReason};
    for finalized in [true, false] {
        for ended in [true, false] {
            let mut events = vec![
                SessionEvent {
                    v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                    seq: 1,
                    time_ms: 1,
                    kind: K::AssistantChunk {
                        turn: 1,
                        step: 1,
                        text: None,
                        reasoning: Some("Readable first.".into()),
                    },
                },
                SessionEvent {
                    v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                    seq: 2,
                    time_ms: 2,
                    kind: K::AssistantChunk {
                        turn: 1,
                        step: 1,
                        text: None,
                        reasoning: Some("Second.".into()),
                    },
                },
            ];
            if finalized {
                events.push(SessionEvent {
                    v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                    seq: 3,
                    time_ms: 3,
                    kind: K::AssistantMessage {
                        turn: 1,
                        step: 1,
                        content: String::new(),
                        reasoning: Some("Readable first.Second.".into()),
                        tool_calls: None,
                        usage: None,
                    },
                });
            }
            if ended {
                events.push(SessionEvent {
                    v: heycode_session::CURRENT_SESSION_LOG_VERSION,
                    seq: 4,
                    time_ms: 4,
                    kind: K::TurnEnd {
                        turn: 1,
                        reason: TurnEndReason::Error,
                    },
                });
            }
            let mut state = AppState::new("m", "/project".into());
            state.replay(&events);
            let rows: Vec<_> = state
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Reasoning { text, done, view } => Some((text, done, view)),
                    _ => None,
                })
                .collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, "Readable first.Second.");
            assert!(*rows[0].1, "restart must not show an active producer");
            assert_eq!(rows[0].2.interrupted, ended || !finalized);
            if !finalized {
                assert_eq!(
                    rows[0].2.elapsed_seconds, None,
                    "no fabricated live timer on replay"
                );
            }
        }
    }
}

#[test]
fn worktree_summaries_wrap_full_paths_and_keep_original_receipts_expandable() {
    let path = format!(
        "/workspace/{}/UNIQUE_WORKTREE_LEAF",
        "long-parent/".repeat(18)
    );
    for width in [40, 126] {
        let mut state = AppState::new("m", "/project".into());
        state.items = vec![tool(
            "exit_worktree",
            json!({}),
            Some((
                true,
                json!({
                    "status": "exited_retained", "cwd": "/original/workspace",
                    "retained_worktrees": [path], "receipt_only_marker": "ORIGINAL_RECEIPT"
                }),
            )),
        )];
        let rows = frame(&mut state, width, 35);
        let text = rows.join("\n");
        let joined = rows.iter().map(|row| row.trim()).collect::<String>();
        assert!(text.contains("Returned to workspace"), "{text}");
        assert!(text.contains("Retained worktree"), "{text}");
        assert!(joined.contains(&path), "{text}");
        assert!(!text.contains("ORIGINAL_RECEIPT"), "{text}");
        if let Item::Tool { view, .. } = &mut state.items[0] {
            view.expanded = true;
        }
        let expanded = frame(&mut state, width, 60)
            .iter()
            .map(|row| row.trim())
            .collect::<String>();
        assert!(expanded.contains("ORIGINAL_RECEIPT"), "{expanded}");
    }
}
