//! Receipt identity and disclosure contracts for durable agent events.

use super::{AppState, Item, ToolViewState};
use heycode_session::{
    AgentCompletionOutcome, InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget,
    SessionEvent, SessionEventKind,
};

fn completion(
    id: &str,
    run: &str,
    outcome: AgentCompletionOutcome,
) -> anyhow::Result<InboxMessage> {
    let (status, notice) = match outcome {
        AgentCompletionOutcome::Completed => (
            "completed",
            "EXACT_INTERNAL_RESULT with job-private and full provenance",
        ),
        AgentCompletionOutcome::Failed => {
            ("failed", "permission denied while reading configuration")
        }
        AgentCompletionOutcome::Cancelled => ("cancelled", "cancelled by the owner"),
        AgentCompletionOutcome::Interrupted => ("interrupted", "runtime stopped before completion"),
    };
    InboxMessage::with_source(
        InboxMessageId::new(id)?,
        InboxDelivery::FollowUp,
        format!("[agent Atlas 分析 (agent-atlas) {status}; run {run}]\n{notice}"),
        InboxSource::Agent {
            agent_id: "agent-atlas".into(),
            agent_name: "Atlas 分析".into(),
            recipient_id: "parent".into(),
            run_id: run.into(),
            completion_id: Some(id.into()),
            outcome: Some(outcome),
        },
    )
    .map_err(Into::into)
}

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: seq as i64,
        kind,
    }
}

fn insert(seq: u64, message: InboxMessage) -> SessionEvent {
    event(
        seq,
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![message],
            outcome: None,
        },
    )
}

fn claim(seq: u64) -> SessionEvent {
    event(
        seq,
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: Some(1),
            inserted: vec![],
            outcome: None,
        },
    )
}

