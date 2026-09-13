//! MCP07 bounded reconnect supervision contracts.
//!
//! A crash loop must exhaust its budget rather than spin, and a recovery that
//! succeeds must replace the live generation exactly once: never two
//! generations, never zero, never a partial tool set. The supervisor owns its
//! retry task and its token, so disposal stops every further attempt.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use heycode_core::Context;
use heycode_mcp::{
    McpChannelError, McpConnectionAttempt, McpConnectionProviderId, McpConnectionPublisher,
    McpConnectionState, McpDefinitionScope, McpFailureCode, McpListChangeWatch, McpReconnect,
    McpReconnectPolicy, McpReconnectSupervisor, McpRecoveryAdmission, McpRecoveryOutcome,
    McpRegistry, McpRequestChannel, McpServerDefinition, McpServerHandshake, McpServerId,
    McpSiblingContributions, McpStreamableHttpTransport, McpToolGenerationOwner, McpToolListLimits,
    McpTransportDefinition,
};
use heycode_tools::ToolRegistry;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// One scripted connect attempt.
enum Attempt {
    /// The transport could not be re-established.
    Connect(McpChannelError),
    /// The transport came back and serves one complete `tools/list` page.
    Serve(&'static [&'static str]),
    /// The transport came back but the tool walk fails.
    Walk(McpChannelError),
    /// The transport hangs until the supervisor's own token is cancelled.
    Blocked,
}

struct AttemptRecord {
    /// Virtual milliseconds since the episode started.
    elapsed_ms: u64,
    /// Tool rows live at the moment this attempt began.
    live_rows: Vec<String>,
}

/// Connector that hands out scripted attempts and records what each observed.
struct ScriptedConnector {
    script: Mutex<VecDeque<Attempt>>,
    log: Mutex<Vec<AttemptRecord>>,
    tools: Arc<ToolRegistry>,
    origin: Instant,
    /// The token the supervisor threaded into the first attempt.
    threaded: Mutex<Option<CancellationToken>>,
}

impl ScriptedConnector {
    fn new(tools: &Arc<ToolRegistry>, script: Vec<Attempt>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into_iter().collect()),
            log: Mutex::new(Vec::new()),
            tools: Arc::clone(tools),
            origin: Instant::now(),
            threaded: Mutex::new(None),
        })
    }

    fn threaded_token_cancelled(&self) -> bool {
        self.threaded
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    fn attempts(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    fn elapsed_ms(&self) -> Vec<u64> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.elapsed_ms)
            .collect()
    }

    fn rows_seen(&self) -> Vec<Vec<String>> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.live_rows.clone())
            .collect()
    }
}

#[async_trait]
impl McpReconnect for ScriptedConnector {
    async fn connect(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<McpConnectionAttempt, McpChannelError> {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        self.threaded
            .lock()
            .unwrap()
            .get_or_insert_with(|| cancellation.clone());
        let next = self.script.lock().unwrap().pop_front();
        self.log.lock().unwrap().push(AttemptRecord {
            elapsed_ms: u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX),
            live_rows: self.tools.names(),
        });
        // An unbounded retry loop overruns the script instead of spinning
        // forever, so removing the attempt bound fails loudly and fast.
        match next.expect("supervisor exceeded its scripted attempt budget") {
            Attempt::Connect(error) => Err(error),
            Attempt::Serve(names) => Ok(McpConnectionAttempt::new(
                ScriptedChannel::serving(names),
                handshake(),
            )),
            Attempt::Walk(error) => Ok(McpConnectionAttempt::new(
                ScriptedChannel::failing(error),
                handshake(),
            )),
            Attempt::Blocked => {
                cancellation.cancelled().await;
                Err(McpChannelError::Cancelled)
            }
        }
    }
}

/// Channel answering exactly one complete `tools/list` page, or one failure.
struct ScriptedChannel(Result<serde_json::Value, McpChannelError>);

