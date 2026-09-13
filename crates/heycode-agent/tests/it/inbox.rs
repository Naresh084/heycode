//! A03 — durable inbox admission and deterministic busy/idle wake rules.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex, Weak};

use heycode_agent::{Agent, AutoApprove, InboxWake, UiEvent};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_session::{InboxDelivery, Session, SessionEventKind};

use super::turn::{World, build_provider_in, script_text};

/// One deterministic action performed from inside a provider stream.
type MidTurnHook = Arc<Mutex<Option<Box<dyn Fn(&Agent) + Send>>>>;

/// Provider that runs one deterministic callback while the turn is inside the
/// stream, which is the only honest way to prove "arrived while busy".
struct MidTurn {
    inner: FakeProvider,
    sink: Arc<Mutex<Vec<ChatRequest>>>,
    agent: Arc<Mutex<Weak<Agent>>>,
    during: MidTurnHook,
}

impl Provider for MidTurn {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.sink.lock().unwrap().push(request.clone());
        let action = self.during.lock().unwrap().take();
        if let Some(action) = action {
            let agent = self.agent.lock().unwrap().upgrade();
            if let Some(agent) = agent {
                action(&agent);
            }
        }
        self.inner.stream(request)
    }
}

struct MidTurnWorld {
    world: World,
    during: MidTurnHook,
}

fn build_mid_turn(scripts: Vec<Vec<StreamChunk>>) -> MidTurnWorld {
    let dir = tempfile::tempdir().unwrap();
    let agent_slot: Arc<Mutex<Weak<Agent>>> = Arc::new(Mutex::new(Weak::new()));
    let during: MidTurnHook = Arc::new(Mutex::new(None));
    let slot = agent_slot.clone();
    let hook = during.clone();
    let world = build_provider_in(
        dir,
        move |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(scripts),
                sink,
                agent: slot,
                during: hook,
            })
        },
        Arc::new(AutoApprove),
    );
    // Weak, so the provider the registry owns cannot keep the Agent alive.
    *agent_slot.lock().unwrap() = Arc::downgrade(&world.agent);
    MidTurnWorld { world, during }
}

fn wakes(log: &Arc<Mutex<Vec<UiEvent>>>) -> Vec<InboxWake> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            UiEvent::InboxUpdated { wake, .. } => Some(*wake),
            _ => None,
        })
        .collect()
}

fn user_messages(session: &Arc<Mutex<heycode_session::Session>>) -> Vec<String> {
    session
        .lock()
        .unwrap()
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn idle_follow_up_and_steer_wake_while_inject_stays_queued() {
    let world = build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![script_text("ok")]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    );
    assert!(!world.agent.token().is_turn_active());

    let (_, follow_up) = world
        .agent
        .submit_inbox(InboxDelivery::FollowUp, "run the suite")
        .unwrap();
    let (_, steer) = world
        .agent
        .submit_inbox(InboxDelivery::Steer, "also check lints")
        .unwrap();
    let (inject_id, inject) = world
        .agent
        .submit_inbox(InboxDelivery::Inject, "context note")
        .unwrap();

    assert_eq!(follow_up, InboxWake::Wake, "follow-up wakes an idle agent");
    assert_eq!(steer, InboxWake::Wake, "steer wakes an idle agent");
    assert_eq!(
        inject,
        InboxWake::Queued,
        "inject must never wake an idle agent"
    );
    assert_eq!(
        wakes(&world.ui_log),
        vec![InboxWake::Wake, InboxWake::Wake, InboxWake::Queued]
    );

    let pending = world.agent.pending_inbox();
    assert_eq!(pending.next_turn, 1);
    assert_eq!(
        pending.next_step, 2,
        "steer and inject share the step queue"
    );

    // Nothing is model-visible until a claim admits it.
    assert!(user_messages(&world.session).is_empty());

    assert!(world.agent.cancel_inbox(&inject_id).unwrap());
    assert!(
        !world.agent.cancel_inbox(&inject_id).unwrap(),
        "a settled id cancels idempotently rather than failing"
    );
    assert_eq!(world.agent.pending_inbox().next_step, 1);
    assert!(user_messages(&world.session).is_empty());
}