fn transcript(state: &AppState) -> String {
    state
        .items
        .iter()
        .flat_map(|item| {
            crate::render::render_transcript_item(
                item,
                Default::default(),
                80,
                false,
                state.styles(),
            )
        })
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn named_completion_appears_at_settlement_once_before_and_after_model_admission()
-> anyhow::Result<()> {
    let message = completion(
        "completion-one",
        "run-one",
        AgentCompletionOutcome::Completed,
    )?;
    let mut state = AppState::new("native", "/workspace".into());
    let inserted = insert(0, message.clone());
    state.apply_session_event(&inserted);
    let text = transcript(&state);
    assert_eq!(text.matches("○ Atlas 分析 completed").count(), 1, "{text}");
    assert!(
        !text.contains("job-private") && !text.contains("completion-one"),
        "{text}"
    );
    let events = vec![
        inserted,
        claim(1),
        event(
            2,
            SessionEventKind::UserMessage {
                text: message.text().into(),
            },
        ),
        event(
            3,
            SessionEventKind::AssistantMessage {
                turn: 1,
                step: 0,
                content: "PARENT_SUBSTANTIVE_ANSWER".into(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            },
        ),
    ];
    for event in &events[1..] {
        state.apply_session_event(event);
    }
    let text = transcript(&state);
    assert_eq!(text.matches("○ Atlas 分析 completed").count(), 1, "{text}");
    assert!(text.contains("PARENT_SUBSTANTIVE_ANSWER"), "{text}");
    assert_eq!(state.items.len(), 2);
    for _ in 0..2 {
        state.replay(&events);
        assert_eq!(
            transcript(&state).matches("○ Atlas 分析 completed").count(),
            1
        );
    }
    let Some(Item::Tool { view, .. }) = state.items.first_mut() else {
        anyhow::bail!("missing completion card")
    };
    view.expanded = true;
    let expanded = transcript(&state);
    for exact in [
        "EXACT_INTERNAL_RESULT",
        "job-private",
        "agent-atlas",
        "completion-one",
        "run-one",
        "parent",
    ] {
        assert!(expanded.contains(exact), "missing {exact}: {expanded}");
    }
    Ok(())
}

#[test]
fn duplicate_delivery_is_one_receipt_but_a_new_run_has_its_own_receipt() -> anyhow::Result<()> {
    let one = completion(
        "completion-one",
        "run-one",
        AgentCompletionOutcome::Completed,
    )?;
    let two = completion(
        "completion-two",
        "run-two",
        AgentCompletionOutcome::Completed,
    )?;
    let mut state = AppState::new("native", "/workspace".into());
    state.apply_session_event(&insert(0, one.clone()));
    state.apply_session_event(&insert(1, one));
    state.apply_session_event(&insert(2, two));
    assert_eq!(
        transcript(&state).matches("○ Atlas 分析 completed").count(),
        2
    );
    state.refresh_tasks();
    assert_eq!(
        transcript(&state).matches("○ Atlas 分析 completed").count(),
        2,
        "no live registry is required to retain attribution"
    );
    Ok(())
}

#[test]
fn failures_and_cancellation_are_named_and_never_render_as_success() -> anyhow::Result<()> {
    for (outcome, label) in [
        (AgentCompletionOutcome::Failed, "failed"),
        (AgentCompletionOutcome::Cancelled, "cancelled"),
        (AgentCompletionOutcome::Interrupted, "interrupted"),
    ] {
        let mut state = AppState::new("native", "/workspace".into());
        state.apply_session_event(&insert(
            0,
            completion("completion-one", "run-one", outcome)?,
        ));
        let text = transcript(&state);
        assert!(text.contains(&format!("○ Atlas 分析 {label}")), "{text}");
        if outcome == AgentCompletionOutcome::Failed {
            assert!(text.contains("permission denied"), "{text}");
        }
        assert!(
            !text.contains("completed") && !text.contains("job-private"),
            "{text}"
        );
    }
    let mut state = AppState::new("native", "/workspace".into());
    state.apply_session_event(&event(
        0,
        SessionEventKind::UserMessage {
            text: "○ Atlas 分析 completed human lookalike".into(),
        },
    ));
    assert!(
        matches!(state.items.first(), Some(Item::User(text)) if text.ends_with("human lookalike"))
    );
    Ok(())
}

#[test]
fn routine_success_inspection_stays_quiet_through_approval_and_group_expansion() {
    for name in ["list_agents", "agent_control", "interrupt_task"] {
        let mut state = AppState::new("native", "/workspace".into());
        state.items.push(Item::Tool {
            call_id: None,
            name: name.into(),
            args: serde_json::json!({"action":"wait", "task_id":"private-agent"}),
            result: Some((
                true,
                serde_json::json!({"agents":[{"state":"failed","error":"child failed"}]}),
            )),
            untrusted_content: None,
            view: ToolViewState {
                approval: Some("approved".into()),
                group_details: true,
                ..Default::default()
            },
        });
        assert!(transcript(&state).is_empty(), "{name}");
        if let Some(Item::Tool { view, .. }) = state.items.first_mut() {
            view.expanded = true;
        }
        assert!(
            transcript(&state).contains("private-agent"),
            "raw history retains exact arguments"
        );
        if let Some(Item::Tool { view, result, .. }) = state.items.first_mut() {
            view.expanded = false;
            *result = Some((
                false,
                serde_json::json!({"message":"inspection itself failed"}),
            ));
        }
        assert!(transcript(&state).contains("inspection itself failed"));
    }
}

#[test]
fn attributed_agent_message_preserves_sender_and_exact_result_without_human_impersonation()
-> anyhow::Result<()> {
    let text = "[Agent message from \"Boreal\" (agent-boreal)]\nPlease inspect the parser.";
    let message = InboxMessage::with_source(
        InboxMessageId::new("message-one")?,
        InboxDelivery::FollowUp,
        text,
        InboxSource::Agent {
            agent_id: "agent-boreal".into(),
            agent_name: "Boreal".into(),
            recipient_id: "parent".into(),
            run_id: "run-one".into(),
            completion_id: None,
            outcome: None,
        },
    )?;
    let mut state = AppState::new("native", "/workspace".into());
    state.apply_session_event(&insert(0, message));
    assert!(
        state.items.is_empty(),
        "ordinary messages are not terminal completion events"
    );
    state.apply_session_event(&claim(1));
    state.apply_session_event(&event(
        2,
        SessionEventKind::UserMessage { text: text.into() },
    ));
    assert!(!state.items.iter().any(|item| matches!(item, Item::User(_))));
    let shown = transcript(&state);
    assert!(
        shown.contains("○ Boreal") && shown.contains("Please inspect the parser."),
        "{shown}"
    );
    assert!(!shown.contains("agent-boreal"), "{shown}");
    if let Some(Item::Tool { view, .. }) = state.items.first_mut() {
        view.expanded = true;
    }
    let raw = transcript(&state);
    assert!(
        raw.contains("agent-boreal") && raw.contains("message-one"),
        "{raw}"
    );
    Ok(())
}

#[test]
fn inbox_driver_lifecycle_controls_activity_without_a_tui_owned_turn_task() -> anyhow::Result<()> {
    let mut state = AppState::new("native", "/workspace".into());
    state.apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
    assert!(state.has_active_turn());
    state.apply_session_event(&insert(
        0,
        completion(
            "completion-one",
            "run-one",
            AgentCompletionOutcome::Completed,
        )?,
    ));
    assert!(
        state.has_active_turn(),
        "child completion must not pretend the parent's response is idle"
    );
    state.apply(&heycode_agent::UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    });
    assert!(!state.has_active_turn());
    Ok(())
}

#[test]
fn long_agent_message_discloses_truncation_and_keeps_exact_expanded_body() -> anyhow::Result<()> {
    let body = (1..=9)
        .map(|i| format!("message line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("[Agent message from \"Boreal\" (agent-boreal)]\n{body}");
    let message = InboxMessage::with_source(
        InboxMessageId::new("message-long")?,
        InboxDelivery::FollowUp,
        text.clone(),
        InboxSource::Agent {
            agent_id: "agent-boreal".into(),
            agent_name: "Boreal".into(),
            recipient_id: "parent".into(),
            run_id: "run-one".into(),
            completion_id: None,
            outcome: None,
        },
    )?;
    let mut state = AppState::new("native", "/workspace".into());
    state
        .items
        .push(super::inbox_transcript::agent_message_item(message));
    let shown = transcript(&state);
    assert!(shown.contains("expand for the full message"), "{shown}");
    assert!(!shown.contains("message line 9"), "{shown}");
    if let Some(Item::Tool { view, .. }) = state.items.first_mut() {
        view.expanded = true;
    }
    assert!(transcript(&state).contains("message line 9"));
    Ok(())
}

#[test]
fn receipt_click_and_enter_inspect_exact_old_run_without_switching_conversation()
-> anyhow::Result<()> {
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    let mut state = AppState::new("native", "/workspace".into());
    state.input.insert_str("parent draft");
    state.apply_session_event(&insert(
        0,
        completion(
            "completion-old",
            "run-old",
            AgentCompletionOutcome::Completed,
        )?,
    ));
    state.apply_session_event(&insert(
        1,
        completion(
            "completion-new",
            "run-new",
            AgentCompletionOutcome::Completed,
        )?,
    ));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 42))?;
    terminal.draw(|frame| crate::render::draw(frame, &mut state))?;
    let (row, index) = state.reasoning_hit_rows.iter().copied().find(|(_, index)| {
        matches!(state.items.get(*index), Some(Item::Tool { args, .. }) if args.pointer("/source/run_id").and_then(serde_json::Value::as_str) == Some("run-old"))
    }).ok_or_else(|| anyhow::anyhow!("older completion must retain a clickable row"))?;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        state.handle_terminal_event(&Event::Mouse(MouseEvent {
            kind,
            column: state.transcript_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }
    assert!(
        matches!(&state.items[index], Item::Tool { view, .. } if view.expanded && view.focused)
    );
    let retained = transcript(&state);
    assert!(
        retained.contains("run-old") && retained.contains("completion-old"),
        "{retained}"
    );
    assert!(
        !retained.contains("run-new") && !retained.contains("completion-new"),
        "newer occurrence stays collapsed: {retained}"
    );
    assert!(!state.task_console.active && !state.task_console.preview);
    assert_eq!(state.input.lines(), &["parent draft"]);
    for expanded in [false, true] {
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert!(
            matches!(&state.items[index], Item::Tool { view, .. } if view.expanded == expanded)
        );
    }
    state.refresh_tasks();
    assert!(
        transcript(&state).contains("run-old"),
        "registry refresh cannot rewrite retained receipt identity"
    );
    Ok(())
}

#[test]
fn pending_question_batch_is_quiet_but_malformed_or_failed_receipts_are_visible() {
    for (ok, ids, quiet) in [
        (true, serde_json::json!(["question-a", "question-b"]), true),
        (true, serde_json::json!([]), false),
        (true, serde_json::json!([""]), false),
        (true, serde_json::json!([1]), false),
        (false, serde_json::json!(["question-a"]), false),
    ] {
        let item = Item::Tool {
            call_id: None,
            name: "ask_user_question_async".into(),
            args: serde_json::json!({}),
            result: Some((
                ok,
                serde_json::json!({"status":"pending", "question_ids":ids}),
            )),
            untrusted_content: None,
            view: ToolViewState::default(),
        };
        assert_eq!(crate::transcript::quiet_orchestration(&item), quiet);
    }
}

#[test]
fn old_foreground_join_cannot_settle_newer_native_inbox_turn_or_duplicate_text()
-> anyhow::Result<()> {
    use heycode_agent::UiEvent;
    let mut state = AppState::new("native", "/workspace".into());
    state.mark_turn_scheduled();
    state
        .items
        .push(Item::Assistant("durable parent answer".into()));
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    })?;
    sender.send(UiEvent::TurnStarted { turn: 2 })?;
    sender.send(UiEvent::AssistantDelta {
        text: "duplicate relay text".into(),
    })?;
    state.settle_joined_turn(&mut receiver, || true);
    assert!(
        state.has_active_turn(),
        "old relay join must preserve the newer driver turn"
    );
    assert!(!transcript(&state).contains("duplicate relay text"));
    assert_eq!(
        transcript(&state).matches("durable parent answer").count(),
        1
    );
    state.apply(&UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    });
    assert!(!state.has_active_turn());
    Ok(())
}

