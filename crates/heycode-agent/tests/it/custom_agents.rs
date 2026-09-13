//! Custom declarations must change actual child requests and execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use heycode_agent::{SubagentConfig, SubagentContinuation, SubagentPreset, SubagentSeed};
use heycode_llm::{ChatRequest, ChunkStream, Provider, ProviderInfo, StreamChunk};
use heycode_tools::{Tool, ToolCtx, ToolError};
use std::sync::{Arc, Mutex};

struct Recorder {
    inner: heycode_llm::testing::FakeProvider,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
impl Provider for Recorder {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}
struct CountTool {
    name: &'static str,
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}
#[async_trait::async_trait]
impl Tool for CountTool {
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec {
            name: self.name.to_owned(),
            description: "test tool".to_owned(),
            parameters: serde_json::json!({"type":"object","properties":{"name":{"type":"string"}}}),
        }
    }
    async fn run(
        &self,
        args: serde_json::Value,
        _: &ToolCtx,
    ) -> Result<serde_json::Value, ToolError> {
        self.calls.lock().unwrap().push(args);
        Ok(serde_json::json!("loaded"))
    }
}
struct World {
    context: heycode_core::Context,
    root: tempfile::TempDir,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
}
impl World {
    fn new(scripts: Vec<Vec<StreamChunk>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(Recorder {
            inner: heycode_llm::testing::FakeProvider::new(scripts),
            requests: requests.clone(),
        });
        let context = heycode_core::compose(&[
            heycode_session::session_plugin(root.path().to_path_buf()),
            heycode_prompt::prompt_plugin(),
            heycode_exec::local_execution_plugin(
                heycode_exec::LocalShellConfig::platform(
                    std::env::current_dir().unwrap(),
                    std::time::Duration::from_secs(10),
                )
                .unwrap(),
            ),
            heycode_exec::local_filesystem_plugin(),
            heycode_native_tools::native_tools_plugin(),
            heycode_web::web_registry_plugin(),
            heycode_tools::tools_plugin(Default::default()),
            heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
            heycode_llm::token_counters_plugin(),
            heycode_llm::llm_plugin(
                heycode_llm::LlmSelection {
                    provider_name: "fake".to_owned(),
                    model: "parent-model".to_owned(),
                },
                vec![provider],
            ),
            heycode_agent::approval_plugin(Arc::new(heycode_agent::AutoApprove)),
            heycode_agent::commands_plugin(),
            heycode_agent::compactions_plugin(),
            heycode_agent::subagent_plugin(root.path().to_path_buf(), 3),
            heycode_agent::agent_options_plugin(Default::default()),
            heycode_agent::agent_plugin(),
            heycode_agent::subagent_jobs_plugin(),
        ])
        .unwrap();
        Self {
            context,
            root,
            requests,
        }
    }
    fn registry(&self) -> Arc<heycode_agent::SubagentRegistry> {
        self.context.get(heycode_agent::SERVICE_SUBAGENTS).unwrap()
    }
    fn tools(&self) -> Arc<heycode_tools::ToolRegistry> {
        self.context.get(heycode_tools::SERVICE_TOOLS).unwrap()
    }
    async fn task(
        &self,
        agent: &str,
        mut extra: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        extra
            .as_object_mut()
            .unwrap()
            .entry("background")
            .or_insert(serde_json::json!(false));
        self.task_with_defaults(agent, extra).await
    }
    async fn task_with_defaults(
        &self,
        agent: &str,
        extra: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let mut args =
            serde_json::json!({"agent":agent,"label":"custom","prompt":"exact user task"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        self.tools()
            .get("task")
            .unwrap()
            .run(args, &ToolCtx::default())
            .await
    }
    fn preset(&self, config: SubagentConfig) -> heycode_agent::SubagentPresetRegistration {
        self.registry()
            .register_preset_owned(
                SubagentPreset::new(
                    "custom",
                    "Custom",
                    "Standing child role instructions.",
                    None,
                    SubagentSeed::Fresh,
                    SubagentContinuation::OneShot,
                )
                .unwrap()
                .with_config(config)
                .unwrap(),
            )
            .unwrap()
    }
}
impl Drop for World {
    fn drop(&mut self) {
        self.context.shutdown();
    }
}
fn stop(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}
fn call(name: &str, args: serde_json::Value) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some(format!("call-{name}")),
            name: Some(name.to_owned()),
            arguments_delta: args.to_string(),
        },
        StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
    ]
}

