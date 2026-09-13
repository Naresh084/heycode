//! O12 versioned workflow definitions, progress, and resumable checkpoints.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    Session, SessionEvent, SessionEventKind, WorkflowAction, WorkflowCapability, WorkflowChange,
    WorkflowDefinition, WorkflowOutcome, WorkflowRunId, WorkflowState, WorkflowStep,
    project_workflows,
};

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        "verify-lane",
        "Run two deterministic verification steps",
        vec![WorkflowCapability::Progress],
        vec![
            WorkflowStep::new(
                "inspect",
                "Inspect durable state",
                WorkflowAction::Emit {
                    value: serde_json::json!({"inspected":true}),
                },
            )
            .unwrap(),
            WorkflowStep::new(
                "verify",
                "Verify the focused gate",
                WorkflowAction::Emit {
                    value: serde_json::json!({"verified":true}),
                },
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

#[test]
fn checkpoint_replay_resumes_without_reexecuting_the_completed_prefix() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let run = WorkflowRunId::new("workflow-1").unwrap();
    for change in [
        WorkflowChange::start(run.clone(), definition()),
        WorkflowChange::progress(run.clone(), 1, 1, "inspect started").unwrap(),
        WorkflowChange::checkpoint(run.clone(), 1, serde_json::json!({"inspected":true})).unwrap(),
    ] {
        session
            .append(SessionEventKind::WorkflowChange {
                change: Box::new(change),
            })
            .unwrap();
    }
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let mut session = Session::open(directory).unwrap();
    let interrupted = project_workflows(session.events()).unwrap();
    let state = interrupted.get(&run).unwrap();
    assert_eq!(state.state(), WorkflowState::Running);
    assert_eq!(state.attempt(), 1);
    assert_eq!(state.completed_steps(), 1);
    assert_eq!(
        state.checkpoint(),
        Some(&serde_json::json!({"inspected":true}))
    );

    for change in [
        WorkflowChange::resume(run.clone(), 2, 1).unwrap(),
        WorkflowChange::progress(run.clone(), 2, 2, "verify started").unwrap(),
        WorkflowChange::checkpoint(run.clone(), 2, serde_json::json!({"verified":true})).unwrap(),
        WorkflowChange::end(run.clone(), WorkflowOutcome::Completed, 2, None).unwrap(),
    ] {
        session
            .append(SessionEventKind::WorkflowChange {
                change: Box::new(change),
            })
            .unwrap();
    }
    let finished = project_workflows(session.events()).unwrap();
    let state = finished.get(&run).unwrap();
    assert_eq!(state.state(), WorkflowState::Completed);
    assert_eq!(state.attempt(), 2);
    assert_eq!(state.completed_steps(), 2);
}

#[test]
fn duplicate_start_progress_gap_and_checkpoint_skip_fail_loud() {
    let run = WorkflowRunId::new("workflow-invalid").unwrap();
    let event = |seq, change| SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: i64::try_from(seq).unwrap(),
        kind: SessionEventKind::WorkflowChange {
            change: Box::new(change),
        },
    };
    let start = event(0, WorkflowChange::start(run.clone(), definition()));
    assert!(
        project_workflows(&[
            start.clone(),
            event(1, WorkflowChange::start(run.clone(), definition()))
        ])
        .is_err()
    );
    assert!(
        project_workflows(&[
            start.clone(),
            event(
                1,
                WorkflowChange::progress(run.clone(), 2, 1, "gap").unwrap()
            ),
        ])
        .is_err()
    );
    assert!(
        project_workflows(&[
            start,
            event(
                1,
                WorkflowChange::checkpoint(run, 2, serde_json::json!({"skipped":true})).unwrap(),
            ),
        ])
        .is_err()
    );
}

#[test]
fn workflow_definition_rejects_unknown_capability_use_and_unbounded_shapes() {
    assert!(
        WorkflowDefinition::new(
            "waiter",
            "Missing delay capability",
            vec![WorkflowCapability::Progress],
            vec![
                WorkflowStep::new(
                    "wait",
                    "wait",
                    WorkflowAction::Delay {
                        millis: 1,
                        value: serde_json::Value::Null,
                    },
                )
                .unwrap()
            ],
        )
        .is_err()
    );
    assert!(
        WorkflowDefinition::new(
            "too-many",
            "too many steps",
            vec![WorkflowCapability::Progress],
            (0..65)
                .map(|index| WorkflowStep::new(
                    format!("step-{index}"),
                    "step",
                    WorkflowAction::Emit {
                        value: serde_json::Value::Null
                    },
                )
                .unwrap())
                .collect(),
        )
        .is_err()
    );
}

