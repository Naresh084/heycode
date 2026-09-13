//! CMD03 effective status/doctor/permission/sandbox command contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::ui::{SettingsShellSection, SettingsShellTab};
use heycode_agent::{AgentOptions, DenyAll, UiEvent};
use heycode_core::{Context, Plugin, compose};
use heycode_doctor::{DoctorCheck, DoctorCheckId, DoctorOutcome, DoctorRegistry};
use heycode_exec::{
    Sandbox, SandboxBackendCapabilities, SandboxError, SandboxMode, SandboxPolicy, SandboxService,
    SandboxSupport,
};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{FinishReason, LlmSelection, Provider, StreamChunk, llm_plugin};
use heycode_status::{context_status_plugin, status_plugin, web_status_plugin};
use tokio_util::sync::CancellationToken;

struct HealthyCheck(DoctorCheckId);

#[async_trait]
impl DoctorCheck for HealthyCheck {
    fn id(&self) -> &DoctorCheckId {
        &self.0
    }

    async fn run(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<DoctorOutcome, heycode_doctor::DoctorError> {
        DoctorOutcome::pass("test.ready", "The test product world is ready.")
    }
}

struct TestBackend;

impl Sandbox for TestBackend {
    fn name(&self) -> &'static str {
        "test-backend"
    }

    fn capabilities(&self) -> SandboxBackendCapabilities {
        SandboxBackendCapabilities {
            read_only: SandboxSupport::Supported,
            workspace_write: SandboxSupport::Supported,
            network_isolation: SandboxSupport::Unsupported,
        }
    }

    fn confine(
        &self,
        argv: &[String],
        _policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        Ok(argv.to_vec())
    }
}

fn check_plugin() -> Box<dyn Plugin> {
    struct CheckPlugin;
    impl Plugin for CheckPlugin {
        fn name(&self) -> &'static str {
            "test-doctor-check"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::unclassified(self.name())
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_doctor::SERVICE_DOCTOR]
        }

        fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
            let doctor = context
                .get::<DoctorRegistry>(heycode_doctor::SERVICE_DOCTOR)
                .ok_or_else(|| heycode_core::CoreError::other("doctor missing"))?;
            doctor
                .register(
                    context,
                    Arc::new(HealthyCheck(DoctorCheckId::new("test-health").map_err(
                        |error| heycode_core::CoreError::other(error.to_string()),
                    )?)),
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }
    Box::new(CheckPlugin)
}

fn test_sandbox(root: &std::path::Path) -> SandboxService {
    SandboxService::new(
        SandboxMode::Off,
        root.canonicalize().unwrap(),
        Some(Arc::new(TestBackend)),
    )
    .unwrap()
}

#[test]
fn plugin_registers_five_attributed_immediate_commands_and_disposes_them() {
    let root = tempfile::tempdir().unwrap();
    let plugins = vec![
        heycode_doctor::doctor_plugin(),
        heycode_agent::commands_plugin(),
        heycode_exec::sandbox_service_plugin(test_sandbox(root.path())),
        heycode_prompt::prompt_plugin(),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(60)),
        heycode_session::session_query_jsonl_plugin(root.path().join("sessions")),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::DenyAll)),
        status_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();

    for id in ["status", "doctor", "permissions", "sandbox", "config"] {
        let command = commands.get(id).unwrap().unwrap();
        assert_eq!(command.descriptor().source().plugin(), "status");
        assert_eq!(
            command.descriptor().timing(),
            heycode_agent::CommandTiming::Immediate
        );
        let expected = match id {
            "permissions" => "/permissions [mode]".to_owned(),
            "config" => "/config [action]".to_owned(),
            _ => format!("/{id}"),
        };
        assert_eq!(command.descriptor().synopsis(), expected);
    }
    let snapshot = context.plugin_inventory().snapshot().unwrap();
    assert_eq!(
        snapshot
            .contributions
            .iter()
            .filter(
                |row| row.plugin == "status" && row.kind == heycode_core::ContributionKind::Command
            )
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["status", "doctor", "permissions", "sandbox", "config"]
    );

    context.shutdown();
    for id in ["status", "doctor", "permissions", "sandbox", "config"] {
        assert!(commands.get(id).unwrap().is_none());
    }
}

