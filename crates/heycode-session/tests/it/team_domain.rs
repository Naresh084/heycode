#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    Session, SessionEventKind, TeamChange, TeamId, TeamMailboxMessage, TeamMember, TeamMemberId,
    TeamMessageId, TeamRole, TeamTask, TeamTaskId, TeamTaskState, project_teams,
};

fn member(id: &str, role: TeamRole) -> TeamMember {
    TeamMember::new(TeamMemberId::new(id).unwrap(), id, role).unwrap()
}

fn append(session: &mut Session, change: TeamChange) {
    session
        .append(SessionEventKind::TeamChange {
            change: Box::new(change),
        })
        .unwrap();
}

#[test]
fn roster_dag_and_mailbox_reopen_from_durable_truth() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let team = TeamId::new("release-team").unwrap();
    let lead = member("lead", TeamRole::Lead);
    let worker = member("worker", TeamRole::Worker);
    let reviewer = member("reviewer", TeamRole::Reviewer);

    append(
        &mut session,
        TeamChange::created(team.clone(), lead.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::member_added(&team, 1, lead.id(), worker.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::member_added(&team, 2, lead.id(), reviewer.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_created(
            &team,
            3,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("implement").unwrap(),
                "Implement the change",
                worker.id().clone(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_created(
            &team,
            4,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("review").unwrap(),
                "Review the change",
                reviewer.id().clone(),
                vec![TeamTaskId::new("implement").unwrap()],
            )
            .unwrap(),
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_state_changed(
            &team,
            5,
            lead.id(),
            &TeamTaskId::new("implement").unwrap(),
            1,
            TeamTaskState::InProgress,
            None,
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_state_changed(
            &team,
            6,
            worker.id(),
            &TeamTaskId::new("implement").unwrap(),
            2,
            TeamTaskState::Completed,
            Some("implemented".to_owned()),
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::message_sent(
            &team,
            7,
            worker.id(),
            TeamMailboxMessage::new(
                "mail-1",
                worker.id().clone(),
                reviewer.id().clone(),
                "Please review task implement",
            )
            .unwrap(),
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::message_claimed(&team, 8, reviewer.id(), "mail-1").unwrap(),
    );
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);

    let reopened = Session::open(directory).unwrap();
    let projection = project_teams(reopened.events()).unwrap();
    let view = projection.team(&team).unwrap();
    assert_eq!(view.revision(), 8);
    assert_eq!(view.members().len(), 3);
    assert_eq!(view.tasks().len(), 2);
    assert_eq!(
        view.task(&TeamTaskId::new("implement").unwrap())
            .unwrap()
            .state(),
        TeamTaskState::Completed
    );
    assert!(view.pending_mail(reviewer.id()).is_empty());
    assert!(
        view.task(&TeamTaskId::new("review").unwrap())
            .unwrap()
            .is_runnable(view)
    );
}

#[test]
fn stale_foreign_and_cyclic_changes_fail_projection() {
    let team = TeamId::new("team").unwrap();
    let lead = member("lead", TeamRole::Lead);
    let worker = member("worker", TeamRole::Worker);
    let mut session = Session::create(tempfile::tempdir().unwrap().path()).unwrap();
    append(
        &mut session,
        TeamChange::created(team.clone(), lead.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::member_added(&team, 1, lead.id(), worker.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_created(
            &team,
            2,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("a").unwrap(),
                "A",
                worker.id().clone(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    append(
        &mut session,
        TeamChange::task_created(
            &team,
            3,
            lead.id(),
            TeamTask::new(
                TeamTaskId::new("b").unwrap(),
                "B",
                worker.id().clone(),
                vec![TeamTaskId::new("a").unwrap()],
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let mut events = session.events().to_vec();
    let mut cycle = events.last().unwrap().clone();
    cycle.seq += 1;
    cycle.kind = SessionEventKind::TeamChange {
        change: Box::new(
            TeamChange::task_dependencies_changed(
                &team,
                4,
                lead.id(),
                &TeamTaskId::new("a").unwrap(),
                1,
                vec![TeamTaskId::new("b").unwrap()],
            )
            .unwrap(),
        ),
    };
    events.push(cycle);
    assert!(project_teams(&events).is_err());

    let mut foreign = session.events().to_vec();
    let mut event = foreign.last().unwrap().clone();
    event.seq += 1;
    event.kind = SessionEventKind::TeamChange {
        change: Box::new(
            TeamChange::member_added(&team, 4, worker.id(), member("intruder", TeamRole::Worker))
                .unwrap(),
        ),
    };
    foreign.push(event);
    assert!(project_teams(&foreign).is_err());
}

#[test]
fn mailbox_revision_stress_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let team = TeamId::new("stress").unwrap();
    let lead = member("lead", TeamRole::Lead);
    let worker = member("worker", TeamRole::Worker);
    append(
        &mut session,
        TeamChange::created(team.clone(), lead.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::member_added(&team, 1, lead.id(), worker.clone()).unwrap(),
    );
    let mut revision = 2;
    for index in 0..256_u32 {
        append(
            &mut session,
            TeamChange::message_sent(
                &team,
                revision,
                lead.id(),
                TeamMailboxMessage::new(
                    format!("mail-{index}"),
                    lead.id().clone(),
                    worker.id().clone(),
                    format!("message {index}"),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        revision += 1;
        append(
            &mut session,
            TeamChange::message_claimed(&team, revision, worker.id(), format!("mail-{index}"))
                .unwrap(),
        );
        revision += 1;
    }
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(directory).unwrap();
    let projection = project_teams(reopened.events()).unwrap();
    let view = projection.team(&team).unwrap();
    assert_eq!(view.revision(), revision - 1);
    assert!(view.pending_mail(worker.id()).is_empty());
}

#[test]
fn atomic_roster_and_owner_delivery_claim_survive_reopen_and_reject_forgery() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let team = TeamId::new("delivery").unwrap();
    let lead = member("lead", TeamRole::Lead);
    let worker = member("worker", TeamRole::Worker);
    let reviewer = member("reviewer", TeamRole::Reviewer);
    append(
        &mut session,
        TeamChange::created(team.clone(), lead.clone()).unwrap(),
    );
    append(
        &mut session,
        TeamChange::MembersBootstrapped {
            version: 1,
            team_id: team.clone(),
            revision: 1,
            actor: lead.id().clone(),
            members: vec![worker.clone(), reviewer.clone()],
        },
    );
    append(
        &mut session,
        TeamChange::message_sent(
            &team,
            2,
            lead.id(),
            TeamMailboxMessage::new("mail", lead.id().clone(), worker.id().clone(), "owner mail")
                .unwrap(),
        )
        .unwrap(),
    );
    let delivery = |actor: TeamMemberId| TeamChange::MessageDelivered {
        version: 1,
        team_id: team.clone(),
        revision: 3,
        actor,
        message_id: TeamMessageId::new("mail").unwrap(),
    };
    let mut forged = session.events().to_vec();
    forged.push(heycode_session::SessionEvent {
        v: heycode_session::CURRENT_SESSION_LOG_VERSION,
        seq: forged.len() as u64,
        time_ms: 0,
        kind: SessionEventKind::TeamChange {
            change: Box::new(delivery(reviewer.id().clone())),
        },
    });
    assert!(project_teams(&forged).is_err());
    append(&mut session, delivery(lead.id().clone()));
    append(
        &mut session,
        TeamChange::message_claimed(&team, 4, worker.id(), "mail").unwrap(),
    );
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(directory).unwrap();
    let projection = project_teams(reopened.events()).unwrap();
    let view = projection.team(&team).unwrap();
    assert_eq!(view.members().len(), 3);
    assert!(view.mail()[0].1);
    assert!(view.mail()[0].2);
}