#[test]
fn v1_cannot_claim_workflow_change() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("v1-workflow");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("session.jsonl"),
        "{\"v\":1,\"seq\":0,\"time_ms\":1,\"kind\":\"workflow/change\",\"data\":{}}\n",
    )
    .unwrap();
    assert!(matches!(
        Session::open(directory),
        Err(heycode_session::OpenError::UnknownKind { .. })
    ));
}

#[test]
fn graph_replay_rejects_premature_dependencies_duplicate_effects_and_unsafe_pause() {
    use heycode_session::{WorkflowNodeRecord, WorkflowNodeState};
    let definition = WorkflowDefinition::graph(
        "graph",
        "strict effect journal",
        vec![WorkflowCapability::Progress],
        vec![
            WorkflowStep::new(
                "first",
                "first",
                WorkflowAction::Emit {
                    value: serde_json::Value::Null,
                },
            )
            .unwrap(),
            WorkflowStep::new(
                "next",
                "next",
                WorkflowAction::Emit {
                    value: serde_json::Value::Null,
                },
            )
            .unwrap()
            .with_graph_policy(vec!["first".into()], None, 1, false)
            .unwrap(),
        ],
        2,
    )
    .unwrap();
    let id = WorkflowRunId::new("graph-run").unwrap();
    let event = |seq, change| SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 0,
        kind: SessionEventKind::WorkflowChange {
            change: Box::new(change),
        },
    };
    let node = |step: &str, state| WorkflowChange::Node {
        version: 1,
        run_id: id.clone(),
        step_id: step.into(),
        record: WorkflowNodeRecord {
            attempt: 1,
            state,
            value: serde_json::Value::Null,
        },
    };
    let start = event(0, WorkflowChange::start(id.clone(), definition));
    assert!(
        project_workflows(&[
            start.clone(),
            event(1, node("next", WorkflowNodeState::Started))
        ])
        .is_err()
    );
    let running = vec![start, event(1, node("first", WorkflowNodeState::Started))];
    let mut paused = running.clone();
    paused.push(event(
        2,
        WorkflowChange::end(
            id.clone(),
            WorkflowOutcome::Paused,
            0,
            Some("paused".into()),
        )
        .unwrap(),
    ));
    assert!(project_workflows(&paused).is_err());
    let mut completed = running;
    completed.push(event(2, node("first", WorkflowNodeState::Completed)));
    assert!(project_workflows(&completed).is_ok());
    completed.push(event(3, node("first", WorkflowNodeState::Completed)));
    assert!(project_workflows(&completed).is_err());
}

#[test]
fn default_legacy_definition_keeps_its_original_serialized_field_set() {
    let definition = definition();
    let value = serde_json::to_value(&definition).unwrap();
    assert!(value.get("max_parallel").is_none());
    assert_eq!(value["steps"][0].as_object().unwrap().len(), 3);
    assert_eq!(
        serde_json::from_value::<WorkflowDefinition>(value).unwrap(),
        definition
    );
}