#[test]
fn context_plugin_requires_shell_and_owns_three_immediate_commands() {
    let missing = vec![
        heycode_agent::commands_plugin(),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(60)),
        context_status_plugin(),
    ];
    assert!(compose(&missing).is_err());
    let root = tempfile::tempdir().unwrap();
    let plugins = vec![
        heycode_doctor::doctor_plugin(),
        heycode_agent::commands_plugin(),
        heycode_exec::sandbox_service_plugin(test_sandbox(root.path())),
        heycode_prompt::prompt_plugin(),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(60)),
        heycode_session::session_query_jsonl_plugin(root.path().join("sessions")),
        heycode_agent::approval_plugin(Arc::new(heycode_agent::DenyAll)),
        status_plugin(),
        context_status_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    for id in ["context", "usage", "stats"] {
        let command = commands.get(id).unwrap().unwrap();
        assert_eq!(command.descriptor().source().plugin(), "status-context");
        assert_eq!(
            command.descriptor().timing(),
            heycode_agent::CommandTiming::Immediate
        );
    }
    context.shutdown();
    assert!(commands.get("context").unwrap().is_none());
    assert!(commands.get("usage").unwrap().is_none());
    assert!(commands.get("stats").unwrap().is_none());
}

#[test]
fn web_status_plugin_requires_the_web_service_and_owns_its_command() {
    let missing = vec![heycode_agent::commands_plugin(), web_status_plugin()];
    assert!(compose(&missing).is_err());

    let plugins = vec![
        heycode_agent::commands_plugin(),
        heycode_web::web_registry_plugin(),
        web_status_plugin(),
    ];
    let mut context = compose(&plugins).unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let command = commands.get("web").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "status-web");
    assert_eq!(command.descriptor().synopsis(), "/web");
    context.shutdown();
    assert!(commands.get("web").unwrap().is_none());
}

struct World {
    agent: Arc<heycode_agent::Agent>,
    commands: Arc<heycode_agent::CommandRegistry>,
    events: Arc<Mutex<Vec<UiEvent>>>,
    _context: heycode_core::Context,
    _root: tempfile::TempDir,
}

fn world() -> World {
    world_with_status(status_plugin())
}

fn world_with_status(status: Box<dyn Plugin>) -> World {
    world_with_session(tempfile::tempdir().unwrap(), status, None)
}

fn world_with_session(
    root: tempfile::TempDir,
    status: Box<dyn Plugin>,
    resume: Option<std::path::PathBuf>,
) -> World {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![vec![
        StreamChunk::TextDelta("ok".to_owned()),
        StreamChunk::Usage(heycode_core::TokenUsage {
            prompt_tokens: 12,
            completion_tokens: 3,
        }),
        StreamChunk::Finish(FinishReason::Stop),
    ]]));
    let sandbox = test_sandbox(root.path());
    let execution = heycode_exec::local_execution_plugin_with_sandbox(
        heycode_exec::LocalShellConfig::platform(
            root.path().canonicalize().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
        sandbox,
    );
    let interactive = heycode_agent::InteractiveApproval::new(heycode_core::EventBus::default());
    let ask: Arc<dyn heycode_agent::ApprovalPolicy> = Arc::new(interactive.clone());
    let approval = Arc::new(heycode_agent::SwitchableApproval::new(
        Arc::new(DenyAll),
        Some(ask),
    ));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        heycode_doctor::doctor_plugin(),
        check_plugin(),
        resume.map_or_else(
            || heycode_session::session_plugin(root.path().to_path_buf()),
            heycode_session::session_resume_plugin,
        ),
        heycode_session::session_query_jsonl_plugin(root.path().to_path_buf()),
        heycode_prompt::prompt_plugin(),
        execution,
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_web::portable_web_plugin(heycode_web::PortableWebConfig::official()),
        heycode_tools::tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".to_owned(),
                model: "model-live".to_owned(),
            },
            vec![provider],
        ),
        heycode_agent::switchable_approval_plugin(approval, interactive),
        heycode_agent::commands_plugin(),
        heycode_agent::agent_options_plugin(AgentOptions {
            cwd: Some(root.path().canonicalize().unwrap()),
            ..AgentOptions::default()
        }),
        heycode_agent::compactions_plugin(),
        heycode_agent::agent_plugin(),
        status,
        context_status_plugin(),
        web_status_plugin(),
    ];
    let context = compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    agent
        .ui()
        .on::<UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    World {
        agent,
        commands,
        events,
        _context: context,
        _root: root,
    }
}

