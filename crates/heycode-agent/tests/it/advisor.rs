//! Persistent advisor composition and exact-route consultation contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use heycode_agent::{
    AdvisorSelection, AdvisorService, Agent, AgentOptions, AutoApprove, BackendControlOwner,
    SubagentBudgetLimits, advisor_plugin, agent_options_plugin, agent_plugin, approval_plugin,
    commands_plugin, subagent_plugin_with_budget,
};
use heycode_core::{NativeToolImplementationKind, Plugin, compose};
use heycode_llm::{
    CatalogFailureKind, CatalogFetchError, ChatRequest, ChunkStream, FinishReason, LlmSelection,
    ModelCatalog, ModelDescriptor, Provider, ProviderDescriptor, ProviderInfo, ProviderProtocol,
    StreamChunk, llm_plugin, model_catalog_plugin,
};
use heycode_session::{SessionEventKind, TurnEndReason, session_plugin};
use heycode_tools::tools_plugin;
use tokio_util::sync::CancellationToken;

const MAIN_PROVIDER: &str = "main-route";
const MAIN_MODEL: &str = "main-model";
const ADVISOR_PROVIDER: &str = "advisor-route";
const ADVISOR_MODEL: &str = "advisor-model";

enum ProviderScript {
    Chunks(Vec<StreamChunk>),
    Fail,
    Hang,
}

struct RecordingProvider {
    name: &'static str,
    model: &'static str,
    scripts: Mutex<VecDeque<ProviderScript>>,
    requests: Mutex<Vec<ChatRequest>>,
    starts: AtomicUsize,
}

impl RecordingProvider {
    fn new(name: &'static str, model: &'static str, scripts: Vec<ProviderScript>) -> Arc<Self> {
        Arc::new(Self {
            name,
            model,
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
            starts: AtomicUsize::new(0),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Provider for RecordingProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name.to_owned(),
            default_model: self.model.to_owned(),
        }
    }

    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.requests.lock().unwrap().push(request);
        self.starts.fetch_add(1, Ordering::SeqCst);
        match self.scripts.lock().unwrap().pop_front() {
            Some(ProviderScript::Chunks(chunks)) => {
                Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
            }
            Some(ProviderScript::Fail) => Box::pin(futures::stream::once(async {
                Err(heycode_llm::LlmError::InvalidResponse(
                    "advisor fixture failure".to_owned(),
                ))
            })),
            Some(ProviderScript::Hang) => Box::pin(futures::stream::pending()),
            None => Box::pin(futures::stream::once(async {
                Err(heycode_llm::LlmError::InvalidResponse(
                    "fixture script exhausted".to_owned(),
                ))
            })),
        }
    }
}

enum CatalogScript {
    Models(Vec<ModelDescriptor>),
    Fail,
    Hang,
}

struct RecordingCatalog {
    descriptor: ProviderDescriptor,
    script: Mutex<Option<CatalogScript>>,
    calls: AtomicUsize,
}

impl RecordingCatalog {
    fn new(provider: &str, display_name: &str, script: CatalogScript) -> Arc<Self> {
        Arc::new(Self {
            descriptor: ProviderDescriptor {
                id: provider.to_owned(),
                display_name: display_name.to_owned(),
                protocols: vec![ProviderProtocol::Unknown],
            },
            script: Mutex::new(Some(script)),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait::async_trait]
impl ModelCatalog for RecordingCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.descriptor.clone()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let script = self.script.lock().unwrap().take().unwrap();
        match script {
            CatalogScript::Models(models) => Ok(models),
            CatalogScript::Fail => Err(CatalogFetchError::new(
                CatalogFailureKind::Network,
                "fixture catalog unavailable",
            )),
            CatalogScript::Hang => {
                cancellation.cancelled().await;
                Err(CatalogFetchError::cancelled())
            }
        }
    }
}

#[derive(Default)]
struct RecordingSettingsWriter {
    writes: Mutex<Vec<(String, serde_json::Value)>>,
}

impl RecordingSettingsWriter {
    fn last_advisor_value(&self) -> serde_json::Value {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(namespace, _)| namespace == "advisor")
            .map(|(_, value)| value.clone())
            .expect("advisor selection must be persisted")
    }
}

