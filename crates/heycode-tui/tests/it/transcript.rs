//! U17 durable transcript projection and U21 virtualization budgets.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use heycode_core::{
    CallId, ProviderProtocol, ProviderStateItem, ProviderStateKind, ServerToolCall,
    ServerToolResult, ServerToolSource, UrlCitation,
};
use heycode_session::{CURRENT_SESSION_LOG_VERSION, SessionEvent, SessionEventKind};
use heycode_tui::app::{AppState, Item};
use heycode_tui::{ScreenReaderSnapshot, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 1_730_000_000_000,
        kind,
    }
}

fn finding_report(count: usize) -> heycode_session::FindingReport {
    let findings = (0..count)
        .map(|index| {
            heycode_session::ReportedFinding::new(
                format!("finding-{index}"),
                if index == 0 {
                    heycode_session::ReviewSeverity::High
                } else {
                    heycode_session::ReviewSeverity::Medium
                },
                format!("src/file-{index}.rs"),
                u32::try_from(index + 1).unwrap(),
                u32::try_from(index + 2).unwrap(),
                format!("{index:064x}"),
                format!("Finding title {index}"),
                format!("Trigger {index}"),
                format!("Failure {index}"),
                format!("Impact {index}"),
            )
            .unwrap()
        })
        .collect();
    heycode_session::FindingReport::new(
        heycode_session::FindingReportId::new("report-1").unwrap(),
        heycode_session::FindingReportSource::workspace(
            heycode_core::SessionId::from_raw("session-1"),
            9,
            0,
            None,
        )
        .unwrap(),
        findings,
    )
    .unwrap()
}