async fn execute(world: &World, id: &str) {
    execute_args(world, id, "").await;
}

async fn execute_args(world: &World, id: &str, args: &str) {
    world
        .commands
        .get(id)
        .unwrap()
        .unwrap()
        .execute(&world.agent, args)
        .await
        .unwrap();
}

fn shell_text(events: &[UiEvent], tab: SettingsShellTab) -> Option<&str> {
    events.iter().rev().find_map(|event| match event {
        UiEvent::SettingsShellRequested {
            tab: active,
            snapshot,
        } if *active == tab => snapshot.section(tab).map(SettingsShellSection::plain_text),
        _ => None,
    })
}

#[tokio::test]
async fn config_opens_settings_while_show_renders_the_effective_report() {
    let world = world();
    execute(&world, "config").await;
    assert!(world.events.lock().unwrap().iter().any(|event| matches!(
        event,
        UiEvent::SettingsShellRequested {
            tab: SettingsShellTab::Config,
            ..
        }
    )));
    assert!(
        !world
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, UiEvent::Info { .. })),
        "bare /config opens the owner instead of printing the legacy report"
    );

    execute_args(&world, "config", "show").await;
    let events = world.events.lock().unwrap().clone();
    let info = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::Info { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        info,
        ["config: this surface was composed without a configuration report"],
        "a bare status_plugin() has no config to show"
    );

    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("config.toml");
    std::fs::write(
        &home,
        format!(
            "schema_version = {}\n[llm]\nmodel = \"from-file\"\n",
            heycode_config::CONFIG_SCHEMA_VERSION
        ),
    )
    .unwrap();
    let mut loaded = heycode_config::Config::load_paths(
        heycode_config::ConfigPaths {
            explicit: Some(home.clone()),
            project: None,
            home: None,
        },
        &[],
    )
    .unwrap();
    loaded.config.apply_patch("ui.accent=cyan").unwrap();
    let world = world_with_status(heycode_status::status_plugin_with_config(
        loaded.config.report(),
    ));
    execute_args(&world, "config", "show").await;
    let events = world.events.lock().unwrap().clone();
    let Some(UiEvent::Info { text }) = events.first() else {
        panic!("{events:?}");
    };
    assert!(text.starts_with("config\n"), "{text}");
    assert!(
        text.contains(&format!("llm.model = \"from-file\"  ({})", home.display())),
        "{text}"
    );
    assert!(
        text.contains("ui.accent = \"cyan\"  (command line)"),
        "{text}"
    );
    assert!(text.contains("compaction.auto = true  (default)"), "{text}");

    let error = world
        .commands
        .get("config")
        .unwrap()
        .unwrap()
        .execute(&world.agent, "effective")
        .await
        .expect_err("only the explicit show form is supported");
    assert_eq!(error.to_string(), "usage: /config [show]");
}

#[tokio::test]
async fn shell_commands_select_their_tabs_and_keep_unavailable_stats_distinct() {
    let world = world();
    for id in ["status", "config", "usage", "stats"] {
        execute(&world, id).await;
    }
    let events = world.events.lock().unwrap().clone();
    let requests = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::SettingsShellRequested { tab, snapshot } => Some((*tab, snapshot)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests.iter().map(|(tab, _)| *tab).collect::<Vec<_>>(),
        [
            SettingsShellTab::Status,
            SettingsShellTab::Config,
            SettingsShellTab::Usage,
            SettingsShellTab::Stats,
        ]
    );
    assert!(matches!(
        requests[0].1.status,
        SettingsShellSection::Ready { ref text } if text.contains("doctor: healthy")
    ));
    assert!(matches!(
        requests[1].1.status,
        SettingsShellSection::Ready { ref text }
            if text.contains("doctor: not refreshed (run /status)")
    ));
    assert!(matches!(
        requests[2].1.usage,
        SettingsShellSection::Ready { .. }
    ));
    assert_eq!(
        requests[3].1.stats,
        SettingsShellSection::Empty {
            message: "stats\nstate: empty (no durable session activity yet)".to_owned(),
        }
    );
}