#[tokio::test]
async fn custom_policy_changes_schemas_model_system_and_actual_dispatch() {
    let marker_directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let marker = marker_directory.path().join("must-not-exist");
    let world = World::new(vec![
        call(
            "write",
            serde_json::json!({"path":marker,"content":"forbidden"}),
        ),
        call("load_skill", serde_json::json!({"name":"forbidden"})),
        call("load_skill", serde_json::json!({"name":"allowed"})),
        call("mcp__foreign__lookup", serde_json::json!({})),
        call("mcp__allowed__lookup", serde_json::json!({})),
        stop("complete"),
    ]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    for name in ["load_skill", "mcp__allowed__lookup", "mcp__foreign__lookup"] {
        world
            .tools()
            .register_shared(Arc::new(CountTool {
                name,
                calls: calls.clone(),
            }))
            .unwrap();
    }
    world
        .context
        .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
        .unwrap()
        .section_shared("skills-catalog", 90, |_| {
            "forbidden-skill-catalog-description".to_owned()
        })
        .unwrap();
    let _preset = world.preset(SubagentConfig {
        model: Some("custom-model".to_owned()),
        tools: Some(vec![
            "read".into(),
            "write".into(),
            "load_skill".into(),
            "mcp__allowed__lookup".into(),
            "mcp__foreign__lookup".into(),
        ]),
        denied_tools: vec!["write".into()],
        skills: Some(vec!["allowed".into()]),
        mcp_servers: Some(vec!["allowed".into()]),
        ..Default::default()
    });
    assert_eq!(
        world.task("custom", serde_json::json!({})).await.unwrap(),
        "complete"
    );
    assert!(!marker.exists());
    assert_eq!(
        *calls.lock().unwrap(),
        vec![serde_json::json!({"name":"allowed"}), serde_json::json!({})]
    );
    let requests = world.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(
        requests
            .iter()
            .all(|request| request.model == "custom-model")
    );
    let initial = &requests[0];
    let names: Vec<_> = initial
        .tools
        .as_ref()
        .unwrap()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, ["load_skill", "mcp__allowed__lookup", "read"]);
    assert!(initial.messages.iter().any(|message| {
        message.role == heycode_llm::Role::System
            && message
                .content
                .contains("Standing child role instructions.")
    }));
    assert!(!initial.messages.iter().any(|message| {
        message
            .content
            .contains("forbidden-skill-catalog-description")
    }));
    assert_eq!(
        initial
            .messages
            .iter()
            .filter(|message| message.role == heycode_llm::Role::User)
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["exact user task"]
    );
    drop(requests);
    for session in std::fs::read_dir(world.root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|row| heycode_session::Session::open(row.path()).ok())
    {
        for event in session.events() {
            if let heycode_session::SessionEventKind::UserMessage { text } = &event.kind {
                assert_eq!(text, "exact user task");
            }
        }
    }
    assert_eq!(
        world
            .context
            .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
            .unwrap()
            .selection()
            .model,
        "parent-model"
    );
}

