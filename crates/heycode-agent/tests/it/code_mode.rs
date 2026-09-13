//! Real JavaScript calls through a composed Agent and its durable session.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::turn::{build, script_text};
use heycode_agent::{ApprovalPolicy, AutoApprove};
use heycode_llm::{FinishReason, StreamChunk};
use heycode_session::{CodeModeChange, Session, SessionEventKind, project_code_mode};
use heycode_tools::{Tool, ToolCallInput, ToolCtx, ToolError, Verdict};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Proof {
    session: Arc<Mutex<Session>>,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Tool for Proof {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "proof".into(),
            description: "Prove durable intent before execution".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn run(&self, args: Value, _cx: &ToolCtx) -> Result<Value, ToolError> {
        assert!(self.session.lock().unwrap().events().iter().any(|e| matches!(&e.kind,SessionEventKind::CodeModeChange{change} if matches!(change.as_ref(),CodeModeChange::CallStarted{name,..} if name=="proof"))));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(args)
    }
}
struct DenyProof;
#[async_trait::async_trait]
impl ApprovalPolicy for DenyProof {
    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        if call.name == "proof" {
            Verdict::Deny {
                reason: "child read-only ceiling".into(),
            }
        } else {
            Verdict::Allow
        }
    }
}
fn call(id: &str, args: Value) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some("run_code".into()),
            arguments_delta: args.to_string(),
        },
        StreamChunk::Finish(FinishReason::ToolCalls),
    ]
}

#[tokio::test]
async fn code_mode_nested_intent_precedes_effect_and_provider_pairing_stays_valid() {
    let mut world = build(
        vec![
            call(
                "script",
                json!({"source":"const value=await tools.proof({n:7}); text(value); return value.n*2;","tools":["proof"]}),
            ),
            script_text("done"),
        ],
        Arc::new(AutoApprove),
    );
    heycode_agent::code_mode_plugin()
        .apply(&mut world.ctx)
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .register_shared(Arc::new(Proof {
            session: world.session.clone(),
            calls: calls.clone(),
        }))
        .unwrap();
    world.agent.send("execute the script").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let session = world.session.lock().unwrap();
    let reopened = Session::open(session.path().parent().unwrap()).unwrap();
    let projection = project_code_mode(reopened.events()).unwrap();
    let run = projection.runs.values().next().unwrap();
    assert!(run.settled && run.calls[&0].settled);
    assert_eq!(run.result.as_ref().unwrap()["value"], 14);
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::ToolCall { .. }))
            .count(),
        1
    );
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::ToolResult { .. }))
            .count(),
        1
    );
    let requests = world.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .any(|text| text.contains("UNTRUSTED TOOL ORCHESTRATION"))
    );
}
#[tokio::test]
async fn code_mode_outer_approval_does_not_grant_inner_tool_authority() {
    let mut world = build(
        vec![
            call(
                "script",
                json!({"source":"return await tools.proof({});","tools":["proof"]}),
            ),
            script_text("refused"),
        ],
        Arc::new(DenyProof),
    );
    heycode_agent::code_mode_plugin()
        .apply(&mut world.ctx)
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    world
        .ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap()
        .register_shared(Arc::new(Proof {
            session: world.session.clone(),
            calls: calls.clone(),
        }))
        .unwrap();
    world.agent.send("execute the script").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let session = world.session.lock().unwrap();
    let projection = project_code_mode(session.events()).unwrap();
    let run = projection.runs.values().next().unwrap();
    assert!(
        run.error
            .as_ref()
            .unwrap()
            .contains("child read-only ceiling")
    );
    assert!(run.calls[&0].settled);
    assert!(run.calls[&0].value.is_none());
}
#[tokio::test]
async fn code_mode_saved_script_is_explicitly_reusable() {
    let mut world = build(
        vec![
            call(
                "save",
                json!({"action":"save","name":"sum","source":"return [1,2,3].reduce((a,b)=>a+b,0);"}),
            ),
            call("run", json!({"action":"run_saved","name":"sum"})),
            script_text("six"),
        ],
        Arc::new(AutoApprove),
    );
    heycode_agent::code_mode_plugin()
        .apply(&mut world.ctx)
        .unwrap();
    world.agent.send("save and run sum").await.unwrap();
    let session = world.session.lock().unwrap();
    let reopened = Session::open(session.path().parent().unwrap()).unwrap();
    let projection = project_code_mode(reopened.events()).unwrap();
    assert_eq!(projection.saved.len(), 1);
    assert_eq!(projection.runs.len(), 1);
    assert_eq!(
        projection
            .runs
            .values()
            .next()
            .unwrap()
            .result
            .as_ref()
            .unwrap()["value"],
        6
    );
}

struct PauseProbe {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    second: Arc<tokio::sync::Notify>,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Tool for PauseProbe {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: "pause_probe".into(),
            description: "Controlled script call".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn run(&self, _: Value, _: &ToolCtx) -> Result<Value, ToolError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.entered.notify_one();
            self.release.notified().await;
        } else {
            self.second.notify_one();
        }
        Ok(json!({"n":call + 1}))
    }
}

#[tokio::test]
async fn code_mode_live_pause_resume_and_stop_preserve_state_and_session_ownership() {
    use std::time::Duration;
    for stop in [false, true] {
        let mut world = build(
            vec![
                call(
                    "script",
                    json!({
                        "source":"let total=10; total+=(await tools.pause_probe({})).n; total+=(await tools.pause_probe({})).n; return total;",
                        "tools":["pause_probe"]
                    }),
                ),
                script_text("settled"),
            ],
            Arc::new(AutoApprove),
        );
        heycode_agent::code_mode_plugin()
            .apply(&mut world.ctx)
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let second = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        world
            .ctx
            .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
            .unwrap()
            .register_shared(Arc::new(PauseProbe {
                entered: entered.clone(),
                release: release.clone(),
                second: second.clone(),
                calls: calls.clone(),
            }))
            .unwrap();
        let agent = world.agent.clone();
        let running = tokio::spawn(async move { agent.send("run controlled script").await });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let id = project_code_mode(world.session.lock().unwrap().events())
            .unwrap()
            .runs
            .keys()
            .next()
            .unwrap()
            .clone();
        let command = world
            .ctx
            .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
            .unwrap()
            .get("scripts")
            .unwrap()
            .unwrap();
        assert_eq!(
            command.descriptor().timing(),
            heycode_agent::CommandTiming::Immediate
        );
        let other = build(vec![], Arc::new(AutoApprove));
        assert!(
            command
                .execute(&other.agent, &format!("stop {id}"))
                .await
                .is_err()
        );
        command
            .execute(&world.agent, &format!("pause {id}"))
            .await
            .unwrap();
        release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_millis(75), second.notified())
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        command
            .execute(
                &world.agent,
                &format!("{} {id}", if stop { "stop" } else { "resume" }),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let projection = project_code_mode(world.session.lock().unwrap().events()).unwrap();
        let run = &projection.runs[&id];
        assert!(run.settled);
        if stop {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(run.error.is_some());
        } else {
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(run.result.as_ref().unwrap()["value"], 13);
        }
        assert!(
            command
                .execute(&world.agent, &format!("resume {id}"))
                .await
                .is_err()
        );
    }
}