#[tokio::test]
async fn work_arriving_while_busy_never_wakes_and_enters_the_running_turn() {
    let world = build_mid_turn(vec![script_text("first"), script_text("second")]);
    *world.during.lock().unwrap() = Some(Box::new(|agent: &Agent| {
        assert!(
            agent.token().is_turn_active(),
            "the hook must observe an active turn"
        );
        let (_, steer) = agent
            .submit_inbox(InboxDelivery::Steer, "steered mid-turn")
            .unwrap();
        assert_eq!(
            steer,
            InboxWake::Queued,
            "a busy agent is never woken; the running turn drains it"
        );
        let (_, follow_up) = agent
            .submit_inbox(InboxDelivery::FollowUp, "queued for later")
            .unwrap();
        assert_eq!(follow_up, InboxWake::Queued);
    }));

    world.world.agent.send("start").await.unwrap();

    // The steer arrived after the only step boundary, so the pre-settlement
    // re-drain is what keeps it from being stranded: it opens a second step.
    let messages = user_messages(&world.world.session);
    assert_eq!(
        messages,
        vec!["start".to_owned(), "steered mid-turn".to_owned()]
    );
    let requests = world.world.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the claimed steer owes another step");
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.content.contains("steered mid-turn")),
        "claimed text must be model-visible in the next request"
    );
    drop(requests);

    // The follow-up is still pending, and settlement announces the owed wake.
    assert_eq!(world.world.agent.pending_inbox().next_turn, 1);
    assert_eq!(
        wakes(&world.world.ui_log).last(),
        Some(&InboxWake::Wake),
        "settlement must announce the follow-up a busy submission did not wake"
    );
}

#[tokio::test]
async fn claim_is_atomic_and_a_follow_up_turn_takes_exactly_one_message() {
    let world = build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![script_text("done")]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    );
    world
        .agent
        .submit_inbox(InboxDelivery::FollowUp, "first follow-up")
        .unwrap();
    world
        .agent
        .submit_inbox(InboxDelivery::FollowUp, "second follow-up")
        .unwrap();

    world
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        world.agent.pending_inbox().next_turn,
        1,
        "exactly one queued input opens each new turn"
    );
    assert_eq!(
        user_messages(&world.session),
        vec!["first follow-up".to_owned()]
    );

    // The claim splice and its admission are one atomic pair, in that order.
    let session = world.session.lock().unwrap();
    let kinds: Vec<&'static str> = session
        .events()
        .iter()
        .map(|event| event.kind.name())
        .collect();
    let splice = kinds
        .iter()
        .rposition(|kind| *kind == "agent/inbox/splice")
        .unwrap();
    assert_eq!(kinds[splice + 1], "user/message");
    assert_eq!(kinds[splice + 2], "turn/start");
}

#[tokio::test]
async fn an_empty_follow_up_turn_is_refused_without_touching_the_log() {
    let world = build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![script_text("unused")]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    );
    let before = world.session.lock().unwrap().events().len();
    let error = world
        .agent
        .send_follow_up_cancellable(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no follow-up input is pending"));
    assert_eq!(world.session.lock().unwrap().events().len(), before);
    assert!(world.requests.lock().unwrap().is_empty());
    assert!(
        !world.agent.token().is_turn_active(),
        "a refused follow-up must release its turn lease"
    );
}