#[tokio::test]
async fn max_turns_survives_retained_followups_and_settles_durably() {
    let world = World::new(vec![stop("first"), stop("must never request")]);
    let _preset = world.preset(SubagentConfig {
        max_turns: Some(1),
        ..Default::default()
    });
    let result = world
        .task("custom", serde_json::json!({"mode":"continuable"}))
        .await
        .unwrap();
    let id = result
        .as_str()
        .unwrap()
        .split("[task_id: ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    let error = world
        .tools()
        .get("send_message")
        .unwrap()
        .run(
            serde_json::json!({"task_id":id,"message":"again"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("max_turns"));
    assert_eq!(world.requests.lock().unwrap().len(), 1);
    let child_session = world
        .registry()
        .task_snapshots()
        .into_iter()
        .find(|task| task.id == id)
        .unwrap()
        .session_id
        .unwrap();
    let session = heycode_session::Session::open(world.root.path().join(child_session)).unwrap();
    assert!(session.events().iter().any(|event| matches!(
        event.kind,
        heycode_session::SessionEventKind::TurnEnd {
            reason: heycode_session::TurnEndReason::MaxSteps,
            ..
        }
    )));
}

#[tokio::test]
async fn unsupported_effort_refuses_before_inference() {
    let world = World::new(vec![stop("must not execute")]);
    let _preset = world.preset(SubagentConfig {
        effort: Some("high".into()),
        ..Default::default()
    });
    assert!(world.task("custom", serde_json::json!({})).await.is_err());
    assert!(world.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn builtin_native_review_presets_are_discoverable_callable_and_overridable() {
    let world = World::new(vec![stop("reviewed"), stop("advised"), stop("checked")]);
    for (name, result) in [
        ("reviewer", "reviewed"),
        ("advisor", "advised"),
        ("security-review", "checked"),
    ] {
        assert!(world.registry().preset(name).is_some());
        assert_eq!(
            world.task(name, serde_json::json!({})).await.unwrap(),
            result
        );
    }
    let requests = world.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter() {
        assert_eq!(request.model, "parent-model");
        assert!(
            request
                .tools
                .as_ref()
                .unwrap()
                .iter()
                .all(|tool| !matches!(tool.name.as_str(), "write" | "edit" | "bash" | "task"))
        );
    }
    drop(requests);
    let override_row = world
        .registry()
        .register_preset_owned(
            SubagentPreset::new(
                "reviewer",
                "My reviewer",
                "Override",
                None,
                SubagentSeed::Fresh,
                SubagentContinuation::OneShot,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        world.registry().preset("reviewer").unwrap().display(),
        "My reviewer"
    );
    drop(override_row);
    assert_eq!(
        world.registry().preset("reviewer").unwrap().display(),
        "Code reviewer"
    );
}

#[tokio::test]
async fn persistent_memory_is_written_by_child_and_loaded_by_next_child() {
    use sha2::{Digest, Sha256};
    let world = World::new(vec![
        call(
            "agent_memory_write",
            serde_json::json!({"text":"durable agent learning","revision":format!("{:x}",Sha256::digest([]))}),
        ),
        stop("saved"),
        stop("recalled"),
    ]);
    let _preset = world.preset(SubagentConfig {
        memory: heycode_agent::ChildMemory::User,
        ..Default::default()
    });
    assert_eq!(
        world.task("custom", serde_json::json!({})).await.unwrap(),
        "saved"
    );
    let _alias = world
        .registry()
        .register_preset_owned(
            world
                .registry()
                .preset("custom")
                .unwrap()
                .with_id("alias")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        world.task("alias", serde_json::json!({})).await.unwrap(),
        "recalled"
    );
    assert!(
        world.requests.lock().unwrap()[2]
            .messages
            .iter()
            .any(|message| message.role == heycode_llm::Role::System
                && message.content.contains("durable agent learning"))
    );
    assert_eq!(
        std::fs::read_to_string(
            world
                .root
                .path()
                .join(".agent-memory/user/custom/MEMORY.md")
        )
        .unwrap(),
        "durable agent learning"
    );
}

#[tokio::test]
async fn background_default_is_used_and_can_be_overridden() {
    let world = World::new(vec![
        stop("background done"),
        stop(""),
        stop("foreground done"),
    ]);
    let _preset = world.preset(SubagentConfig::default());
    let result = world
        .task_with_defaults("custom", serde_json::json!({}))
        .await
        .unwrap();
    let id = result["job_id"]
        .as_str()
        .expect("structured background receipt");
    assert_eq!(result["delivery"], "automatic");
    let jobs = world
        .context
        .get::<Arc<heycode_agent::JobRegistry>>(heycode_agent::SERVICE_JOBS)
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        jobs.wait_for_settlement(&heycode_agent::JobId::parse(id).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome, heycode_agent::JobOutcome::Completed);
    world
        .context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap()
        .wait_for_background(tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        world
            .task("custom", serde_json::json!({"background":false}))
            .await
            .unwrap(),
        "foreground done"
    );
}

#[tokio::test]
async fn explicit_default_permissions_cannot_inherit_headless_auto_approval() {
    let marker = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = marker.path().join("blocked");
    let world = World::new(vec![
        call(
            "write",
            serde_json::json!({"path":path,"content":"must not write"}),
        ),
        stop("permission required"),
    ]);
    let _preset = world.preset(SubagentConfig {
        permissions: heycode_agent::ChildPermissions::Default,
        ..Default::default()
    });
    assert_eq!(
        world.task("custom", serde_json::json!({})).await.unwrap(),
        "permission required"
    );
    assert!(!path.exists());
    let requests = world.requests.lock().unwrap();
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.role == heycode_llm::Role::Tool
                && message.content.contains("approval required"))
    );
}

#[tokio::test]
async fn reload_does_not_widen_a_retained_child_but_changes_future_children() {
    let marker = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let path = marker.path().join("new-child-only");
    let world = World::new(vec![
        stop("ready"),
        call(
            "write",
            serde_json::json!({"path":path,"content":"old child"}),
        ),
        stop("old remained restricted"),
        call(
            "write",
            serde_json::json!({"path":path,"content":"new child"}),
        ),
        stop("new child wrote"),
    ]);
    let mut owned = vec![world.preset(SubagentConfig {
        permissions: heycode_agent::ChildPermissions::ReadOnly,
        ..Default::default()
    })];
    let result = world
        .task("custom", serde_json::json!({"mode":"continuable"}))
        .await
        .unwrap();
    let id = result
        .as_str()
        .unwrap()
        .split("[task_id: ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    let next = SubagentPreset::new(
        "custom",
        "New custom",
        "New role instructions",
        None,
        SubagentSeed::Fresh,
        SubagentContinuation::OneShot,
    )
    .unwrap();
    world
        .registry()
        .replace_presets_owned(&mut owned, vec![next])
        .unwrap();
    world
        .tools()
        .get("send_message")
        .unwrap()
        .run(
            serde_json::json!({"task_id":id,"message":"try a write"}),
            &ToolCtx::default(),
        )
        .await
        .unwrap();
    assert!(!path.exists());
    assert_eq!(
        world.task("custom", serde_json::json!({})).await.unwrap(),
        "new child wrote"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "new child");
}