#[test]
fn native_activity_sample_after_queued_finish_prevents_idle_resurrection() -> anyhow::Result<()> {
    use heycode_agent::UiEvent;
    let mut state = AppState::new("native", "/workspace".into());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(UiEvent::TurnStarted { turn: 2 })?;
    sender.send(UiEvent::TurnFinished {
        reason: "stop".into(),
        usage: None,
        context_tokens: None,
    })?;
    state.settle_joined_turn(&mut receiver, || false);
    assert!(!state.has_active_turn());
    Ok(())
}

#[test]
fn required_question_batch_shows_questions_and_readable_answers_with_exact_raw_history() {
    let mut state = AppState::new("native", "/workspace".into());
    state.items.push(Item::Tool {
        call_id: None,
        name: "ask_user_question".into(),
        args: serde_json::json!({"questions":[{"id":"private-scope","question":"Which scope?"}]}),
        result: Some((
            true,
            serde_json::json!({"answers":[
                {"id":"private-scope","question":"Which scope?","answer":["Architecture","Tests"]},
                {"id":"private-detail","question":"Any detail?","answer":"Keep the parser simple"}
            ]}),
        )),
        untrusted_content: None,
        view: ToolViewState::default(),
    });
    let shown = transcript(&state);
    assert!(
        shown.contains("Question: Which scope?")
            && shown.contains("You answered: Architecture, Tests"),
        "{shown}"
    );
    assert!(
        shown.contains("Question: Any detail?") && shown.contains("Keep the parser simple"),
        "{shown}"
    );
    assert!(
        !shown.contains("private-scope") && !shown.contains("\"answers\""),
        "{shown}"
    );
    if let Some(Item::Tool { view, .. }) = state.items.first_mut() {
        view.expanded = true;
    }
    assert!(transcript(&state).contains("private-scope"));
}