// ---------------------------------------------------------------------------
// O04 — background jobs: settlement notices and the wake budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn settled_jobs_deliver_durable_notices_without_an_implicit_wake_budget() {
    use heycode_agent::{JobOutcome, JobRegistry, JobSettlement, WakeDecision};

    let world = build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![script_text("ok")]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    );
    // The composed world already provides the registry, so this exercises the
    // production wiring rather than a hand-built one.
    let jobs = world
        .ctx
        .get::<Arc<JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .expect("job registry must be a composed service");
    let token = tokio_util::sync::CancellationToken::new();
    let mut ids = Vec::new();
    for index in 0..2 {
        ids.push(
            jobs.admit(
                format!("scan-{index}"),
                InboxDelivery::FollowUp,
                token.clone(),
                tokio::spawn(async {}),
            )
            .unwrap(),
        );
    }

    // Both settlements may wake the agent under the unlimited default.
    let first = world
        .agent
        .settle_job(
            &jobs,
            &ids[0],
            &JobSettlement::new(JobOutcome::Completed, "indexed 12 files").unwrap(),
        )
        .unwrap();
    assert_eq!(first, WakeDecision::Woke);
    assert_eq!(world.agent.pending_inbox().next_turn, 1);

    // A later settlement is not demoted by an implicit allowance.
    let second = world
        .agent
        .settle_job(
            &jobs,
            &ids[1],
            &JobSettlement::new(JobOutcome::Failed, "scan hit a permission error").unwrap(),
        )
        .unwrap();
    assert_eq!(second, WakeDecision::Woke);
    assert_eq!(
        world.agent.pending_inbox().next_turn,
        2,
        "both notices remain in the waking queue"
    );
    assert_eq!(world.agent.pending_inbox().next_step, 0);
    assert_eq!(jobs.demoted_wakes(), 0);

    // Both notices are durable and neither is model-visible before a claim.
    assert!(user_messages(&world.session).is_empty());

    // A settled job cannot settle twice.
    assert!(
        world
            .agent
            .settle_job(
                &jobs,
                &ids[0],
                &JobSettlement::new(JobOutcome::Cancelled, "late").unwrap()
            )
            .is_err()
    );

    // A subsequent turn does not change this unlimited behavior.
    world.agent.send("do something").await.unwrap();
    let third = jobs
        .admit(
            "scan-2",
            InboxDelivery::FollowUp,
            token,
            tokio::spawn(async {}),
        )
        .unwrap();
    assert_eq!(
        world
            .agent
            .settle_job(
                &jobs,
                &third,
                &JobSettlement::new(JobOutcome::Completed, "second pass done").unwrap()
            )
            .unwrap(),
        WakeDecision::Woke,
        "later settlements must also wake"
    );
}

// ---------------------------------------------------------------------------
// C12 — compaction strategies and the verified transaction
// ---------------------------------------------------------------------------

/// A strategy whose reported outcome and durable effect can be made to
/// disagree, so the registry's transaction check is observable.
struct ScriptedCompaction {
    descriptor: heycode_agent::CompactionStrategyDescriptor,
    plan: heycode_agent::CompactionPlan,
    session: Arc<std::sync::Mutex<Session>>,
    /// Forbidden durable events to append before returning.
    writes: usize,
}

#[async_trait::async_trait]
impl heycode_agent::CompactionStrategy for ScriptedCompaction {
    fn descriptor(&self) -> &heycode_agent::CompactionStrategyDescriptor {
        &self.descriptor
    }

    async fn prepare(
        &self,
        _context: &heycode_agent::CompactionContext,
        _keep_recent_turns: u64,
        _cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<heycode_agent::CompactionPlan, heycode_agent::CompactionError> {
        for index in 0..self.writes {
            let mut session = self.session.lock().unwrap();
            session
                .append(SessionEventKind::CompactionApplied {
                    summary: format!("scripted {index}"),
                    replaced_upto_seq: 0,
                })
                .unwrap();
        }
        Ok(self.plan.clone())
    }
}

fn scripted(
    id: &str,
    plan: heycode_agent::CompactionPlan,
    session: Arc<std::sync::Mutex<Session>>,
    writes: usize,
) -> Arc<dyn heycode_agent::CompactionStrategy> {
    Arc::new(ScriptedCompaction {
        descriptor: heycode_agent::CompactionStrategyDescriptor::new(
            heycode_agent::CompactionStrategyId::new(id).unwrap(),
            heycode_agent::CompactionKind::Prune,
        ),
        plan,
        session,
        writes,
    })
}

fn compaction_world() -> World {
    build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![script_text("ok")]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    )
}

