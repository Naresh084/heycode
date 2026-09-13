//! O13 durable session-local schedules and dispatch correlation.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    InboxDelivery, InboxMessage, InboxMessageId, InboxSource, InboxTarget, ScheduleChange,
    ScheduleId, ScheduleRecord, ScheduleRule, ScheduleTimeZone, SessionEvent, SessionEventKind,
    project_schedules,
};

use chrono::{Datelike, Local, TimeZone, Timelike};

fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq,
        time_ms: i64::try_from(seq).unwrap(),
        kind,
    }
}

#[test]
fn dispatch_requires_a_prior_correlated_enqueue_and_recurrence_skips_backlog() {
    let id = ScheduleId::new("schedule-1").unwrap();
    let message_id = InboxMessageId::new("schedule-1-occurrence-1").unwrap();
    let record = ScheduleRecord::every(id.clone(), "check the build", 1_000, 10_000).unwrap();
    let create = event(
        0,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::create(record)),
        },
    );
    let enqueue = event(
        1,
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                InboxMessage::with_source(
                    message_id.clone(),
                    InboxDelivery::FollowUp,
                    "scheduled reminder",
                    InboxSource::Schedule {
                        schedule_id: id.clone(),
                        occurrence_at_ms: 10_000,
                    },
                )
                .unwrap(),
            ],
            outcome: None,
        },
    );
    let dispatch = event(
        2,
        SessionEventKind::ScheduleChange {
            change: Box::new(
                ScheduleChange::dispatch(id.clone(), 13_500, message_id.clone()).unwrap(),
            ),
        },
    );

    assert!(project_schedules(&[create.clone(), dispatch.clone()], 0).is_err());
    let projection = project_schedules(&[create, enqueue, dispatch], 0).unwrap();
    let active = projection.get(&id).unwrap();
    assert_eq!(active.scheduled_at_ms(), 14_000);
    assert_eq!(active.every_ms(), Some(1_000));
    assert_eq!(projection.dispatch_count(&id), 1);
}

#[test]
fn one_shot_dispatch_and_delete_are_terminal_and_ids_never_reuse() {
    let id = ScheduleId::new("schedule-once").unwrap();
    let message_id = InboxMessageId::new("schedule-once-message").unwrap();
    let create = event(
        0,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::create(
                ScheduleRecord::at(id.clone(), "one shot", 20_000).unwrap(),
            )),
        },
    );
    let enqueue = event(
        1,
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                InboxMessage::with_source(
                    message_id.clone(),
                    InboxDelivery::FollowUp,
                    "one shot",
                    InboxSource::Schedule {
                        schedule_id: id.clone(),
                        occurrence_at_ms: 20_000,
                    },
                )
                .unwrap(),
            ],
            outcome: None,
        },
    );
    let dispatch = event(
        2,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::dispatch(id.clone(), 20_000, message_id).unwrap()),
        },
    );
    let projection = project_schedules(&[create.clone(), enqueue, dispatch], 0).unwrap();
    assert!(projection.get(&id).is_none());
    assert!(
        project_schedules(
            &[
                create.clone(),
                event(
                    1,
                    SessionEventKind::ScheduleChange {
                        change: Box::new(ScheduleChange::delete(id.clone())),
                    }
                ),
                event(
                    2,
                    SessionEventKind::ScheduleChange {
                        change: Box::new(ScheduleChange::create(
                            ScheduleRecord::at(id, "reused", 30_000).unwrap(),
                        )),
                    }
                ),
            ],
            0
        )
        .is_err()
    );
}