fn frame_text(state: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render::draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn structured_finding_reports_replay_as_bounded_local_only_cards() {
    let report = finding_report(33);
    let durable = event(
        0,
        SessionEventKind::ReviewChange {
            change: Box::new(
                heycode_session::ReviewChange::findings_reported(report.clone()).unwrap(),
            ),
        },
    );
    let mut state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    state.apply_session_event(&durable);
    assert!(matches!(
        state.items.as_slice(),
        [Item::FindingsReport {
            expanded: false,
            ..
        }]
    ));
    let collapsed = frame_text(&mut state, 120, 100);
    assert!(collapsed.contains("Code review(33 findings)"));
    assert!(collapsed.contains("● 1-2 Finding title 0"));
    assert!(collapsed.contains("1 additional legacy finding retained in session history"));
    assert!(!collapsed.contains("Trigger 0"));

    let expanded = Item::FindingsReport {
        report: Box::new(report),
        expanded: true,
        focused: false,
    };
    let mut expanded_state = AppState::new("model", std::path::PathBuf::from("/workspace"));
    expanded_state.items.push(expanded);
    let expanded_frame = frame_text(&mut expanded_state, 120, 400);
    assert!(expanded_frame.contains("Local report; not externally published."));
    assert!(expanded_frame.contains("trigger: Trigger 0"));
    assert!(expanded_frame.contains("failure: Failure 0"));
    assert!(expanded_frame.contains("impact: Impact 0"));
    assert!(expanded_frame.contains("1 additional legacy finding retained in session history"));
    assert!(!expanded_frame.contains("Finding title 32"));

    state.replay(&[durable]);
    assert!(matches!(
        state.items.as_slice(),
        [Item::FindingsReport { report, .. }] if report.findings().len() == 33
    ));
}

#[test]
fn finding_review_card_groups_source_and_preserves_explicit_review_dimensions() {
    let base = finding_report(1);
    let finding = base.findings()[0]
        .clone()
        .with_reference_dimensions(
            Some("correctness".to_owned()),
            Some(heycode_session::FindingVerificationVerdict::Confirmed),
            Some(heycode_session::FindingOutcome::NoChangeNeeded),
        )
        .unwrap();
    let report = heycode_session::FindingReport::new(
        base.id().clone(),
        base.source().clone(),
        vec![finding],
    )
    .unwrap()
    .with_level(heycode_session::ReviewLevel::High);
    let mut state = AppState::new("model", "/workspace".into());
    state.items.push(Item::FindingsReport {
        report: Box::new(report),
        expanded: true,
        focused: false,
    });
    let rendered = frame_text(&mut state, 120, 45);
    assert!(rendered.contains("Code review(high · 1 finding)"));
    assert!(rendered.contains("⎿  src/file-0.rs"));
    assert!(rendered.contains("● 1-2 [correctness] Finding title 0"));
    assert!(rendered.contains("verdict: CONFIRMED"));
    assert!(rendered.contains("outcome: no change needed"));
}

#[test]
fn replay_correlates_parallel_tools_and_renders_normalized_provider_events() {
    let first = CallId::from_raw("call-first");
    let second = CallId::from_raw("call-second");
    let server = CallId::from_raw("server-web");
    let provider_state = ProviderStateItem::new(
        "openrouter",
        "model-a",
        ProviderProtocol::OpenAiChatCompletions,
        ProviderStateKind::ChatAssistantMessage,
        serde_json::json!({"role":"assistant","content":"OPAQUE_PROVIDER_SENTINEL"}),
    )
    .unwrap();
    let server_call = ServerToolCall::new(
        server.clone(),
        "web_search",
        "openrouter:web_search",
        serde_json::json!({"query":"OPAQUE_QUERY_SENTINEL"}),
    )
    .unwrap();
    let source =
        ServerToolSource::new("https://example.com/source", Some("Example source")).unwrap();
    let server_result = ServerToolResult::success(server.clone(), Some(1), vec![source]).unwrap();
    let citation = UrlCitation::new(
        "https://example.com/citation",
        Some("Cited page"),
        Some("bounded excerpt"),
        Some(2),
        Some(8),
    )
    .unwrap();
    let events = vec![
        event(
            0,
            SessionEventKind::ToolCall {
                turn: 1,
                call_id: first.clone(),
                name: "read".to_owned(),
                args: serde_json::json!({"path":"first.rs"}),
            },
        ),
        event(
            1,
            SessionEventKind::ToolCall {
                turn: 1,
                call_id: second.clone(),
                name: "grep".to_owned(),
                args: serde_json::json!({"pattern":"needle"}),
            },
        ),
        event(
            2,
            SessionEventKind::ToolResult {
                call_id: second,
                content: "second-result".to_owned(),
                is_error: false,
                untrusted_content: None,
            },
        ),
        event(
            3,
            SessionEventKind::ToolResult {
                call_id: first,
                content: "first-result".to_owned(),
                is_error: false,
                untrusted_content: None,
            },
        ),
        event(
            4,
            SessionEventKind::AssistantProviderItem {
                turn: 1,
                step: 1,
                request_id: heycode_core::RequestId::from_raw("request-1"),
                output_index: 0,
                item: Box::new(provider_state),
            },
        ),
        event(
            5,
            SessionEventKind::ServerToolCall {
                turn: 1,
                step: 1,
                request_id: heycode_core::RequestId::from_raw("request-1"),
                output_index: 1,
                call: Box::new(server_call),
            },
        ),
        event(
            6,
            SessionEventKind::AssistantCitation {
                turn: 1,
                step: 1,
                request_id: heycode_core::RequestId::from_raw("request-1"),
                output_index: 2,
                citation: Box::new(citation),
            },
        ),
        event(
            7,
            SessionEventKind::ServerToolResult {
                turn: 1,
                step: 1,
                request_id: heycode_core::RequestId::from_raw("request-2"),
                output_index: 0,
                result: Box::new(server_result),
            },
        ),
        event(
            8,
            SessionEventKind::CompactionApplied {
                summary: "portable checkpoint summary".to_owned(),
                replaced_upto_seq: 4,
            },
        ),
        event(
            9,
            SessionEventKind::NativeCompactionApplied {
                strategy: "provider-native".to_owned(),
                replaced_upto_seq: 8,
                items: Vec::new(),
                usage: None,
            },
        ),
    ];

    let mut state = AppState::new("model-a", "/workspace".into());
    state.replay(&events);

    let first = state.items.iter().find_map(|item| match item {
        Item::Tool {
            name,
            result: Some((true, value)),
            ..
        } if name == "read" => Some(value),
        _ => None,
    });
    let second = state.items.iter().find_map(|item| match item {
        Item::Tool {
            name,
            result: Some((true, value)),
            ..
        } if name == "grep" => Some(value),
        _ => None,
    });
    assert_eq!(
        first.and_then(serde_json::Value::as_str),
        Some("first-result")
    );
    assert_eq!(
        second.and_then(serde_json::Value::as_str),
        Some("second-result")
    );

    let flat = ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(!flat.contains("provider state:"), "{flat}");
    assert!(
        flat.contains("provider tool succeeded: web_search (openrouter:web_search)"),
        "{flat}"
    );
    assert!(flat.contains("citation: Cited page — https://example.com/citation"));
    assert!(flat.contains("portable compaction: through event 4"));
    assert!(flat.contains("native compaction: provider-native through event 8"));
    assert!(!flat.contains("OPAQUE_PROVIDER_SENTINEL"));
    assert!(!flat.contains("OPAQUE_QUERY_SENTINEL"));

    let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let full = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        full.contains("provider tool succeeded: web_search"),
        "{full}"
    );
    assert!(full.contains("citation: Cited page"), "{full}");
    assert!(
        full.contains("Compacted (ctrl+o to see full summary)"),
        "{full}"
    );
    assert!(
        !full.contains("portable compaction"),
        "summary starts collapsed: {full}"
    );
    assert!(!full.contains("OPAQUE_PROVIDER_SENTINEL"));
}