#[test]
fn optional_selected_labels_are_readable_but_custom_json_answer_is_preserved() -> anyhow::Result<()>
{
    for selected in [
        Some(vec!["Architecture".to_owned(), "Tests".to_owned()]),
        None,
    ] {
        let id = InboxMessageId::new("optional-selection")?;
        let answer = "[\"Architecture\",\"Tests\"]";
        let prompt = "Which scope?\n\nKeep this prompt paragraph.";
        let text = format!("Answer to optional question [{id}]: {prompt}\n\n{answer}");
        let message = InboxMessage::with_source(
            id.clone(),
            InboxDelivery::FollowUp,
            text.clone(),
            InboxSource::OptionalQuestion {
                question_id: id,
                selected_answers: selected.clone(),
            },
        )?;
        let mut state = AppState::new("native", "/workspace".into());
        state.apply_session_event(&insert(0, message));
        state.apply_session_event(&claim(1));
        state.apply_session_event(&event(2, SessionEventKind::UserMessage { text }));
        let shown = transcript(&state);
        assert!(shown.contains("Keep this prompt paragraph."), "{shown}");
        if selected.is_some() {
            assert!(
                shown.contains("You answered: Architecture, Tests"),
                "{shown}"
            );
            assert!(!shown.contains(answer), "{shown}");
        } else {
            assert!(
                shown.contains(answer),
                "custom JSON text must never be inferred as selected labels: {shown}"
            );
        }
        assert!(!shown.contains("optional-selection"), "{shown}");
    }
    Ok(())
}