#[tokio::test]
async fn the_registry_verifies_the_compaction_transaction_rather_than_trusting_it() {
    use heycode_agent::{
        CompactionError, CompactionOutcome, CompactionPlan, CompactionRegistry,
        CompactionReplacement,
    };

    let world = compaction_world();
    let registry = world
        .ctx
        .get::<CompactionRegistry>(heycode_agent::SERVICE_COMPACTIONS)
        .unwrap();
    world
        .session
        .lock()
        .unwrap()
        .append(SessionEventKind::UserMessage {
            text: "transaction seed".to_owned(),
        })
        .unwrap();
    let cancellation = tokio_util::sync::CancellationToken::new();
    let id = |name: &str| heycode_agent::CompactionStrategyId::new(name).unwrap();

    // Honest preparation writes nothing; the registry commits once.
    registry
        .register(scripted(
            "honest-applied",
            CompactionPlan::Applied {
                folded: 1,
                replaced_upto_seq: 0,
                replacement: CompactionReplacement::Summary("prepared summary".to_owned()),
            },
            world.session.clone(),
            0,
        ))
        .unwrap();
    assert_eq!(
        world
            .agent
            .compact("honest-applied", 2, cancellation.clone())
            .await
            .unwrap(),
        CompactionOutcome::Applied {
            strategy: id("honest-applied"),
            folded: 1
        }
    );

    // An invalid prefix proposal cannot reach the commit point.
    registry
        .register(scripted(
            "lying-applied",
            CompactionPlan::Applied {
                folded: 3,
                replaced_upto_seq: 99,
                replacement: CompactionReplacement::Summary("invalid".to_owned()),
            },
            world.session.clone(),
            0,
        ))
        .unwrap();
    assert!(matches!(
        world
            .agent
            .compact("lying-applied", 2, cancellation.clone())
            .await
            .unwrap_err(),
        CompactionError::BrokenTransaction { .. }
    ));

    // Reports a no-op but mutated the log — a fold nobody was told about.
    registry
        .register(scripted(
            "lying-noop",
            CompactionPlan::Noop {
                reason: "nothing to do",
            },
            world.session.clone(),
            1,
        ))
        .unwrap();
    assert!(matches!(
        world
            .agent
            .compact("lying-noop", 2, cancellation.clone())
            .await
            .unwrap_err(),
        CompactionError::BrokenTransaction { .. }
    ));

    // Applied twice in one run is also a broken transaction.
    registry
        .register(scripted(
            "double-applied",
            CompactionPlan::Noop {
                reason: "malicious writes",
            },
            world.session.clone(),
            2,
        ))
        .unwrap();
    assert!(matches!(
        world
            .agent
            .compact("double-applied", 2, cancellation)
            .await
            .unwrap_err(),
        CompactionError::BrokenTransaction { .. }
    ));
}

#[tokio::test]
async fn registration_selection_and_disposal_are_explicit() {
    use heycode_agent::{CompactionError, CompactionKind, CompactionRegistry};

    let registry = CompactionRegistry::new();

    registry
        .register(Arc::new(heycode_agent::PortableCompaction::new().unwrap()))
        .unwrap();
    registry
        .register(Arc::new(heycode_agent::PruneCompaction::new().unwrap()))
        .unwrap();

    // Duplicate registration fails loud rather than shadowing.
    assert!(matches!(
        registry
            .register(Arc::new(heycode_agent::PortableCompaction::new().unwrap()))
            .unwrap_err(),
        CompactionError::Duplicate(_)
    ));

    let descriptors = registry.descriptors();
    assert_eq!(descriptors.len(), 2, "ordered by id");
    assert_eq!(descriptors[0].id().as_str(), "portable-summary");
    assert_eq!(descriptors[0].kind(), CompactionKind::Portable);
    assert_eq!(descriptors[1].id().as_str(), "prune-oldest");

    let world = compaction_world();
    // An unknown strategy is named, never silently defaulted to another.
    assert!(matches!(
        world
            .agent
            .compact(
                "does-not-exist",
                2,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_err(),
        CompactionError::Unknown(_)
    ));

    // A cancelled request settles before the strategy runs.
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        world
            .agent
            .compact("prune-oldest", 2, cancelled)
            .await
            .unwrap_err(),
        CompactionError::Cancelled
    ));

    registry.dispose();
    assert!(registry.descriptors().is_empty());
}