impl heycode_settings::SettingsWriter for RecordingSettingsWriter {
    fn persist_user(
        &self,
        namespace: &heycode_settings::SettingsNamespace,
        section: &serde_json::Value,
    ) -> Result<(), String> {
        self.writes
            .lock()
            .unwrap()
            .push((namespace.as_str().to_owned(), section.clone()));
        Ok(())
    }
}

fn writable_settings_plugin(
    documents: heycode_settings::SettingsDocuments,
    writer: Arc<RecordingSettingsWriter>,
) -> Box<dyn Plugin> {
    struct WritableSettings {
        documents: heycode_settings::SettingsDocuments,
        writer: Arc<RecordingSettingsWriter>,
    }
    impl Plugin for WritableSettings {
        fn name(&self) -> &'static str {
            "advisor-test-settings"
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            context.provide(
                heycode_settings::SERVICE_SETTINGS,
                self.name(),
                heycode_settings::SettingsService::with_writer(
                    self.documents.clone(),
                    self.writer.clone() as Arc<dyn heycode_settings::SettingsWriter>,
                ),
            )
        }
    }
    Box::new(WritableSettings { documents, writer })
}

struct World {
    context: heycode_core::Context,
    root: tempfile::TempDir,
    agent: Arc<Agent>,
    advisor: Arc<AdvisorService>,
    main: Arc<RecordingProvider>,
    consultant: Arc<RecordingProvider>,
    writer: Arc<RecordingSettingsWriter>,
}

fn world(
    main_scripts: Vec<ProviderScript>,
    advisor_scripts: Vec<ProviderScript>,
    budget: SubagentBudgetLimits,
    documents: heycode_settings::SettingsDocuments,
) -> World {
    let root = tempfile::tempdir().unwrap();
    let main = RecordingProvider::new(MAIN_PROVIDER, MAIN_MODEL, main_scripts);
    let consultant = RecordingProvider::new(ADVISOR_PROVIDER, ADVISOR_MODEL, advisor_scripts);
    let writer = Arc::new(RecordingSettingsWriter::default());
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(root.path().to_path_buf()),
        heycode_prompt::prompt_plugin(),
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(
                root.path().to_path_buf(),
                Duration::from_secs(30),
            )
            .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: MAIN_PROVIDER.to_owned(),
                model: MAIN_MODEL.to_owned(),
            },
            vec![
                main.clone() as Arc<dyn Provider>,
                consultant.clone() as Arc<dyn Provider>,
            ],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        writable_settings_plugin(documents, writer.clone()),
        heycode_agent::compactions_plugin(),
        subagent_plugin_with_budget(root.path().to_path_buf(), 3, budget),
        agent_options_plugin(AgentOptions {
            cwd: Some(root.path().to_path_buf()),
            auto_title: false,
            ..AgentOptions::default()
        }),
        agent_plugin(),
        advisor_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let agent = context.get::<Agent>(heycode_agent::SERVICE_AGENT).unwrap();
    let advisor = context
        .get::<AdvisorService>(heycode_agent::SERVICE_ADVISOR)
        .unwrap();
    World {
        context,
        root,
        agent,
        advisor,
        main,
        consultant,
        writer,
    }
}

fn advisor_selection() -> AdvisorSelection {
    AdvisorSelection::new(
        BackendControlOwner::NativeInference {
            provider: ADVISOR_PROVIDER.to_owned(),
        },
        ADVISOR_MODEL,
        None,
    )
    .unwrap()
}

fn main_selection() -> AdvisorSelection {
    AdvisorSelection::new(
        BackendControlOwner::NativeInference {
            provider: MAIN_PROVIDER.to_owned(),
        },
        MAIN_MODEL,
        None,
    )
    .unwrap()
}

fn call_advisor(id: &str) -> ProviderScript {
    ProviderScript::Chunks(vec![
        StreamChunk::ToolCallDelta {
            index: 0,
            id: Some(id.to_owned()),
            name: Some("advisor".to_owned()),
            arguments_delta: "{}".to_owned(),
        },
        StreamChunk::Finish(FinishReason::ToolCalls),
    ])
}