#[test]
fn all_four_required_answers_fit_the_transcript_height_contract_at_narrow_widths() {
    let args = serde_json::json!({"questions":[]});
    let value = serde_json::json!({"answers": (1..=4).map(|number| serde_json::json!({
        "id":format!("private-question-{number}"),
        "question":format!("Question {number}: choose the responsibilities to include"),
        "answer":[format!("Choice {number}A"),format!("Choice {number}B")]
    })).collect::<Vec<_>>()});
    let item = Item::Tool {
        call_id: None,
        name: "ask_user_question".into(),
        args,
        result: Some((true, value)),
        untrusted_content: None,
        view: ToolViewState::default(),
    };
    let state = AppState::new("native", "/workspace".into());
    for width in [20, 40, 60, 110] {
        let rows = crate::render::render_transcript_item(
            &item,
            Default::default(),
            width,
            false,
            state.styles(),
        );
        let shown = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for number in 1..=4 {
            assert!(
                shown.contains(&format!("Choice {number}B")),
                "width={width}: {shown}"
            );
        }
        let bound = crate::transcript::height_bound(&item, Default::default(), width, false);
        assert!(
            rows.len() <= bound,
            "width={width}: actual={} bound={bound}",
            rows.len()
        );
    }
}

#[test]
fn queued_send_message_transport_receipts_are_quiet_but_exact_raw_history_survives() {
    for name in ["send_message", "mcp__heycode__send_message"] {
        let mut state = AppState::new("native", "/workspace".into());
        state.items.push(Item::Tool {
            call_id: None, name: name.into(), args: serde_json::json!({"to":"Atlas","message":"Inspect parser"}),
            result: Some((true, serde_json::json!({"agent_id":"private-agent","name":"Atlas","message_id":"private-message","status":"queued"}))),
            untrusted_content: None, view: ToolViewState { approval: Some("approved".into()), group_details: true, ..Default::default() },
        });
        assert!(transcript(&state).is_empty());
        assert!(
            !super::accessibility::ScreenReaderSnapshot::from_state(&state)
                .into_text()
                .contains("private-agent")
        );
        if let Some(Item::Tool { view, .. }) = state.items.first_mut() {
            view.expanded = true;
        }
        let raw = transcript(&state);
        assert!(
            raw.contains("private-agent")
                && raw.contains("private-message")
                && raw.contains("Inspect parser"),
            "{raw}"
        );
    }
}