#[test]
fn explicit_titles_and_phase_order_are_validated_without_changing_step_dependencies() {
    let mut value = serde_json::to_value(definition()).unwrap();
    assert!(value.get("phases").is_none());
    assert!(value.get("title").is_none());
    value["title"] = serde_json::json!("Inspect and verify");
    value["phases"] = serde_json::json!([{"id":"inspection","title":"Inspect"},{"id":"verification","title":"Verify"}]);
    value["steps"][0]["phase_id"] = serde_json::json!("inspection");
    value["steps"][1]["phase_id"] = serde_json::json!("verification");
    let grouped: WorkflowDefinition = serde_json::from_value(value.clone()).unwrap();
    grouped.validate().unwrap();
    assert_eq!(grouped.title(), "Inspect and verify");
    assert_eq!(grouped.phases()[0].id, "inspection");
    assert!(grouped.steps()[1].depends_on().is_empty());
    for invalid in [
        serde_json::json!([{"id":"inspection","title":"Inspect"},{"id":"inspection","title":"Duplicate"}]),
        serde_json::json!([{"id":"inspection","title":"Inspect"},{"id":"verification","title":"Verify"},{"id":"empty","title":"Empty"}]),
        serde_json::json!([]),
    ] {
        let mut invalid_value = value.clone();
        invalid_value["phases"] = invalid;
        assert!(
            serde_json::from_value::<WorkflowDefinition>(invalid_value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    value["steps"][1]
        .as_object_mut()
        .unwrap()
        .remove("phase_id");
    assert!(
        serde_json::from_value::<WorkflowDefinition>(value)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn native_task_correlations_are_strict_and_survive_terminal_run_and_handle_history() {
    use heycode_session::{
        WorkflowAgentRecord, WorkflowAgentState, WorkflowNodeRecord, WorkflowNodeState,
        WorkflowUsage,
    };
    let run_id = WorkflowRunId::new("workflow-agent-correlation").unwrap();
    let definition:WorkflowDefinition=serde_json::from_value(serde_json::json!({"version":2,"name":"fixture","description":"correlation","capabilities":["progress","agent"],"steps":[{"id":"agent","label":"Actual worker","action":{"kind":"agent","prompt":"Actual assignment"}}]})).unwrap();
    let event = |seq, change| SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 1000 + seq as i64,
        kind: SessionEventKind::WorkflowChange {
            change: Box::new(change),
        },
    };
    let node = |state| WorkflowChange::Node {
        version: 1,
        run_id: run_id.clone(),
        step_id: "agent".into(),
        record: WorkflowNodeRecord {
            attempt: 1,
            state,
            value: serde_json::Value::Null,
        },
    };
    let record = WorkflowAgentRecord {
        node_id: "agent".into(),
        node_attempt: 1,
        owner_session_id: "session-owner".into(),
        task_id: "task-stable-1".into(),
        session_id: None,
        job_id: None,
        label: "Actual worker".into(),
        prompt: "Actual assignment".into(),
        prompt_truncated: false,
        revision: 1,
        state: WorkflowAgentState::Queued,
        created_at_ms: 1001,
        updated_at_ms: 1001,
        summary: String::new(),
        summary_truncated: false,
        usage: WorkflowUsage::default(),
    };
    let observation = |record| WorkflowChange::Agent {
        version: 1,
        run_id: run_id.clone(),
        record,
    };
    let mut events = vec![
        event(0, WorkflowChange::start(run_id.clone(), definition)),
        event(1, node(WorkflowNodeState::Started)),
        event(2, observation(record.clone())),
    ];
    let mut published = record.clone();
    published.revision = 2;
    published.updated_at_ms = 1002;
    published.session_id = Some("actual-native-session".into());
    published.state = WorkflowAgentState::Running;
    events.push(event(3, observation(published.clone())));
    let mut changed_session = published.clone();
    changed_session.session_id = Some("other-session".into());
    let mut changed_owner = published.clone();
    changed_owner.owner_session_id = "other-owner".into();
    for mut invalid in [changed_session, changed_owner] {
        invalid.revision = 3;
        invalid.updated_at_ms = 1003;
        let mut replay = events.clone();
        replay.push(event(4, observation(invalid)));
        assert!(project_workflows(&replay).is_err());
    }
    let mut stale = events.clone();
    stale.push(event(4, observation(published.clone())));
    assert!(project_workflows(&stale).is_err());
    let mut wrong_node = published.clone();
    wrong_node.revision = 3;
    wrong_node.node_id = "unknown".into();
    let mut invalid = events.clone();
    invalid.push(event(4, observation(wrong_node)));
    assert!(project_workflows(&invalid).is_err());
    let mut duplicate = published.clone();
    duplicate.task_id = "task-other".into();
    let mut invalid = events.clone();
    invalid.push(event(4, observation(duplicate)));
    assert!(project_workflows(&invalid).is_err());
    let mut settled = published;
    settled.revision = 3;
    settled.updated_at_ms = 1004;
    settled.state = WorkflowAgentState::Completed;
    settled.summary = "Actual completed result".into();
    settled.usage = WorkflowUsage {
        reported_requests: 1,
        unreported_requests: 0,
        prompt_tokens: 12,
        completion_tokens: 7,
    };
    events.push(event(4, observation(settled.clone())));
    events.push(event(5, node(WorkflowNodeState::Completed)));
    events.push(event(
        6,
        WorkflowChange::end(run_id.clone(), WorkflowOutcome::Completed, 1, None).unwrap(),
    ));
    let mut closed = settled;
    closed.revision = 4;
    closed.updated_at_ms = 1007;
    closed.state = WorkflowAgentState::Closed;
    events.push(event(7, observation(closed)));
    let projection = project_workflows(&events).unwrap();
    let agent = &projection.get(&run_id).unwrap().agents()["task-stable-1"];
    assert_eq!(agent.state, WorkflowAgentState::Closed);
    assert_eq!(agent.summary, "Actual completed result");
    assert_eq!(agent.usage.prompt_tokens, 12);
    let mut regression = agent.clone();
    regression.revision = 5;
    regression.usage.prompt_tokens = 0;
    events.push(event(8, observation(regression)));
    assert!(project_workflows(&events).is_err());
}

#[test]
fn coordinator_jobs_are_unique_attempt_scoped_and_settlements_survive_resume() {
    let id = WorkflowRunId::new("coordinator-run").unwrap();
    let event = |seq, change| SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: 1000 + seq as i64,
        kind: SessionEventKind::WorkflowChange {
            change: Box::new(change),
        },
    };
    let job = |run_id: WorkflowRunId, attempt, job_id: &str| WorkflowChange::Job {
        version: 1,
        run_id,
        attempt,
        job_id: job_id.into(),
    };
    let start = event(0, WorkflowChange::start(id.clone(), definition()));
    let mut events = vec![start.clone(), event(1, job(id.clone(), 1, "job-7"))];
    assert!(
        project_workflows(std::slice::from_ref(&start))
            .unwrap()
            .get(&id)
            .unwrap()
            .jobs()
            .is_empty(),
        "old history has no inferred job association"
    );
    for invalid in [
        job(id.clone(), 1, "job-7"),
        job(id.clone(), 1, "job-8"),
        job(id.clone(), 2, "job-8"),
        job(WorkflowRunId::new("foreign").unwrap(), 1, "job-8"),
        job(id.clone(), 1, "job-07"),
    ] {
        let mut replay = events.clone();
        replay.push(event(2, invalid));
        assert!(project_workflows(&replay).is_err());
    }
    let later = vec![
        start.clone(),
        event(
            1,
            WorkflowChange::progress(id.clone(), 1, 1, "actual work").unwrap(),
        ),
        event(2, job(id.clone(), 1, "job-8")),
    ];
    assert!(
        project_workflows(&later).is_err(),
        "no retroactive claim after attempt work"
    );
    let other = WorkflowRunId::new("other-run").unwrap();
    let mut reused = events.clone();
    reused.push(event(2, WorkflowChange::start(other.clone(), definition())));
    reused.push(event(3, job(other, 1, "job-7")));
    assert!(project_workflows(&reused).is_err());
    events.push(event(
        2,
        WorkflowChange::end(
            id.clone(),
            WorkflowOutcome::Paused,
            0,
            Some("paused".into()),
        )
        .unwrap(),
    ));
    let paused = project_workflows(&events).unwrap();
    assert_eq!(
        paused.get(&id).unwrap().jobs()[&1].outcome,
        Some(WorkflowOutcome::Paused)
    );
    // Stopping an already-paused run cannot rewrite its previous job's outcome.
    let mut cancelled = events.clone();
    cancelled.push(event(
        3,
        WorkflowChange::end(
            id.clone(),
            WorkflowOutcome::Cancelled,
            0,
            Some("stopped".into()),
        )
        .unwrap(),
    ));
    assert_eq!(
        project_workflows(&cancelled)
            .unwrap()
            .get(&id)
            .unwrap()
            .jobs()[&1]
            .outcome,
        Some(WorkflowOutcome::Paused)
    );
    events.push(event(3, WorkflowChange::resume(id.clone(), 2, 0).unwrap()));
    events.push(event(4, job(id.clone(), 2, "job-8")));
    events.push(event(
        5,
        WorkflowChange::end(
            id.clone(),
            WorkflowOutcome::Failed,
            0,
            Some("provider failure".into()),
        )
        .unwrap(),
    ));
    let projection = project_workflows(&events).unwrap();
    let jobs = projection.get(&id).unwrap().jobs();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[&1].job_id, "job-7");
    assert_eq!(jobs[&1].outcome, Some(WorkflowOutcome::Paused));
    assert_eq!(jobs[&1].settled_at_ms, Some(1002));
    assert_eq!(jobs[&2].job_id, "job-8");
    assert_eq!(jobs[&2].outcome, Some(WorkflowOutcome::Failed));
    assert_eq!(jobs[&2].settled_at_ms, Some(1005));
}