#[tokio::test]
async fn stats_renders_durable_cross_session_activity_and_reported_usage() {
    let world = world();
    world.agent.send("hello").await.unwrap();
    execute(&world, "stats").await;
    let events = world.events.lock().unwrap();
    let stats = shell_text(&events, SettingsShellTab::Stats).expect("stats shell output");
    assert!(
        stats.contains("scope: local durable sessions · active + archived · UTC"),
        "{stats}"
    );
    assert!(
        stats.contains("sessions: used=1 · active=1 · archived=0"),
        "{stats}"
    );
    assert!(
        stats.contains("messages: total=2 · user=1 · assistant=1"),
        "{stats}"
    );
    assert!(
        stats.contains("reported tokens: input=12 · output=3 · total=15"),
        "{stats}"
    );
    assert!(
        stats.contains("usage gaps: unreported responses=0 · unattributed responses=1"),
        "{stats}"
    );
    assert!(
        stats.contains("favorite model: unavailable (no attributable response)"),
        "{stats}"
    );
    assert!(
        stats.contains("coverage: complete for the bounded local store scan"),
        "{stats}"
    );
    let typed = events.iter().rev().find_map(|event| match event {
        UiEvent::SettingsShellRequested {
            tab: SettingsShellTab::Stats,
            snapshot,
        } => snapshot.stats_snapshot(),
        _ => None,
    });
    let typed = typed.expect("ready Stats must retain its typed local snapshot");
    assert_eq!(typed.used_sessions(), 1);
    assert_eq!(typed.total_messages(), 2);
    assert_eq!(typed.reported_input_tokens(), 12);
    assert_eq!(typed.reported_output_tokens(), 3);
}

#[tokio::test]
async fn status_and_doctor_render_only_effective_live_state() {
    let world = world();
    execute(&world, "status").await;
    execute(&world, "doctor").await;
    execute(&world, "web").await;
    let events = world.events.lock().unwrap().clone();
    let status = shell_text(&events, SettingsShellTab::Status).expect("status shell output");
    let info = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::Info { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(status.contains("runtime: native"), "{status}");
    assert!(status.contains("provider: fake"), "{status}");
    assert!(status.contains("model: model-live"), "{status}");
    assert!(status.contains("permission: deny"), "{status}");
    assert!(status.contains("sandbox: full_access"), "{status}");
    assert!(status.contains("active backend: none"), "{status}");
    assert!(
        status.contains("available backend: test-backend"),
        "{status}"
    );
    assert!(status.contains("network: host (not isolated)"), "{status}");
    assert!(status.contains("doctor: healthy"), "{status}");
    assert!(info[0].starts_with("doctor: healthy"), "{}", info[0]);
    assert!(info[0].contains("test-health"), "{}", info[0]);
    let web = info[1];
    assert!(web.starts_with("web\n"), "{web}");
    assert!(web.contains("web search: portable (automatic)"), "{web}");
    assert!(web.contains("web fetch: portable (automatic)"), "{web}");
    assert!(
        web.contains("web search domains: allow=any · block=none"),
        "{web}"
    );
    assert!(
        web.contains("web fetch domains: allow=any · block=none"),
        "{web}"
    );
    assert!(
        web.contains("web provider portable: search+fetch · available"),
        "{web}"
    );
}

#[tokio::test]
async fn permissions_and_sandbox_open_the_picker_with_the_authoritative_report() {
    let world = world();
    execute(&world, "permissions").await;
    execute(&world, "sandbox").await;
    let events = world.events.lock().unwrap().clone();
    let reports = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::PermissionPickerRequested { report }
            | UiEvent::SandboxPanelRequested { report } => Some(report),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reports.len(), 2);
    for report in reports {
        assert_eq!(report.effective_mode, SandboxMode::Off);
        assert_eq!(report.active_backend, None);
        assert_eq!(report.available_backend, Some("test-backend"));
        assert_eq!(report.choices.len(), 3);
    }
}

#[tokio::test]
async fn auto_unavailable_does_not_change_permissions_and_full_access_is_explicit() {
    let world = world();
    assert!(world.commands.get("auto").unwrap().is_none());
    let error = world
        .commands
        .get("permissions")
        .unwrap()
        .unwrap()
        .execute(&world.agent, "auto")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Choose full_access, accepted_edits, default or plan")
    );
    assert_eq!(
        world.agent.approval_kind(),
        heycode_agent::ApprovalPolicyKind::Deny
    );
    let permissions = world.commands.get("permissions").unwrap().unwrap();
    for (name, kind) in [
        ("full_access", heycode_agent::ApprovalPolicyKind::FullAccess),
        (
            "accepted_edits",
            heycode_agent::ApprovalPolicyKind::AcceptedEdits,
        ),
        ("default", heycode_agent::ApprovalPolicyKind::Ask),
    ] {
        permissions.execute(&world.agent, name).await.unwrap();
        assert_eq!(world.agent.approval_kind(), kind);
    }
}