#[test]
fn one_hundred_thousand_event_session_replays_and_renders_only_the_requested_viewport() {
    let mut state = AppState::new("model-a", "/workspace".into());
    let events: Vec<SessionEvent> = (0..100_000_u64)
        .map(|index| {
            event(
                index,
                SessionEventKind::UserMessage {
                    text: format!("message {index}"),
                },
            )
        })
        .collect();
    let replay_started = Instant::now();
    state.replay(&events);
    let replay_elapsed = replay_started.elapsed();
    assert_eq!(state.items.len(), 100_000);
    assert!(
        replay_elapsed < Duration::from_secs(2),
        "100K replay exceeded budget: {replay_elapsed:?}"
    );
    let backend = TestBackend::new(100, 32);
    let mut terminal = Terminal::new(backend).unwrap();

    let started = Instant::now();
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let bottom_elapsed = started.elapsed();
    let bottom = state.transcript_cache_metrics();
    assert!(
        bottom.rendered_items <= 32,
        "bottom frame rendered {} items",
        bottom.rendered_items
    );
    assert!(bottom.retained_entries <= 256);
    assert!(
        bottom_elapsed < Duration::from_secs(1),
        "{bottom_elapsed:?}"
    );
    assert_eq!(bottom.indexed_items, 100_000);

    state.scroll_from_bottom = 50_000;
    let before = state.transcript_cache_metrics();
    let started = Instant::now();
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let middle_elapsed = started.elapsed();
    let middle = state.transcript_cache_metrics();
    assert!(
        middle.rendered_items.saturating_sub(before.rendered_items) <= 40,
        "middle frame rendered {} new items",
        middle.rendered_items.saturating_sub(before.rendered_items)
    );
    assert!(middle.retained_entries <= 256);
    assert_eq!(middle.indexed_items, bottom.indexed_items);
    assert!(
        middle_elapsed < Duration::from_secs(1),
        "{middle_elapsed:?}"
    );
}

#[test]
fn durable_settlement_and_operation_wrappers_do_not_duplicate_the_actionable_error() {
    let cause = "This model requires 18+ age confirmation. Choose another model with /model.";
    for cause_first in [true, false] {
        let mut state = AppState::new("m", "/p".into());
        state.apply_session_event(&event(0, SessionEventKind::TurnStart { turn: 1 }));
        if cause_first {
            state.apply(&heycode_agent::UiEvent::Error {
                message: cause.into(),
            });
        }
        state.apply_session_event(&event(
            1,
            SessionEventKind::TurnEnd {
                turn: 1,
                reason: heycode_session::TurnEndReason::Error,
            },
        ));
        state.apply(&heycode_agent::UiEvent::Error {
            message: "app-server operation failed: runtime operation failed".into(),
        });
        if !cause_first {
            state.apply(&heycode_agent::UiEvent::Error {
                message: cause.into(),
            });
        }
        state.apply(&heycode_agent::UiEvent::TurnFinished {
            reason: "error".into(),
            usage: None,
            context_tokens: None,
        });
        let errors = state
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Error(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(errors, [cause]);
    }
}

#[test]
fn replay_merges_saved_output_by_job_identity_and_keeps_unrelated_calls_separate() {
    for marker in [
        "[inline output capped; use job_output job-2]\n",
        "[inline output capped; use job_control action=output job_id=job-2]\n",
    ] {
        for (name, args) in [
            ("job_output", serde_json::json!({"job_id":"job-2"})),
            (
                "job_control",
                serde_json::json!({"action":"output","job_id":"job-2"}),
            ),
            (
                "mcp__heycode__job_control",
                serde_json::json!({"action":"output","job_id":"job-2"}),
            ),
        ] {
            let shell = CallId::from_raw("shell");
            let retrieval = CallId::from_raw("retrieval");
            let unrelated = CallId::from_raw("unrelated");
            let events = vec![
                event(0, SessionEventKind::ToolCall { turn:1, call_id:shell.clone(), name:"bash".into(), args:serde_json::json!({"command":"inspect"}) }),
                event(1, SessionEventKind::ToolResult { call_id:shell, content:format!("{marker}tail"), is_error:false, untrusted_content:None }),
                event(2, SessionEventKind::ToolCall { turn:1, call_id:unrelated.clone(), name:"read".into(), args:serde_json::json!({"path":"file"}) }),
                event(3, SessionEventKind::ToolResult { call_id:unrelated, content:"keep this independent".into(), is_error:false, untrusted_content:None }),
                event(4, SessionEventKind::ToolCall { turn:1, call_id:retrieval.clone(), name:name.into(), args:args.clone() }),
                event(5, SessionEventKind::ToolResult { call_id:retrieval, content:serde_json::json!({"job_id":"job-2","page":{"offset":0,"text":"restored full prefix"}}).to_string(), is_error:false, untrusted_content:None }),
            ];
            let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
            state.replay(&events);
            assert!(
                matches!(&state.items[0], Item::Tool { name, view, .. } if name == "bash" && view.retrieved_output.values().any(|text| text == "restored full prefix"))
            );
            assert!(
                matches!(&state.items[1], Item::Tool { name, view, .. } if name == "read" && view.retrieved_output.is_empty())
            );
            assert!(matches!(&state.items[2], Item::Tool { view, .. } if view.merged));
            state.replay(&events);
            assert!(
                matches!(&state.items[0], Item::Tool { view, .. } if view.retrieved_output.len() == 1)
            );
        }
    }
}