#[tokio::test]
async fn prune_folds_at_the_same_boundary_as_portable_and_says_it_dropped_history() {
    use heycode_agent::{CompactionKind, CompactionOutcome, PRUNE_MARKER};

    let world = build_provider_in(
        tempfile::tempdir().unwrap(),
        |sink| {
            Arc::new(MidTurn {
                inner: FakeProvider::new(vec![
                    script_text("first"),
                    script_text("second"),
                    script_text("third"),
                ]),
                sink,
                agent: Arc::new(Mutex::new(Weak::new())),
                during: Arc::new(Mutex::new(None)),
            })
        },
        Arc::new(AutoApprove),
    );
    for turn in ["one", "two", "three"] {
        world.agent.send(turn).await.unwrap();
    }

    assert!(
        world
            .agent
            .compaction_strategies()
            .iter()
            .any(|descriptor| descriptor.kind() == CompactionKind::Prune)
    );

    let outcome = world
        .agent
        .compact(
            "prune-oldest",
            1,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let CompactionOutcome::Applied { folded, .. } = outcome else {
        panic!("expected an applied prune")
    };
    assert!(folded > 0);

    // The fold is recorded as a dropped span, not as a summary the model could
    // mistake for content it produced.
    // The guard is scoped so it cannot be held across the await below — the
    // session mutex is std, not tokio.
    let (applied, before) = {
        let session = world.session.lock().unwrap();
        let applied: Vec<String> = session
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::CompactionApplied { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .collect();
        let before = session.events().len();
        (applied, before)
    };
    assert_eq!(applied.len(), 1, "exactly one fold was committed");
    assert_eq!(applied[0], PRUNE_MARKER);
    assert!(
        PRUNE_MARKER.contains("not summarized"),
        "the record must state that history was dropped, not summarized"
    );

    // A second prune inside the keep window is a no-op that writes nothing.
    let repeat = world
        .agent
        .compact(
            "prune-oldest",
            64,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(repeat, CompactionOutcome::Noop { .. }));
    assert_eq!(world.session.lock().unwrap().events().len(), before);
}

#[tokio::test]
async fn exact_follow_up_id_never_claims_a_different_pending_message() {
    let owner = build_mid_turn(vec![
        script_text("first reply"),
        script_text("second reply"),
    ]);
    let agent = &owner.world.agent;
    let (first, _) = agent
        .submit_inbox(InboxDelivery::FollowUp, "first input")
        .unwrap();
    let (second, _) = agent
        .submit_inbox(InboxDelivery::FollowUp, "second input")
        .unwrap();
    assert!(
        agent
            .send_follow_up_id_cancellable(&second, tokio_util::sync::CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(agent.pending_inbox().next_turn, 2);
    agent
        .send_follow_up_id_cancellable(&first, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let consumed = agent
        .send_follow_up_id_cancellable(&first, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        consumed.downcast_ref::<heycode_agent::FollowUpError>(),
        Some(heycode_agent::FollowUpError::Empty)
    ));
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(
        agent
            .send_follow_up_id_cancellable(&second, cancelled)
            .await
            .is_err()
    );
    assert_eq!(agent.pending_inbox().next_turn, 1);
    agent
        .send_follow_up_id_cancellable(&second, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        user_messages(&owner.world.session),
        vec!["first input", "second input"]
    );
}