fn reply(text: &str) -> ProviderScript {
    ProviderScript::Chunks(vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(FinishReason::Stop),
    ])
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture condition must settle");
}

#[tokio::test]
async fn exact_route_consultation_has_no_child_tools_and_resumes_the_same_parent_turn() {
    let world = world(
        vec![call_advisor("advisor-1"), reply("parent final answer")],
        vec![reply("consulted guidance")],
        SubagentBudgetLimits::default(),
        heycode_settings::SettingsDocuments::new(),
    );
    world
        .advisor
        .select(&world.agent, advisor_selection())
        .unwrap();

    let report = world.agent.send("choose the safe boundary").await.unwrap();

    assert_eq!(report.text, "parent final answer");
    let main = world.main.requests();
    let consultant = world.consultant.requests();
    assert_eq!(main.len(), 2);
    assert_eq!(consultant.len(), 1);
    assert_eq!(consultant[0].model, ADVISOR_MODEL);
    assert!(
        consultant[0].tools.as_ref().is_none_or(Vec::is_empty),
        "advisor child must expose no client or hosted tools"
    );
    assert!(
        consultant[0].messages.iter().any(|message| {
            message.content.contains("current_parent_turn_json")
                && message.content.contains("choose the safe boundary")
        }),
        "the open parent turn must reach the forked advisor through bounded context"
    );
    assert!(
        main[0]
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool.name == "advisor"))
    );
    let result = main[1]
        .messages
        .iter()
        .find(|message| message.role == heycode_llm::Role::Tool)
        .expect("advisor result must precede the resumed parent step");
    assert!(result.content.contains("consulted guidance"));
    assert!(result.content.contains(ADVISOR_MODEL));

    let session = world.agent.session().lock().unwrap();
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnStart { .. }))
            .count(),
        1,
        "consultation is a tool step, not a second human-facing parent turn"
    );
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| matches!(
                event.kind,
                SessionEventKind::TurnEnd {
                    reason: TurnEndReason::Stop,
                    ..
                }
            ))
            .count(),
        1
    );
    drop(session);
    assert_eq!(
        world
            .advisor
            .status()
            .unwrap()
            .descendant_budget
            .requests_reserved,
        1
    );
}

#[tokio::test]
async fn disabled_portable_advisor_preserves_provider_owned_route_and_enabled_conflict_fails_closed()
 {
    let world = world(
        vec![reply("provider advisor remains available")],
        Vec::new(),
        SubagentBudgetLimits::default(),
        heycode_settings::SettingsDocuments::new(),
    );
    let native_tools = world
        .context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    native_tools
        .register(
            &world.context,
            heycode_native_tools::NativeToolImplementation::new(
                "advisor",
                "main-route:advisor",
                NativeToolImplementationKind::Provider,
                Some(MAIN_PROVIDER.to_owned()),
                100,
            )
            .unwrap(),
        )
        .unwrap();
    let routes = native_tools
        .resolve_for_model(MAIN_PROVIDER, MAIN_MODEL)
        .unwrap();
    assert!(routes.iter().any(|route| {
        route.logical() == "advisor" && route.kind() == NativeToolImplementationKind::Provider
    }));

    world.agent.send("use provider capability").await.unwrap();

    assert_eq!(world.main.requests().len(), 1);
    assert!(
        world.main.requests()[0]
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|tool| tool.name == "advisor")),
        "the logical schema must survive until the provider-aware drafting boundary"
    );
    assert!(world.advisor.status().unwrap().selection.is_none());
    assert!(world.consultant.requests().is_empty());

    world
        .advisor
        .select(&world.agent, advisor_selection())
        .unwrap();
    let error = world.agent.send("ambiguous advisor").await.unwrap_err();
    assert!(error.to_string().contains("provider-owned advisor"));
    assert_eq!(world.main.requests().len(), 1);
    assert!(world.consultant.requests().is_empty());
}