#[test]
fn send_message_results_failures_pending_approvals_and_incoming_messages_remain_visible()
-> anyhow::Result<()> {
    let queued = serde_json::json!({"agent_id":"private-agent","name":"Atlas","message_id":"private-message","status":"queued"});
    for (result, approval) in [
        (None, None),
        (
            Some((
                false,
                serde_json::json!({"message":"recipient unavailable"}),
            )),
            None,
        ),
        (
            Some((true, serde_json::json!("Actual synchronous finding"))),
            None,
        ),
        (
            Some((
                true,
                serde_json::json!({"agent_id":"private-agent","name":"Atlas","message_id":"private-message","status":"queued","result":"Actual synchronous finding"}),
            )),
            None,
        ),
        (Some((true, queued.clone())), Some("awaiting approval")),
        (Some((true, queued)), Some("rejected")),
    ] {
        let item = Item::Tool {
            call_id: None,
            name: "send_message".into(),
            args: serde_json::json!({"to":"Atlas"}),
            result,
            untrusted_content: None,
            view: ToolViewState {
                approval: approval.map(str::to_owned),
                ..Default::default()
            },
        };
        assert!(!crate::transcript::quiet_orchestration(&item));
    }
    let message = InboxMessage::with_source(
        InboxMessageId::new("incoming-one")?,
        InboxDelivery::Steer,
        "[Agent message from \"Atlas\" (agent-atlas)]\nThe parser needs an explicit delimiter.",
        InboxSource::Agent {
            agent_id: "agent-atlas".into(),
            agent_name: "Atlas".into(),
            recipient_id: "main".into(),
            run_id: "run-one".into(),
            completion_id: None,
            outcome: None,
        },
    )?;
    let mut state = AppState::new("native", "/workspace".into());
    state
        .items
        .push(super::inbox_transcript::agent_message_item(message));
    assert!(!crate::transcript::quiet_orchestration(&state.items[0]));
    assert!(transcript(&state).contains("The parser needs an explicit delimiter."));
    Ok(())
}

#[test]
fn long_incoming_messages_keep_disclosure_and_cached_scroll_boundaries() -> anyhow::Result<()> {
    let state = AppState::new("native", "/workspace".into());
    let items = (0..80)
        .map(|index| {
            let name = format!("Agent {index:02}");
            let body = (1..=9)
                .map(|line| format!("message-{index:02} row-{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let text = format!(
                "[Agent message from {} (agent-{index})]\n{body}",
                serde_json::to_string(&name)?
            );
            let message = InboxMessage::with_source(
                InboxMessageId::new(format!("incoming-{index}"))?,
                InboxDelivery::Steer,
                text,
                InboxSource::Agent {
                    agent_id: format!("agent-{index}"),
                    agent_name: name,
                    recipient_id: "main".into(),
                    run_id: format!("run-{index}"),
                    completion_id: None,
                    outcome: None,
                },
            )?;
            Ok(super::inbox_transcript::agent_message_item(message))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for width in [20, 60, 110] {
        let full = items
            .iter()
            .enumerate()
            .flat_map(|(index, item)| {
                let neighbors = crate::render::item_neighbors(&items, index);
                let rows = crate::render::render_transcript_item(
                    item,
                    neighbors,
                    width,
                    false,
                    state.styles(),
                );
                assert_eq!(
                    rows.len(),
                    9,
                    "header, six content rows, disclosure, and blank"
                );
                assert!(
                    rows.len() <= crate::transcript::height_bound(item, neighbors, width, false)
                );
                rows.into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mut cache = crate::transcript::TranscriptRenderCache::default();
        // Beyond 32 items scrolling uses the prefix height index. Offsets at
        // exact receipt boundaries must agree with the fully rendered source.
        for offset in [0, 9, 288, 297, 360, 540, 720] {
            let spec = crate::transcript::ViewportSpec {
                width,
                visible_lines: 18,
                scroll_from_bottom: offset,
                show_reasoning: false,
                style_generation: 0,
            };
            let start = full.len().saturating_sub(offset).saturating_sub(18);
            for _ in 0..2 {
                let viewport = cache.viewport(&items, spec, |item, neighbors| {
                    crate::render::render_transcript_item(
                        item,
                        neighbors,
                        width,
                        false,
                        state.styles(),
                    )
                });
                let shown = viewport.iter().map(ToString::to_string).collect::<Vec<_>>();
                let mut expected = full[start..start + 18].to_vec();
                // Viewports omit trailing separator rows, without pulling in
                // older content or changing the indexed starting position.
                while expected.last().is_some_and(String::is_empty) {
                    expected.pop();
                }
                assert_eq!(shown, expected, "width={width}, offset={offset}");
            }
        }
    }
    Ok(())
}