impl ScriptedChannel {
    fn serving(names: &[&str]) -> Arc<dyn McpRequestChannel> {
        let rows = names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "description": format!("tool {name}"),
                    "inputSchema": {"type": "object"}
                })
            })
            .collect::<Vec<_>>();
        Arc::new(Self(Ok(serde_json::json!({"tools": rows}))))
    }

    fn failing(error: McpChannelError) -> Arc<dyn McpRequestChannel> {
        Arc::new(Self(Err(error)))
    }
}

#[async_trait]
impl McpRequestChannel for ScriptedChannel {
    async fn call(
        &self,
        _method: &str,
        _params: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<serde_json::Value, McpChannelError> {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        self.0.clone()
    }
}

fn handshake() -> McpServerHandshake {
    McpServerHandshake::from_initialize_result(&serde_json::json!({
        "protocolVersion": "2025-11-25",
        "capabilities": {"tools": {"listChanged": true}},
        "serverInfo": {"name": "fixture", "version": "1.4"}
    }))
    .unwrap()
}

fn policy(max_attempts: u32) -> McpReconnectPolicy {
    McpReconnectPolicy::new(true, 500, 30_000, max_attempts).unwrap()
}

struct Fixture {
    context: Context,
    registry: McpRegistry,
    tools: Arc<ToolRegistry>,
    owner: Arc<McpToolGenerationOwner>,
    publisher: McpConnectionPublisher,
    policy: McpReconnectPolicy,
    parent: CancellationToken,
}

impl Fixture {
    fn new(policy: McpReconnectPolicy) -> Self {
        let context = Context::new();
        let registry = McpRegistry::new();
        let endpoint =
            McpStreamableHttpTransport::new("https://mcp.example.test/mcp", BTreeMap::new())
                .unwrap();
        let definition = McpServerDefinition::new(
            "fixture",
            "Fixture",
            McpDefinitionScope::User,
            McpTransportDefinition::StreamableHttp(endpoint),
        )
        .unwrap()
        .with_reconnect(policy);
        registry.register_definition(&context, definition).unwrap();
        let id = McpServerId::new("fixture").unwrap();
        let publisher = registry
            .register_connection(
                &context,
                &id,
                McpConnectionProviderId::new("streamable-http").unwrap(),
                1,
            )
            .unwrap();
        let tools = Arc::new(ToolRegistry::new());
        let exact = registry.definition(&id).unwrap().unwrap();
        let owner = Arc::new(McpToolGenerationOwner::new(
            &exact,
            Arc::clone(&tools),
            publisher.clone(),
            McpListChangeWatch::new(),
            McpToolListLimits::default(),
        ));
        Self {
            context,
            registry,
            tools,
            owner,
            publisher,
            policy,
            parent: CancellationToken::new(),
        }
    }