#[tokio::test]
async fn failed_dispatch_spends_existing_budget_and_the_next_consultation_is_refused_locally() {
    let world = world(
        vec![
            call_advisor("advisor-fail"),
            reply("continued after advisor failure"),
            call_advisor("advisor-budget"),
            reply("continued after budget refusal"),
        ],
        vec![ProviderScript::Fail],
        SubagentBudgetLimits {
            max_in_flight: 1,
            max_requests: 1,
            max_output_tokens: 77,
        },
        heycode_settings::SettingsDocuments::new(),
    );
    world
        .advisor
        .select(&world.agent, advisor_selection())
        .unwrap();

    let first = world.agent.send("first consultation").await.unwrap();
    assert_eq!(first.text, "continued after advisor failure");
    assert_eq!(world.consultant.requests().len(), 1);
    assert_eq!(world.consultant.requests()[0].max_tokens, Some(77));
    let after_failure = world.advisor.status().unwrap().descendant_budget;
    assert_eq!(after_failure.requests_reserved, 1);
    assert_eq!(after_failure.limits.max_requests, 1);

    let second = world.agent.send("second consultation").await.unwrap();
    assert_eq!(second.text, "continued after budget refusal");
    assert_eq!(
        world.consultant.requests().len(),
        1,
        "exhaustion must refuse before a second provider dispatch"
    );
    let resumed = world.main.requests();
    assert!(
        resumed[1].messages.iter().any(|message| {
            message.role == heycode_llm::Role::Tool
                && message.content.contains("advisor fixture failure")
        }),
        "the first provider failure must be returned explicitly to the parent"
    );
    assert!(
        resumed[3]
            .messages
            .iter()
            .any(|message| message.role == heycode_llm::Role::Tool
                && message.content.contains("budget exhausted")),
        "the parent must receive an explicit tool failure and remain in control"
    );
}

#[tokio::test]
async fn cancelling_a_held_consultation_settles_the_parent_and_does_not_refund_dispatch() {
    let world = world(
        vec![call_advisor("advisor-held")],
        vec![ProviderScript::Hang],
        SubagentBudgetLimits {
            max_in_flight: 1,
            max_requests: 2,
            max_output_tokens: 128,
        },
        heycode_settings::SettingsDocuments::new(),
    );
    world
        .advisor
        .select(&world.agent, advisor_selection())
        .unwrap();
    let agent = world.agent.clone();
    let turn = tokio::spawn(async move { agent.send("held consultation").await });
    until(|| world.consultant.starts.load(Ordering::SeqCst) == 1).await;

    assert!(matches!(
        world.advisor.disable(&world.agent),
        Err(heycode_agent::AdvisorError::TurnActive)
    ));
    world.agent.token().cancel();
    let report = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("advisor cancellation must join the child operation")
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, "aborted");
    assert!(!world.agent.token().is_turn_active());
    let budget = world.advisor.status().unwrap().descendant_budget;
    assert_eq!(budget.requests_reserved, 1);
    assert_eq!(budget.in_flight, 0);
}

#[test]
fn persisted_exact_route_is_restored_without_a_provider_validation_request() {
    let first = world(
        Vec::new(),
        Vec::new(),
        SubagentBudgetLimits::default(),
        heycode_settings::SettingsDocuments::new(),
    );
    first
        .advisor
        .select(&first.agent, advisor_selection())
        .unwrap();
    let persisted = first.writer.last_advisor_value();
    assert!(first.main.requests().is_empty());
    assert!(first.consultant.requests().is_empty());
    drop(first);

    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            heycode_settings::SettingsNamespace::new("advisor").unwrap(),
            persisted,
        )
        .unwrap();
    let restarted = world(
        Vec::new(),
        Vec::new(),
        SubagentBudgetLimits::default(),
        documents,
    );
    assert_eq!(
        restarted.advisor.status().unwrap().selection,
        Some(advisor_selection())
    );
    assert!(restarted.main.requests().is_empty());
    assert!(restarted.consultant.requests().is_empty());
    assert!(restarted.root.path().exists());
}