#[tokio::test]
async fn context_and_usage_render_evidence_bounds_cost_unknown_and_strategies() {
    let world = world();
    world.agent.send("hello").await.unwrap();
    execute(&world, "context").await;
    execute(&world, "usage").await;
    let info = world
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            UiEvent::Info { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let context = info
        .iter()
        // `/context` now leads with the occupancy grid; the evidence block that
        // these assertions read still follows it verbatim.
        .find(|text| text.starts_with("Context Usage\n") && text.contains("\ncontext\n"))
        .expect("context output");
    assert!(context.contains("estimated"), "{context}");
    assert!(
        context.contains("window unknown") && context.contains("capacity source: Unknown"),
        "without model evidence or an explicit cap the window remains unknown: {context}"
    );
    assert!(
        context.contains("retry: unavailable (no durable request header)"),
        "with no request on record the replay policy is unknown, not assumed: {context}"
    );
    assert!(context.contains("input cost: unknown"), "{context}");
    assert!(context.contains("detailed cache: unavailable"), "{context}");
    for strategy in ["portable-summary", "provider-native", "prune-oldest"] {
        assert!(context.contains(strategy), "{context}");
    }
    let events = world.events.lock().unwrap().clone();
    let usage = shell_text(&events, SettingsShellTab::Usage).expect("usage shell output");
    assert!(
        usage.contains("reported tokens: input=12 · output=3 · total=15"),
        "{usage}"
    );
    assert!(usage.contains("derived cost: unknown"), "{usage}");
    assert!(usage.contains("unreported steps: 0"), "{usage}");
}

#[tokio::test]
async fn usage_attributes_local_exact_and_aggregate_provider_tool_work() {
    let world = world();
    world.agent.send("hello").await.unwrap();
    {
        let mut session = world.agent.session().lock().unwrap();
        let (turn, step) = (1, 1);
        let request_id = heycode_core::RequestId::from_raw("request_provider");
        session
            .append(heycode_session::SessionEventKind::RequestHeader {
                turn,
                step,
                request_id: request_id.clone(),
                header: Box::new(
                    heycode_session::RequestHeaderSnapshot::new(
                        "fake",
                        "model-live",
                        heycode_core::ProviderProtocol::OpenAiChatCompletions,
                        heycode_session::RequestTargetSnapshot::Http {
                            base_url: "https://example.test/v1".to_owned(),
                        },
                        heycode_session::RequestAuthenticationSnapshot::None,
                        None,
                        Vec::new(),
                        heycode_session::RequestOptionsSnapshot {
                            input_modalities: vec!["text".to_owned()],
                            reasoning_effort: None,
                            defaulted_reasoning_effort: false,
                            structured_output: None,
                            native_features: vec!["web".to_owned()],
                            native_tool_routes: Vec::new(),
                            provider_options: Vec::new(),
                            temperature: None,
                            max_output_tokens: None,
                            defaulted_max_output_tokens: false,
                            purpose: "conversation".to_owned(),
                            retry: None,
                        },
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(heycode_session::SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: heycode_session::RequestContextSnapshot::new(None, None, None, None, 1)
                    .unwrap(),
            })
            .unwrap();
        let local = heycode_core::CallId::from_raw("call_local");
        session
            .append(heycode_session::SessionEventKind::ToolCall {
                turn,
                call_id: local.clone(),
                name: "read".to_owned(),
                args: serde_json::json!({"path":"private"}),
            })
            .unwrap();
        session
            .append(heycode_session::SessionEventKind::ToolResult {
                call_id: local,
                content: "private".to_owned(),
                is_error: false,
                untrusted_content: None,
            })
            .unwrap();
        session
            .append(heycode_session::SessionEventKind::ServerToolUsage {
                turn,
                step,
                request_id,
                usage: Box::new(
                    heycode_core::ServerToolUsage::new(
                        "web_search",
                        2,
                        heycode_core::ServerToolUsageEvidence::ProviderAggregate,
                        heycode_core::ServerToolUsageCost::Unknown,
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
    }

    execute(&world, "usage").await;
    let events = world.events.lock().unwrap();
    let usage = shell_text(&events, SettingsShellTab::Usage).expect("usage shell output");
    assert!(
        usage.contains(
            "tool local/read: requests=1 · success=1 · error=0 · unsettled=0 · cost=unknown"
        ),
        "{usage}"
    );
    assert!(
        usage.contains("tool provider-aggregate/web_search: requests=2 · success=0 · error=0 · unsettled=0 · cost=unknown"),
        "{usage}"
    );
    assert!(!usage.contains("private"), "{usage}");
}

#[tokio::test]
async fn context_and_usage_render_restart_surviving_cache_and_edit_facts() {
    let world = world();
    world.agent.send("hello").await.unwrap();
    {
        let mut session = world.agent.session().lock().unwrap();
        let (turn, step) = session
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                heycode_session::SessionEventKind::AssistantMessage { turn, step, .. } => {
                    Some((*turn, *step))
                }
                _ => None,
            })
            .unwrap();
        let request_id = heycode_core::RequestId::from_raw("status_cache_req");
        session
            .append(heycode_session::SessionEventKind::RequestHeader {
                turn,
                step,
                request_id: request_id.clone(),
                header: Box::new(
                    heycode_session::RequestHeaderSnapshot::new(
                        "fake",
                        "model-live",
                        heycode_core::ProviderProtocol::OpenAiChatCompletions,
                        heycode_session::RequestTargetSnapshot::Http {
                            base_url: "https://status.test/v1".to_owned(),
                        },
                        heycode_session::RequestAuthenticationSnapshot::None,
                        None,
                        Vec::new(),
                        heycode_session::RequestOptionsSnapshot {
                            input_modalities: vec!["text".to_owned()],
                            reasoning_effort: None,
                            defaulted_reasoning_effort: false,
                            structured_output: None,
                            native_features: Vec::new(),
                            native_tool_routes: Vec::new(),
                            provider_options: Vec::new(),
                            temperature: None,
                            max_output_tokens: None,
                            defaulted_max_output_tokens: false,
                            purpose: "conversation".to_owned(),
                            retry: Some(heycode_session::RequestRetrySnapshot {
                                max_attempts: 2,
                                safety: heycode_session::RequestRetrySafetySnapshot::DefinitiveFailuresOnly,
                            }),
                        },
                    )
                    .unwrap(),
                ),
            })
            .unwrap();
        session
            .append(heycode_session::SessionEventKind::RequestContext {
                request_id: request_id.clone(),
                context: {
                    use heycode_session::{
                        RequestContributorMeasurement as Measurement, RequestContributorSnapshot,
                    };
                    let mut context =
                        heycode_session::RequestContextSnapshot::new(None, None, None, None, 1)
                            .unwrap();
                    context.contributors = Some(
                        [
                            "system",
                            "messages",
                            "tool_results",
                            "tools",
                            "provider_state",
                            "attachments",
                        ]
                        .into_iter()
                        .map(|name| RequestContributorSnapshot {
                            contributor: name.into(),
                            measurement: if name == "provider_state" {
                                Measurement::Uncounted {
                                    reason: "unmeasurable".into(),
                                }
                            } else if name == "tool_results" {
                                Measurement::Estimated {
                                    tokens: 1700,
                                    method: "utf8_byte_ratio".into(),
                                }
                            } else {
                                Measurement::Exact { tokens: 0 }
                            },
                            refusals: if name == "tool_results" {
                                vec!["counter:fixture refusal".into()]
                            } else {
                                vec![]
                            },
                        })
                        .collect(),
                    );
                    context
                },
            })
            .unwrap();
        session
            .append(
                heycode_session::SessionEventKind::AssistantResponseMetadata {
                    turn,
                    step,
                    request_id,
                    metadata: Box::new(
                        heycode_core::ProviderResponseMetadata::new(
                            Some(heycode_core::ProviderCacheUsage::new(12, 3, 4, 2).unwrap()),
                            vec![
                                heycode_core::ProviderContextEdit::new(
                                    heycode_core::ContextEditKind::ClearToolUses,
                                    2,
                                    5,
                                )
                                .unwrap(),
                            ],
                            Some(heycode_core::CachePrefixImpact::InvalidatedAtEdit),
                        )
                        .unwrap(),
                    ),
                },
            )
            .unwrap();
    }
    // Reopen the durable session with a fresh agent. Keeping the original
    // agent would exercise its live envelope instead of restart restoration.
    let session_path = world.agent.session().lock().unwrap().path().to_path_buf();
    let World {
        agent,
        commands,
        events,
        _context,
        _root,
    } = world;
    drop((agent, commands, events, _context));
    let world = world_with_session(_root, status_plugin(), Some(session_path));
    assert!(world.agent.token_envelope().is_none());
    assert!(world.agent.context_budget().is_none());
    execute(&world, "context").await;
    execute(&world, "usage").await;
    let events = world.events.lock().unwrap().clone();
    let info = events
        .iter()
        .filter_map(|event| match event {
            UiEvent::Info { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let usage = shell_text(&events, SettingsShellTab::Usage).expect("usage shell output");
    for rendered in info
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(usage))
    {
        assert!(rendered.contains("cache read=4"), "{rendered}");
        assert!(rendered.contains("cache write=2"), "{rendered}");
        assert!(
            rendered.contains("clear_tool_uses=2/5 tokens"),
            "{rendered}"
        );
        assert!(
            rendered.contains("cache prefix=invalidated_at_edit"),
            "{rendered}"
        );
    }
    let context = info
        .iter()
        // `/context` now leads with the occupancy grid; the evidence block that
        // these assertions read still follows it verbatim.
        .find(|text| text.starts_with("Context Usage\n") && text.contains("\ncontext\n"))
        .expect("context output");
    assert!(
        context.contains("retry: 2 attempt(s) · definitive failures only"),
        "the replay policy the request ran under is part of the record: {context}"
    );
    assert!(
        context.contains(
            "tool_results: estimated 1700 (utf8_byte_ratio) · refused=counter:fixture refusal"
        ),
        "{context}"
    );
    assert!(
        context.contains("provider_state: uncounted (unmeasurable)"),
        "{context}"
    );
    assert!(context.contains("total: at least 1700"), "{context}");
    // A subsequent request has no response yet. Its context view must not
    // borrow cache facts from the previous model/request.
    {
        let mut session = world.agent.session().lock().unwrap();
        let mut header = session
            .events()
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                heycode_session::SessionEventKind::RequestHeader { header, .. } => {
                    Some(header.clone())
                }
                _ => None,
            })
            .unwrap();
        header.model = "different-model".into();
        let request_id = heycode_core::RequestId::from_raw("status_next_request");
        session
            .append(heycode_session::SessionEventKind::RequestHeader {
                turn: 1,
                step: 2,
                request_id: request_id.clone(),
                header,
            })
            .unwrap();
        session
            .append(heycode_session::SessionEventKind::RequestContext {
                request_id,
                context: heycode_session::RequestContextSnapshot::new(None, None, None, None, 1)
                    .unwrap(),
            })
            .unwrap();
    }
    world.events.lock().unwrap().clear();
    execute(&world, "context").await;
    let output = world
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            UiEvent::Info { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(output.contains("route: fake/different-model"), "{output}");
    assert!(output.contains("request: status_next_request"), "{output}");
    assert!(output.contains("current agent only"), "{output}");
    assert!(output.contains("detailed cache: unavailable"), "{output}");
    assert!(!output.contains("cache read=4"), "{output}");
    assert!(!output.contains("tool_results: estimated 1700"), "{output}");
    assert!(output.contains("contributors: unavailable"), "{output}");
}

#[tokio::test]
async fn commands_reject_arguments_without_echoing_them_or_publishing_state() {
    let world = world();
    for id in [
        "status",
        "doctor",
        "permissions",
        "sandbox",
        "web",
        "context",
        "usage",
        "stats",
    ] {
        let error = world
            .commands
            .get(id)
            .unwrap()
            .unwrap()
            .execute(&world.agent, "private-unexpected-argument")
            .await
            .unwrap_err()
            .to_string();
        if id == "permissions" {
            assert_eq!(error, "Choose full_access, accepted_edits, default or plan");
        } else {
            assert_eq!(error, format!("usage: /{id}"));
        }
        assert!(!error.contains("private-unexpected-argument"));
    }
    assert!(world.events.lock().unwrap().is_empty());
}