#[test]
fn fork_local_projection_excludes_inherited_schedule_events_until_explicit_copy() {
    let parent = ScheduleId::new("parent-schedule").unwrap();
    let local = ScheduleId::new("local-copy").unwrap();
    let events = vec![
        event(
            0,
            SessionEventKind::ScheduleChange {
                change: Box::new(ScheduleChange::create(
                    ScheduleRecord::at(parent.clone(), "parent", 10_000).unwrap(),
                )),
            },
        ),
        event(
            1,
            SessionEventKind::SessionTitle {
                title: "fork-local-boundary".to_owned(),
            },
        ),
        event(
            2,
            SessionEventKind::ScheduleChange {
                change: Box::new(ScheduleChange::create(
                    ScheduleRecord::at(local.clone(), "copied", 10_000).unwrap(),
                )),
            },
        ),
    ];
    let fork = project_schedules(&events, 1).unwrap();
    assert!(fork.get(&parent).is_none());
    assert!(fork.get(&local).is_some());
}

#[test]
fn v1_cannot_claim_schedule_change() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("v1-schedule");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("session.jsonl"),
        "{\"v\":1,\"seq\":0,\"time_ms\":1,\"kind\":\"schedule/change\",\"data\":{}}\n",
    )
    .unwrap();
    assert!(matches!(
        heycode_session::Session::open(directory),
        Err(heycode_session::OpenError::UnknownKind { .. })
    ));
}

fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    Local
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

#[test]
fn cron_is_five_field_local_time_with_deterministic_bounded_jitter() {
    let created = local_ms(2030, 1, 1, 8, 1);
    let recurring = ScheduleRecord::cron(
        ScheduleId::new("cron-jitter").unwrap(),
        "poll locally",
        "*/15 * * * *",
        true,
        created,
    )
    .unwrap();
    let repeated = ScheduleRecord::cron(
        ScheduleId::new("cron-jitter").unwrap(),
        "poll locally",
        "*/15 * * * *",
        true,
        created,
    )
    .unwrap();
    assert_eq!(recurring, repeated, "id-derived jitter must replay exactly");
    assert_eq!(recurring.timezone(), Some(ScheduleTimeZone::Local));
    assert_eq!(recurring.cron_expression(), Some("*/15 * * * *"));
    assert!((0..=7 * 60 * 1_000).contains(&recurring.jitter_ms()));
    assert_eq!(
        recurring.expires_at_ms(),
        Some(created + 7 * 24 * 60 * 60 * 1_000)
    );
    let base = Local
        .timestamp_millis_opt(recurring.scheduled_at_ms() - recurring.jitter_ms())
        .single()
        .unwrap();
    assert_eq!(base.minute() % 15, 0);

    let exact = ScheduleRecord::cron(
        ScheduleId::new("cron-exact").unwrap(),
        "exact non-boundary minute",
        "5 9 * * *",
        false,
        local_ms(2030, 3, 14, 9, 6),
    )
    .unwrap();
    let exact_local = Local
        .timestamp_millis_opt(exact.scheduled_at_ms())
        .single()
        .unwrap();
    assert_eq!((exact_local.month(), exact_local.day()), (3, 15));
    assert_eq!((exact_local.hour(), exact_local.minute()), (9, 5));
    assert_eq!(exact.jitter_ms(), 0);

    let boundary = ScheduleRecord::cron(
        ScheduleId::new("cron-boundary").unwrap(),
        "top of hour",
        "0 10 * * *",
        false,
        created,
    )
    .unwrap();
    assert!((-90_000..=0).contains(&boundary.jitter_ms()));
}

