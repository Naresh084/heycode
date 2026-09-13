//! Actual native message execution: retained sessions, provenance and reply ownership.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_agent::{
    Agent, AgentOptions, AutoApprove, SubagentAuthority, SubagentContinuation, SubagentId,
    SubagentRegistry, SubagentRequest, SubagentSeed,
};
use heycode_core::{Plugin, compose};
use heycode_llm::{
    ChatRequest, ChunkStream, FinishReason, LlmSelection, Provider, ProviderInfo, Role, StreamChunk,
};
use heycode_session::{InboxSource, SessionEventKind};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const REQUEST: &str = "PEER_REQUEST: inspect the retained evidence";
const ANSWER: &str = "RECIPIENT_ANSWER: retained evidence verified";
const ACCEPTED: &str = "SENDER_ACCEPTED_PEER_RESULT";

struct MessagingProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    recipient_id: Arc<Mutex<Option<String>>>,
    to: &'static str,
}
fn stop(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.into()),
        StreamChunk::Finish(FinishReason::Stop),
    ]
}
fn tool(index: u16, id: &str, name: &str, arguments: serde_json::Value) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index,
        id: Some(id.into()),
        name: Some(name.into()),
        arguments_delta: arguments.to_string(),
    }
}
impl Provider for MessagingProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "native-message-fixture".into(),
            default_model: "retained-model".into(),
        }
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.requests.lock().unwrap().push(request.clone());
        let user_has = |text: &str| {
            request
                .messages
                .iter()
                .any(|message| message.role == Role::User && message.content.contains(text))
        };
        let chunks = if user_has("RECIPIENT_SEED") {
            if user_has(REQUEST) {
                stop(ANSWER)
            } else {
                stop("recipient ready")
            }
        } else if user_has("SENDER_SEED") {
            if user_has(ANSWER) {
                stop(ACCEPTED)
            } else if request
                .messages
                .iter()
                .any(|message| message.tool_call_id.as_deref() == Some("peer-send"))
            {
                stop("sender idle until the peer result arrives")
            } else {
                vec![
                    tool(
                        0,
                        "peer-send",
                        "send_message",
                        json!({"to":self.to,"message":REQUEST}),
                    ),
                    // Messaging a peer or parent must not grant lifecycle control.
                    tool(
                        1,
                        "foreign-control",
                        "agent_control",
                        json!({"action":"interrupt","agent_id":self.recipient_id.lock().unwrap().clone().unwrap()}),
                    ),
                    StreamChunk::Finish(FinishReason::ToolCalls),
                ]
            }
        } else {
            panic!(
                "The root must not consume a sibling/direct-parent reply: {:?}",
                request.messages
            );
        };
        Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
    }
}
fn world(root: &std::path::Path, provider: Arc<MessagingProvider>) -> heycode_core::Context {
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_session::session_plugin(root.to_path_buf()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                root.to_path_buf(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_llm::llm_plugin(
            LlmSelection {
                provider_name: "native-message-fixture".into(),
                model: "retained-model".into(),
            },
            vec![provider],
        ),
        heycode_agent::approval_plugin(Arc::new(AutoApprove)),
        heycode_agent::commands_plugin(),
        heycode_agent::compactions_plugin(),
        heycode_agent::subagent_plugin(root.to_path_buf(), 4),
        heycode_agent::plan_plugin(),
        heycode_agent::agent_options_plugin(AgentOptions::default()),
        heycode_agent::agent_plugin(),
        heycode_agent::subagent_jobs_plugin(),
    ];
    compose(&plugins).unwrap()
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn request(name: &str, prompt: &str, authority: SubagentAuthority) -> SubagentRequest {
    SubagentRequest::with_authority(
        name,
        prompt,
        SubagentSeed::Fresh,
        SubagentContinuation::Continuable,
        authority,
    )
    .unwrap()
}
fn inserted(agent: &Agent) -> Vec<heycode_session::InboxMessage> {
    agent
        .session()
        .lock()
        .unwrap()
        .events()
        .iter()
        .flat_map(|event| match &event.kind {
            SessionEventKind::AgentInboxSplice { inserted, .. } => inserted.clone(),
            _ => vec![],
        })
        .collect()
}

async fn native_message_round_trip(nested: bool) {
    let root = tempfile::tempdir().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recipient_id = Arc::new(Mutex::new(None));
    let mut ctx = world(
        root.path(),
        Arc::new(MessagingProvider {
            requests: requests.clone(),
            recipient_id: recipient_id.clone(),
            to: if nested { "parent" } else { "Boreal" },
        }),
    );
    let root_agent = ctx.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    let registry = ctx
        .get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let owner = SubagentId::new(root_agent.session().lock().unwrap().id().as_str()).unwrap();
    let authority = registry.root_authority(owner.clone());
    let recipient_name = if nested { "Branch" } else { "Boreal" };
    let recipient = registry
        .start(
            request(recipient_name, "RECIPIENT_SEED", authority.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    *recipient_id.lock().unwrap() = Some(recipient.id.to_string());
    let recipient_agent = registry
        .native_child_for(&authority, &recipient.id)
        .unwrap();
    let retained_session = recipient_agent.session().lock().unwrap().id().clone();
    let sender_owner = if nested {
        registry
            .authority_for_child(&authority, &recipient.id)
            .unwrap()
    } else {
        authority.clone()
    };
    let sender_name = if nested { "Leaf" } else { "Atlas" };
    let sender = registry
        .start(
            request(sender_name, "SENDER_SEED", sender_owner.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let sender_agent = registry
        .native_child_for(&sender_owner, &sender.id)
        .unwrap();
    until(|| sender_agent.session().lock().unwrap().events().iter().any(|event| matches!(&event.kind, SessionEventKind::AssistantMessage { content, .. } if content == ACCEPTED))).await;
    let jobs = ctx
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    until(|| {
        jobs.list()
            .iter()
            .all(|job| matches!(job.state, heycode_agent::JobState::Settled(_)))
            && !sender_agent.token().is_turn_active()
            && !recipient_agent.token().is_turn_active()
    })
    .await;

    assert!(Arc::ptr_eq(
        &recipient_agent,
        &registry
            .native_child_for(&authority, &recipient.id)
            .unwrap()
    ));
    assert_eq!(
        *recipient_agent.session().lock().unwrap().id(),
        retained_session
    );
    let incoming = inserted(&recipient_agent);
    let directed: Vec<_> = incoming
        .iter()
        .filter(|message| message.text().contains(REQUEST))
        .collect();
    assert_eq!(directed.len(), 1);
    assert!(
        matches!(directed[0].source(), InboxSource::Agent { agent_id, agent_name, recipient_id, completion_id:None, .. } if agent_id == sender.id.as_str() && agent_name == sender_name && recipient_id == recipient.id.as_str())
    );
    let replies: Vec<_> = inserted(&sender_agent)
        .into_iter()
        .filter(|message| {
            matches!(
                message.source(),
                InboxSource::Agent {
                    completion_id: Some(_),
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        replies.len(),
        1,
        "one real recipient result; no extra consumed-message acknowledgement"
    );
    assert!(replies[0].text().contains(ANSWER));
    assert!(
        matches!(replies[0].source(), InboxSource::Agent { agent_id, agent_name, recipient_id, completion_id:Some(_), .. } if agent_id == recipient.id.as_str() && agent_name == recipient_name && recipient_id == sender.id.as_str())
    );
    assert!(
        inserted(&root_agent).is_empty(),
        "the root does not own this directed reply"
    );
    assert_eq!(
        root_agent.pending_inbox().next_step + root_agent.pending_inbox().next_turn,
        0
    );
    let sender_events = sender_agent.session().lock().unwrap().events().to_vec();
    let control_result = sender_events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolResult {
                call_id, content, ..
            } if call_id.as_str() == "foreign-control" => Some(content),
            _ => None,
        })
        .expect("the native sender must execute the foreign-control probe");
    let control: serde_json::Value = serde_json::from_str(control_result)
        .expect("canonical agent control returns structured metadata");
    assert_eq!(
        control["agent_id"],
        recipient.id.as_str(),
        "{control_result}"
    );
    assert_eq!(
        control["requested"], false,
        "peer messaging must not grant interrupt authority: {control_result}"
    );
    assert_eq!(
        control["status"], "not_found",
        "foreign ownership remains indistinguishable from absence"
    );
    let sender_authority = registry
        .authority_for_child(&sender_owner, &sender.id)
        .unwrap();
    assert!(
        registry
            .child_for(&sender_authority, &recipient.id)
            .is_none()
    );
    assert!(!recipient_agent.token().is_cancelled());
    assert_eq!(
        registry
            .task_snapshots_for(&authority)
            .iter()
            .find(|row| row.id == recipient.id.as_str())
            .unwrap()
            .state,
        heycode_agent::TaskState::Idle
    );
    assert_eq!(recipient_agent.session().lock().unwrap().events().iter().filter(|event| matches!(&event.kind, SessionEventKind::UserMessage { text } if text.contains(REQUEST))).count(), 1);
    assert_eq!(sender_events.iter().filter(|event| matches!(&event.kind, SessionEventKind::UserMessage { text } if text.contains(ANSWER))).count(), 1);
    let recorded = requests.lock().unwrap();
    assert_eq!(
        recorded
            .iter()
            .filter(|request| request
                .messages
                .iter()
                .any(|message| message.content == "RECIPIENT_SEED"))
            .count(),
        2,
        "recipient initial and directed-message turn only"
    );
    assert!(
        recorded
            .iter()
            .all(|request| request.model == "retained-model")
    );
    assert!(
        recorded.iter().any(|request| request
            .messages
            .iter()
            .any(|message| message.role == Role::User && message.content.contains(REQUEST))
            && request
                .messages
                .iter()
                .any(|message| message.content == "RECIPIENT_SEED")),
        "delivery retains the original native conversation context"
    );
    drop(recorded);
    ctx.shutdown();
}

#[tokio::test]
async fn native_sibling_message_resumes_same_session_and_returns_result_to_sender() {
    native_message_round_trip(false).await;
}

#[tokio::test]
async fn native_nested_parent_message_targets_immediate_parent_and_preserves_authority() {
    native_message_round_trip(true).await;
}