    /// Establish the generation the supervisor must protect and then replace.
    async fn establish(&self, names: &[&str]) {
        self.owner
            .refresh(
                ScriptedChannel::serving(names),
                &handshake(),
                McpSiblingContributions::none(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
    }

    fn supervisor(&self, connector: &Arc<ScriptedConnector>) -> McpReconnectSupervisor {
        McpReconnectSupervisor::new(
            Arc::clone(&self.owner),
            self.publisher.clone(),
            Arc::clone(connector) as Arc<dyn McpReconnect>,
            self.policy,
            &self.parent,
        )
    }

    fn last_good_number(&self) -> Option<u64> {
        self.registry.snapshot().unwrap().servers()[0]
            .last_good_generation()
            .map(heycode_mcp::McpConnectionGeneration::number)
    }

    fn state(&self) -> McpConnectionState {
        self.registry.snapshot().unwrap().servers()[0]
            .state()
            .clone()
    }

    fn rows(&self) -> Vec<String> {
        let mut names = self.tools.names();
        names.sort();
        names
    }
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    names.sort();
    names
}

#[tokio::test(start_paused = true)]
async fn a_crash_loop_exhausts_the_attempt_budget_and_removes_the_generation() {
    let fixture = Fixture::new(policy(4));
    fixture.establish(&["alpha", "beta"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        (0..4)
            .map(|_| Attempt::Connect(McpChannelError::Transport))
            .collect(),
    );
    let supervisor = fixture.supervisor(&connector);

    assert!(matches!(
        supervisor.recover(),
        McpRecoveryAdmission::Started
    ));
    let outcome = supervisor.settle().await;

    assert!(
        matches!(outcome, Some(McpRecoveryOutcome::Exhausted { attempts: 4 })),
        "a crash loop must exhaust the exact budget, got {outcome:?}"
    );
    assert_eq!(connector.attempts(), 4);
    assert!(
        matches!(
            fixture.state(),
            McpConnectionState::Failed {
                code: McpFailureCode::ReconnectExhausted,
                ..
            }
        ),
        "got {:?}",
        fixture.state()
    );
    assert_eq!(
        fixture.last_good_number(),
        None,
        "an exhausted reconnect must stop claiming a generation"
    );
    assert!(
        fixture.rows().is_empty(),
        "rows the removed generation accounted for must not stay model-visible"
    );
}

#[tokio::test(start_paused = true)]
async fn a_recovery_swaps_the_live_generation_exactly_once() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha", "beta"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["beta", "gamma"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    let outcome = supervisor.settle().await;

    assert!(
        matches!(
            outcome,
            Some(McpRecoveryOutcome::Recovered {
                attempts: 3,
                generation: 2
            })
        ),
        "recovery must advance the registry generation exactly once, got {outcome:?}"
    );
    assert_eq!(fixture.last_good_number(), Some(2));
    assert_eq!(
        fixture.rows(),
        sorted(&["mcp__fixture__beta", "mcp__fixture__gamma"]),
        "the live rows must be exactly the recovered generation"
    );
    assert!(matches!(fixture.state(), McpConnectionState::Ready { .. }));
}

#[tokio::test(start_paused = true)]
async fn the_previous_rows_stay_live_through_every_failed_attempt() {
    let fixture = Fixture::new(policy(4));
    fixture.establish(&["alpha", "beta"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Walk(McpChannelError::Rpc { code: -32603 }),
            Attempt::Serve(&["gamma"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    supervisor.settle().await;

    let established = sorted(&["mcp__fixture__alpha", "mcp__fixture__beta"]);
    for (index, rows) in connector.rows_seen().into_iter().enumerate() {
        let mut rows = rows;
        rows.sort();
        assert_eq!(
            rows,
            established,
            "attempt {} observed a window without the previous generation",
            index + 1
        );
    }
    assert_eq!(fixture.rows(), sorted(&["mcp__fixture__gamma"]));
    assert_eq!(fixture.last_good_number(), Some(2));
}

#[tokio::test(start_paused = true)]
async fn concurrent_recover_calls_run_one_episode_and_publish_one_generation() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["gamma"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    let admissions: Vec<McpRecoveryAdmission> =
        (0..5).map(|_| supervisor.recover()).collect::<Vec<_>>();
    let outcome = supervisor.settle().await;

    assert_eq!(
        admissions
            .iter()
            .filter(|admission| matches!(admission, McpRecoveryAdmission::Started))
            .count(),
        1,
        "a crash storm must start exactly one recovery episode, got {admissions:?}"
    );
    assert!(
        admissions[1..]
            .iter()
            .all(|admission| matches!(admission, McpRecoveryAdmission::AlreadyRunning)),
        "got {admissions:?}"
    );
    assert!(matches!(
        outcome,
        Some(McpRecoveryOutcome::Recovered {
            attempts: 2,
            generation: 2
        })
    ));
    assert_eq!(fixture.last_good_number(), Some(2));
    assert_eq!(connector.attempts(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_supervisor_waits_the_policy_backoff_between_attempts() {
    let fixture = Fixture::new(policy(5));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        (0..5)
            .map(|_| Attempt::Connect(McpChannelError::Transport))
            .collect(),
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    supervisor.settle().await;

    assert_eq!(
        connector.elapsed_ms(),
        vec![0, 500, 1_500, 3_500, 7_500],
        "attempts must be separated by the doubling policy backoff"
    );
}

#[tokio::test(start_paused = true)]
async fn the_backoff_saturates_at_the_policy_ceiling() {
    let policy = McpReconnectPolicy::new(true, 500, 2_000, 64).unwrap();

    let schedule: Vec<u64> = (1..=8)
        .map(|attempt| {
            u64::try_from(policy.delay_before_attempt(attempt).as_millis()).unwrap_or(u64::MAX)
        })
        .collect();

    assert_eq!(
        schedule,
        vec![0, 500, 1_000, 2_000, 2_000, 2_000, 2_000, 2_000]
    );
    assert_eq!(
        policy.delay_before_attempt(u32::MAX),
        Duration::from_millis(2_000),
        "a huge attempt number must saturate, never overflow"
    );
}

#[tokio::test(start_paused = true)]
async fn a_disposed_supervisor_cancels_its_task_and_makes_no_further_attempts() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["gamma"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    // Let the first attempt run and the backoff wait begin.
    tokio::time::sleep(Duration::from_millis(100)).await;
    supervisor.shutdown();
    let outcome = supervisor.settle().await;
    tokio::time::sleep(Duration::from_millis(60_000)).await;

    assert_eq!(
        connector.attempts(),
        1,
        "a disposed supervisor must not run the attempt waiting behind the backoff"
    );
    assert!(
        matches!(outcome, Some(McpRecoveryOutcome::Cancelled { attempts: 1 })),
        "got {outcome:?}"
    );
    assert_eq!(
        fixture.rows(),
        sorted(&["mcp__fixture__alpha"]),
        "a cancelled recovery must not swap the live generation"
    );
    assert_eq!(fixture.last_good_number(), Some(1));
    assert!(
        connector.threaded_token_cancelled(),
        "disposal must cancel the token threaded into the transport, not only abort the task"
    );
    assert!(matches!(
        supervisor.recover(),
        McpRecoveryAdmission::ShutDown
    ));
}

/// A supervisor whose owning context is already gone admits nothing, even
/// before any episode has run.
#[tokio::test(start_paused = true)]
async fn a_supervisor_whose_parent_is_already_cancelled_admits_no_episode() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(&fixture.tools, vec![Attempt::Serve(&["gamma"])]);
    let supervisor = fixture.supervisor(&connector);
    fixture.parent.cancel();

    let admission = supervisor.recover();
    let outcome = supervisor.settle().await;

    assert!(
        matches!(admission, McpRecoveryAdmission::ShutDown),
        "got {admission:?}"
    );
    assert!(outcome.is_none());
    assert_eq!(connector.attempts(), 0);
    assert_eq!(fixture.rows(), sorted(&["mcp__fixture__alpha"]));
    assert_eq!(fixture.last_good_number(), Some(1));
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_parent_token_settles_the_supervisor_without_publishing() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["gamma"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.parent.cancel();
    let outcome = supervisor.settle().await;
    tokio::time::sleep(Duration::from_millis(60_000)).await;

    assert!(
        matches!(outcome, Some(McpRecoveryOutcome::Cancelled { attempts: 1 })),
        "got {outcome:?}"
    );
    assert_eq!(connector.attempts(), 1);
    assert_eq!(fixture.last_good_number(), Some(1));
    assert!(
        matches!(fixture.state(), McpConnectionState::Reconnecting { .. }),
        "cancellation is not a commit point and must publish no terminal state"
    );
    assert!(matches!(
        supervisor.recover(),
        McpRecoveryAdmission::ShutDown
    ));
}

#[tokio::test(start_paused = true)]
async fn cancelling_an_in_flight_attempt_is_not_an_exhausted_budget() {
    let fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![Attempt::Blocked, Attempt::Serve(&["gamma"])],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    // Cancel while attempt 1 is in flight, not while it waits out a backoff.
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.parent.cancel();
    let outcome = supervisor.settle().await;

    assert!(
        matches!(outcome, Some(McpRecoveryOutcome::Cancelled { attempts: 1 })),
        "a cancelled attempt is not a spent budget, got {outcome:?}"
    );
    assert!(
        matches!(
            fixture.state(),
            McpConnectionState::Reconnecting { attempt: 1, .. }
        ),
        "cancellation must publish no terminal failure, got {:?}",
        fixture.state()
    );
    assert_eq!(fixture.last_good_number(), Some(1));
    assert_eq!(
        fixture.rows(),
        sorted(&["mcp__fixture__alpha"]),
        "a cancelled attempt must not retire the live rows"
    );
    assert_eq!(connector.attempts(), 1);
}

#[tokio::test(start_paused = true)]
async fn exhaustion_is_terminal_and_refuses_further_recovery() {
    let fixture = Fixture::new(policy(2));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Connect(McpChannelError::Transport),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    supervisor.settle().await;
    let admission = supervisor.recover();

    assert!(
        matches!(admission, McpRecoveryAdmission::Exhausted),
        "an exhausted budget must not be re-armed into a slower crash loop, got {admission:?}"
    );
    assert_eq!(connector.attempts(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_successful_recovery_restores_the_whole_attempt_budget() {
    let fixture = Fixture::new(policy(2));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["gamma"]),
            Attempt::Connect(McpChannelError::Transport),
            Attempt::Serve(&["delta"]),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    supervisor.settle().await;
    assert!(matches!(
        supervisor.recover(),
        McpRecoveryAdmission::Started
    ));
    let outcome = supervisor.settle().await;

    assert!(
        matches!(
            outcome,
            Some(McpRecoveryOutcome::Recovered {
                attempts: 2,
                generation: 3
            })
        ),
        "a success resets the consecutive-failure budget, got {outcome:?}"
    );
    assert_eq!(fixture.rows(), sorted(&["mcp__fixture__delta"]));
}

#[tokio::test(start_paused = true)]
async fn a_disabled_reconnect_policy_admits_no_recovery() {
    let disabled = McpReconnectPolicy::new(false, 500, 30_000, 10).unwrap();
    let fixture = Fixture::new(disabled);
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(&fixture.tools, vec![Attempt::Serve(&["gamma"])]);
    let supervisor = fixture.supervisor(&connector);

    let admission = supervisor.recover();
    let outcome = supervisor.settle().await;

    assert!(matches!(admission, McpRecoveryAdmission::Disabled));
    assert!(outcome.is_none());
    assert_eq!(connector.attempts(), 0);
    assert_eq!(fixture.rows(), sorted(&["mcp__fixture__alpha"]));
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_cannot_list_still_spends_its_attempt() {
    let fixture = Fixture::new(policy(2));
    fixture.establish(&["alpha"]).await;
    let connector = ScriptedConnector::new(
        &fixture.tools,
        vec![
            Attempt::Walk(McpChannelError::Rpc { code: -32602 }),
            Attempt::Walk(McpChannelError::Rpc { code: -32602 }),
        ],
    );
    let supervisor = fixture.supervisor(&connector);

    supervisor.recover();
    let outcome = supervisor.settle().await;

    assert!(
        matches!(outcome, Some(McpRecoveryOutcome::Exhausted { attempts: 2 })),
        "a connect that survives but cannot list still spends its attempt, got {outcome:?}"
    );
    assert!(matches!(
        fixture.state(),
        McpConnectionState::Failed {
            code: McpFailureCode::ReconnectExhausted,
            ..
        }
    ));
    assert_eq!(fixture.last_good_number(), None);
}

#[tokio::test(start_paused = true)]
async fn context_shutdown_disposes_the_supervisor_and_its_generation() {
    let mut fixture = Fixture::new(policy(6));
    fixture.establish(&["alpha"]).await;
    // A blocked attempt cannot settle on its own, so only a real disposer can
    // end the retry task and release its hold on the generation owner.
    let connector = ScriptedConnector::new(&fixture.tools, vec![Attempt::Blocked]);
    let supervisor = fixture.supervisor(&connector);
    let held = Arc::clone(&fixture.tools);
    supervisor.recover();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The effect owns the supervisor, exactly as the stdio bridge's effect owns
    // its connection and generation.
    fixture.context.effect(move || drop(supervisor));

    fixture.context.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(fixture.owner);

    assert_eq!(
        connector.attempts(),
        1,
        "a supervisor must not outlive the context that owns it"
    );
    assert!(
        held.names().is_empty(),
        "context shutdown must release the supervisor and drop every MCP row"
    );
}