#[test]
fn cron_supports_vixie_day_or_and_rejects_extended_or_malformed_syntax() {
    let record = ScheduleRecord::cron(
        ScheduleId::new("cron-day-or").unwrap(),
        "day match",
        "0 9 15 * 1",
        false,
        local_ms(2030, 1, 1, 0, 0),
    )
    .unwrap();
    let local = Local
        .timestamp_millis_opt(record.scheduled_at_ms() - record.jitter_ms())
        .single()
        .unwrap();
    assert!(local.day() == 15 || local.weekday().num_days_from_sunday() == 1);

    for (expression, id) in [
        ("0 9 1-31 * 1", "cron-full-dom-range"),
        ("0 9 15 * 0-6", "cron-full-dow-range"),
    ] {
        let full_numeric_range = ScheduleRecord::cron(
            ScheduleId::new(id).unwrap(),
            "numeric ranges remain restricted fields",
            expression,
            false,
            local_ms(2030, 1, 1, 0, 0),
        )
        .unwrap();
        let local = Local
            .timestamp_millis_opt(
                full_numeric_range.scheduled_at_ms() - full_numeric_range.jitter_ms(),
            )
            .single()
            .unwrap();
        assert_eq!((local.month(), local.day()), (1, 1));
    }

    let sunday = ScheduleRecord::cron(
        ScheduleId::new("cron-sunday-seven").unwrap(),
        "Sunday alias",
        "0 9 * * 7",
        false,
        local_ms(2030, 1, 1, 0, 0),
    )
    .unwrap();
    let sunday = Local
        .timestamp_millis_opt(sunday.scheduled_at_ms() - sunday.jitter_ms())
        .single()
        .unwrap();
    assert_eq!(sunday.weekday().num_days_from_sunday(), 0);

    let range_and_list = ScheduleRecord::cron(
        ScheduleId::new("cron-range-list").unwrap(),
        "range and list",
        "5,20 8-9 * * *",
        false,
        local_ms(2030, 1, 1, 8, 6),
    )
    .unwrap();
    let range_and_list = Local
        .timestamp_millis_opt(range_and_list.scheduled_at_ms() - range_and_list.jitter_ms())
        .single()
        .unwrap();
    assert_eq!((range_and_list.hour(), range_and_list.minute()), (8, 20));

    for expression in [
        "0 9 * *",
        "0 9 * * * *",
        "0 9 L * *",
        "0 9 ? * *",
        "0 9 * JAN *",
        "0 9 * * MON",
        "*/0 * * * *",
        "60 * * * *",
        "1-0 * * * *",
        "1,,2 * * * *",
    ] {
        assert!(
            ScheduleRecord::cron(
                ScheduleId::new("invalid-cron").unwrap(),
                "invalid",
                expression,
                true,
                local_ms(2030, 1, 1, 0, 0),
            )
            .is_err(),
            "accepted invalid cron expression: {expression}"
        );
    }
}