#[test]
fn higher_precedence_route_is_rejected_before_persistence_or_live_mutation() {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_project(
            heycode_settings::SettingsNamespace::new("advisor").unwrap(),
            serde_json::json!({
                "mode":"enabled",
                "owner":format!("native:{MAIN_PROVIDER}"),
                "model":MAIN_MODEL,
                "effort":""
            }),
        )
        .unwrap();
    let world = world(
        Vec::new(),
        Vec::new(),
        SubagentBudgetLimits::default(),
        documents,
    );
    let before = world.advisor.status().unwrap();
    assert_eq!(before.selection, Some(main_selection()));

    let error = world
        .advisor
        .select(&world.agent, advisor_selection())
        .unwrap_err();

    assert!(error.to_string().contains("higher-precedence"));
    let after = world.advisor.status().unwrap();
    assert_eq!(after.selection, before.selection);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.settings_revision, before.settings_revision);
    assert!(world.writer.writes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn picker_refreshes_every_connected_catalog_without_inference_and_keeps_partial_results() {
    let world = world(
        Vec::new(),
        Vec::new(),
        SubagentBudgetLimits::default(),
        heycode_settings::SettingsDocuments::new(),
    );
    world
        .advisor
        .select(&world.agent, main_selection())
        .unwrap();
    let main_catalog = RecordingCatalog::new(
        MAIN_PROVIDER,
        "Main Route",
        CatalogScript::Models(vec![
            ModelDescriptor::unknown("main-small"),
            ModelDescriptor::unknown("main-large"),
        ]),
    );
    let advisor_catalog =
        RecordingCatalog::new(ADVISOR_PROVIDER, "Advisor Route", CatalogScript::Fail);
    let catalogs = world
        .context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    catalogs
        .register(&world.context, main_catalog.clone())
        .unwrap();
    catalogs
        .register(&world.context, advisor_catalog.clone())
        .unwrap();

    let loaded = world
        .advisor
        .refresh_route_choices(CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(main_catalog.calls.load(Ordering::SeqCst), 1);
    assert_eq!(advisor_catalog.calls.load(Ordering::SeqCst), 1);
    assert!(loaded.choices.iter().any(|choice| {
        choice.selection.owner_key() == format!("native:{MAIN_PROVIDER}")
            && choice.selection.model() == "main-small"
    }));
    assert!(loaded.choices.iter().any(|choice| {
        choice.selection.owner_key() == format!("native:{MAIN_PROVIDER}")
            && choice.selection.model() == "main-large"
    }));
    assert!(
        loaded
            .choices
            .iter()
            .any(|choice| choice.selection == main_selection()),
        "the committed route remains visible when its catalog refresh fails"
    );
    assert!(
        loaded
            .choices
            .iter()
            .any(|choice| choice.selection == advisor_selection()),
        "a failed provider contributes its configured default only"
    );
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains("could not load"));
    assert!(loaded.warnings[0].contains("configured default"));
    let before = world.advisor.status().unwrap();
    let writes = world.writer.writes.lock().unwrap().len();
    let unchanged = world
        .advisor
        .select(&world.agent, main_selection())
        .unwrap();
    assert_eq!(unchanged.generation, before.generation);
    assert_eq!(world.writer.writes.lock().unwrap().len(), writes);
    assert!(world.main.requests().is_empty());
    assert!(world.consultant.requests().is_empty());
}

#[tokio::test]
async fn picker_catalog_loading_honors_caller_cancellation_without_inference() {
    let world = world(
        Vec::new(),
        Vec::new(),
        SubagentBudgetLimits::default(),
        heycode_settings::SettingsDocuments::new(),
    );
    let hanging = RecordingCatalog::new(MAIN_PROVIDER, "Main Route", CatalogScript::Hang);
    let catalogs = world
        .context
        .get::<heycode_llm::CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    catalogs.register(&world.context, hanging.clone()).unwrap();
    let cancellation = CancellationToken::new();
    let service = world.advisor.clone();
    let token = cancellation.clone();
    let loading = tokio::spawn(async move { service.refresh_route_choices(token).await });
    until(|| hanging.calls.load(Ordering::SeqCst) == 1).await;

    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), loading)
        .await
        .expect("catalog loading must settle after cancellation")
        .unwrap()
        .unwrap_err();

    assert!(matches!(error, heycode_agent::AdvisorError::Cancelled));
    assert!(world.main.requests().is_empty());
    assert!(world.consultant.requests().is_empty());
}