#[test]
fn replay_enforces_fifty_live_schedules() {
    let events = (0..51_u64)
        .map(|seq| {
            event(
                seq,
                SessionEventKind::ScheduleChange {
                    change: Box::new(ScheduleChange::create(
                        ScheduleRecord::at(
                            ScheduleId::new(format!("schedule-{seq}")).unwrap(),
                            "bounded",
                            10_000 + i64::try_from(seq).unwrap(),
                        )
                        .unwrap(),
                    )),
                },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(project_schedules(&events[..50], 0).unwrap().len(), 50);
    assert!(project_schedules(&events, 0).is_err());

    let legacy = (0..51_u64)
        .map(|seq| {
            event(
                seq,
                SessionEventKind::ScheduleChange {
                    change: Box::new(ScheduleChange::Create {
                        version: 1,
                        schedule: ScheduleRecord::at(
                            ScheduleId::new(format!("legacy-schedule-{seq}")).unwrap(),
                            "pre-cap schedule",
                            20_000 + i64::try_from(seq).unwrap(),
                        )
                        .unwrap(),
                    }),
                },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        project_schedules(&legacy, 0).unwrap().len(),
        51,
        "pre-cap version-one histories must remain readable"
    );
}

#[test]
fn recurring_cron_skips_backlog_and_retires_on_its_first_expired_dispatch() {
    let id = ScheduleId::new("cron-expiry").unwrap();
    let created = local_ms(2030, 1, 1, 8, 1);
    let record = ScheduleRecord::cron(id.clone(), "poll", "*/5 * * * *", true, created).unwrap();
    let occurrence = record.scheduled_at_ms();
    let message_id = InboxMessageId::new("cron-expiry-message").unwrap();
    let create = event(
        0,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::create(record.clone())),
        },
    );
    let enqueue = event(
        1,
        SessionEventKind::AgentInboxSplice {
            target: InboxTarget::NextTurn,
            start: 0,
            removed_count: None,
            inserted: vec![
                InboxMessage::with_source(
                    message_id.clone(),
                    InboxDelivery::FollowUp,
                    "cron",
                    InboxSource::Schedule {
                        schedule_id: id.clone(),
                        occurrence_at_ms: occurrence,
                    },
                )
                .unwrap(),
            ],
            outcome: None,
        },
    );
    let ordinary = event(
        2,
        SessionEventKind::ScheduleChange {
            change: Box::new(
                ScheduleChange::dispatch(id.clone(), occurrence + 17 * 60 * 1_000, message_id)
                    .unwrap(),
            ),
        },
    );
    let active = project_schedules(&[create.clone(), enqueue.clone(), ordinary], 0).unwrap();
    assert!(active.get(&id).unwrap().scheduled_at_ms() > occurrence + 17 * 60 * 1_000);
    assert_eq!(active.dispatch_count(&id), 1);

    let expired = event(
        2,
        SessionEventKind::ScheduleChange {
            change: Box::new(
                ScheduleChange::dispatch(
                    id.clone(),
                    record.expires_at_ms().unwrap(),
                    InboxMessageId::new("cron-expiry-message").unwrap(),
                )
                .unwrap(),
            ),
        },
    );
    assert!(
        project_schedules(&[create, enqueue, expired], 0)
            .unwrap()
            .get(&id)
            .is_none()
    );
}

#[test]
fn wakeup_dispatch_waits_for_explicit_reschedule_or_stop() {
    let id = ScheduleId::new("self-paced").unwrap();
    let record = ScheduleRecord::wakeup(id.clone(), "continue checking", 60_000, 1_000).unwrap();
    let message_id = InboxMessageId::new("self-paced-message").unwrap();
    let mut events = vec![
        event(
            0,
            SessionEventKind::ScheduleChange {
                change: Box::new(ScheduleChange::create(record.clone())),
            },
        ),
        event(
            1,
            SessionEventKind::AgentInboxSplice {
                target: InboxTarget::NextTurn,
                start: 0,
                removed_count: None,
                inserted: vec![
                    InboxMessage::with_source(
                        message_id.clone(),
                        InboxDelivery::FollowUp,
                        "self paced",
                        InboxSource::Schedule {
                            schedule_id: id.clone(),
                            occurrence_at_ms: record.scheduled_at_ms(),
                        },
                    )
                    .unwrap(),
                ],
                outcome: None,
            },
        ),
        event(
            2,
            SessionEventKind::ScheduleChange {
                change: Box::new(
                    ScheduleChange::dispatch(id.clone(), record.scheduled_at_ms(), message_id)
                        .unwrap(),
                ),
            },
        ),
    ];
    let awaiting = project_schedules(&events, 0).unwrap();
    assert!(awaiting.get(&id).is_some());
    assert!(!awaiting.is_armed(&id));

    events.push(event(
        3,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::reschedule(id.clone(), 120_000, 130_000).unwrap()),
        },
    ));
    let rescheduled = project_schedules(&events, 0).unwrap();
    assert!(rescheduled.is_armed(&id));
    assert_eq!(rescheduled.get(&id).unwrap().scheduled_at_ms(), 130_000);
    assert!(matches!(
        rescheduled.get(&id).unwrap().rule(),
        ScheduleRule::Wakeup { delay_ms: 120_000 }
    ));

    events.push(event(
        4,
        SessionEventKind::ScheduleChange {
            change: Box::new(ScheduleChange::delete(id.clone())),
        },
    ));
    assert!(project_schedules(&events, 0).unwrap().get(&id).is_none());
}
