//! Skills end-to-end: discovery precedence, catalog, loader enforcement,
//! /skill turn injection (works for user-only skills).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_agent::{
    AgentOptions, AutoApprove, agent_options_plugin, agent_plugin, approval_plugin,
    commands_plugin, parse_slash,
};
use heycode_core::{Plugin, compose};
use heycode_llm::testing::FakeProvider;
use heycode_llm::{
    ChatRequest, ChunkStream, LlmSelection, Provider, ProviderInfo, StreamChunk, llm_plugin,
};
use heycode_prompt::prompt_plugin;
use heycode_session::session_plugin;
use heycode_skills::{SkillRoot, skills_plugin};
use heycode_tools::tools_plugin;

#[derive(Default)]
struct RecordingSettingsWriter {
    writes: std::sync::Mutex<Vec<serde_json::Value>>,
    fail: std::sync::atomic::AtomicBool,
}

impl heycode_settings::SettingsWriter for RecordingSettingsWriter {
    fn persist_user(
        &self,
        _namespace: &heycode_settings::SettingsNamespace,
        section: &serde_json::Value,
    ) -> Result<(), String> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("fixture persistence failure".to_owned());
        }
        self.writes.lock().unwrap().push(section.clone());
        Ok(())
    }
}

struct WritableSettingsPlugin(heycode_settings::SettingsService);

impl Plugin for WritableSettingsPlugin {
    fn name(&self) -> &'static str {
        "fixture-settings"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            "fixture-settings",
            "1.0.0",
            &[heycode_core::PluginContributionKind::Service],
        )
    }

    fn provides(&self) -> &'static [heycode_core::ServiceKey] {
        &[heycode_settings::SERVICE_SETTINGS]
    }

    fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
        context.provide(
            heycode_settings::SERVICE_SETTINGS,
            "fixture-settings",
            self.0.clone(),
        )
    }
}

fn execution_plugin() -> Box<dyn heycode_core::Plugin> {
    heycode_exec::local_execution_plugin(
        heycode_exec::LocalShellConfig::platform(
            std::env::current_dir().unwrap(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    )
}

struct Recording {
    inner: FakeProvider,
    sink: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
}

impl Provider for Recording {
    fn info(&self) -> ProviderInfo {
        self.inner.info()
    }
    fn stream(&self, request: ChatRequest) -> ChunkStream {
        self.sink.lock().unwrap().push(request.clone());
        self.inner.stream(request)
    }
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

fn write_skill(root: &std::path::Path, dir: &str, front: &str, body: &str) {
    let d = root.join(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("SKILL.md"), format!("---\n{front}\n---\n\n{body}\n")).unwrap();
}

fn skill_root(authority: &std::path::Path, relative: &str) -> SkillRoot {
    SkillRoot::bind(authority, relative).unwrap()
}

#[test]
fn discovery_first_root_wins_and_restricted_flag_parses() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "review",
        "name: review\ndescription: Review code changes",
        "# Review\nDo the review.",
    );
    write_skill(
        &home.path().join("skills"),
        "review",
        "name: review\ndescription: HOME OVERRIDE",
        "should not win",
    );
    write_skill(
        &home.path().join("skills"),
        "deploy",
        "name: deploy\ndescription: Ship it\ndisable-model-invocation: true",
        "# Deploy\nOnly humans trigger this.",
    );

    let roots = heycode_skills::default_roots(project.path(), home.path()).unwrap();
    let skills = heycode_skills::discover(&roots);
    assert_eq!(skills.len(), 2);
    let review = skills.iter().find(|s| s.name == "review").unwrap();
    assert_eq!(review.description, "Review code changes"); // project wins
    assert!(!review.disable_model_invocation);
    let deploy = skills.iter().find(|s| s.name == "deploy").unwrap();
    assert!(deploy.disable_model_invocation);
}

#[tokio::test]
async fn catalog_lists_invocable_only_and_loader_enforces_restriction() {
    let project = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Say hello warmly",
        "# Greet\nSay hello.",
    );
    write_skill(
        &project.path().join(".agents/skills"),
        "secret-deploy",
        "name: secret-deploy\ndescription: Deploy\ndisable-model-invocation: true",
        "# Deploy steps",
    );
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![
            stop_text("name-only catalog"),
            stop_text("name-only explicit"),
            stop_text("user-only catalog"),
            stop_text("user-only explicit"),
            stop_text("off catalog"),
        ]),
        sink: requests.clone(),
    });

    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(project.path().to_path_buf()),
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".agents/skills")]),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    // Catalog section reaches the model WITHOUT the restricted skill.
    agent.send("what can you do?").await.unwrap();
    {
        let reqs = requests.lock().unwrap();
        let system = reqs[0]
            .messages
            .iter()
            .find(|m| m.role == heycode_llm::Role::System)
            .unwrap();
        assert!(system.content.contains("- greet: Say hello warmly"));
        assert!(!system.content.contains("secret-deploy"));
    }

    // Loader serves the invocable skill…
    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let out = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert!(out.as_str().unwrap().contains("<skill name=\"greet\">"));

    // …and refuses the user-only one with actionable guidance.
    let err = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "secret-deploy"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("user-invoked only"), "{}", err.message);
}

#[tokio::test]
async fn persisted_preferences_gate_every_invocation_and_refuse_stale_or_failed_writes() {
    let project = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Say hello warmly",
        "INITIAL-GREET-BODY",
    );
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![stop_text("ok")]),
        sink: requests.clone(),
    });
    let writer = Arc::new(RecordingSettingsWriter::default());
    let settings = heycode_settings::SettingsService::with_writer(
        heycode_settings::SettingsDocuments::new(),
        writer.clone(),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(project.path().to_path_buf()),
        prompt_plugin(),
        Box::new(WritableSettingsPlugin(settings.clone())),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".agents/skills")]),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let initial = skills.catalog_snapshot().unwrap();
    assert!(initial.records()[0].enabled());
    assert_eq!(
        initial.records()[0].admission(),
        heycode_skills::SkillAdmission::On
    );
    assert_eq!(initial.sort(), heycode_skills::SkillSort::Name);

    let name_only = skills
        .set_admission("greet", heycode_skills::SkillAdmission::NameOnly, &initial)
        .unwrap();
    assert_eq!(
        name_only.records()[0].admission(),
        heycode_skills::SkillAdmission::NameOnly
    );
    assert_eq!(
        skills.model_invocable_snapshot().unwrap()[0].description,
        ""
    );
    assert_eq!(
        settings
            .get(&heycode_skills::settings_namespace().unwrap())
            .unwrap()
            .unwrap()
            .resolved()["name_only"],
        serde_json::json!(["greet"])
    );

    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let loaded = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert!(loaded.as_str().unwrap().contains("INITIAL-GREET-BODY"));

    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Hidden catalog description",
        "NAME-ONLY-UPDATED-BODY",
    );
    let name_only_reload = skills.reload().unwrap();
    assert!(
        !name_only_reload.prompt_changed,
        "description and body changes are invisible to the name-only prompt projection"
    );
    let reloaded_name_only = skills.catalog_snapshot().unwrap();
    assert_eq!(
        reloaded_name_only.records()[0].admission(),
        heycode_skills::SkillAdmission::NameOnly
    );
    assert_eq!(
        skills.model_invocable_snapshot().unwrap()[0].description,
        ""
    );
    let loaded = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert!(loaded.as_str().unwrap().contains("NAME-ONLY-UPDATED-BODY"));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.send("NAME_ONLY_CATALOG_REQUEST"),
    )
    .await
    .expect("name-only catalog turn must finish")
    .unwrap();
    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        commands
            .get("skill")
            .unwrap()
            .unwrap()
            .execute(&agent, "greet NAME_ONLY_SKILL_REQUEST"),
    )
    .await
    .expect("name-only explicit turn must finish")
    .unwrap();

    let user_only = skills
        .set_admission(
            "greet",
            heycode_skills::SkillAdmission::UserOnly,
            &reloaded_name_only,
        )
        .unwrap();
    assert_eq!(
        user_only.records()[0].admission(),
        heycode_skills::SkillAdmission::UserOnly
    );
    assert!(skills.model_invocable_snapshot().unwrap().is_empty());
    let user_only_error = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(
        user_only_error.message.contains("user-invoked only"),
        "{user_only_error:?}"
    );
    assert!(skills.get_enabled("greet").unwrap().is_some());
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.send("USER_ONLY_CATALOG_REQUEST"),
    )
    .await
    .expect("user-only catalog turn must finish")
    .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        commands
            .get("skill")
            .unwrap()
            .unwrap()
            .execute(&agent, "greet USER_ONLY_SKILL_REQUEST"),
    )
    .await
    .expect("user-only explicit turn must finish")
    .unwrap();

    let disabled = skills
        .set_admission("greet", heycode_skills::SkillAdmission::Off, &user_only)
        .unwrap();
    assert!(!disabled.records()[0].enabled());
    assert_eq!(
        disabled.records()[0].admission(),
        heycode_skills::SkillAdmission::Off
    );
    assert_eq!(writer.writes.lock().unwrap().len(), 3);
    assert_eq!(
        settings
            .get(&heycode_skills::settings_namespace().unwrap())
            .unwrap()
            .unwrap()
            .resolved()["disabled"],
        serde_json::json!(["greet"])
    );

    let load_error = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap_err();
    assert!(load_error.message.contains("disabled"), "{load_error:?}");
    let command_error = commands
        .get("skill")
        .unwrap()
        .unwrap()
        .execute(&agent, "greet run this")
        .await
        .unwrap_err();
    assert!(command_error.to_string().contains("disabled"));
    assert_eq!(requests.lock().unwrap().len(), 4);

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.send("OFF_CATALOG_REQUEST"),
    )
    .await
    .expect("off catalog turn must finish")
    .unwrap();
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 5);
    let system = recorded[0]
        .messages
        .iter()
        .find(|message| message.role == heycode_llm::Role::System)
        .unwrap();
    assert!(system.content.contains("\n- greet\n"));
    assert!(!system.content.contains("Say hello warmly"));
    assert!(!system.content.contains("Hidden catalog description"));
    assert!(
        recorded[1]
            .messages
            .iter()
            .any(|message| message.content.contains("NAME-ONLY-UPDATED-BODY"))
    );
    let system = recorded[2]
        .messages
        .iter()
        .find(|message| message.role == heycode_llm::Role::System)
        .unwrap();
    assert!(!system.content.contains("\n- greet"));
    assert!(
        recorded[3]
            .messages
            .iter()
            .any(|message| message.content.contains("NAME-ONLY-UPDATED-BODY"))
    );
    let system = recorded[4]
        .messages
        .iter()
        .find(|message| message.role == heycode_llm::Role::System)
        .unwrap();
    assert!(!system.content.contains("\n- greet"));
    drop(recorded);

    let sorted = skills
        .set_sort(heycode_skills::SkillSort::Source, &disabled)
        .unwrap();
    assert_eq!(sorted.sort(), heycode_skills::SkillSort::Source);
    let writes_after_sort = writer.writes.lock().unwrap().len();
    let stale_settings = skills.set_enabled("greet", true, &disabled).unwrap_err();
    assert!(stale_settings.to_string().contains("changed since"));
    assert_eq!(writer.writes.lock().unwrap().len(), writes_after_sort);
    assert!(!skills.catalog_snapshot().unwrap().records()[0].enabled());

    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Updated but disabled",
        "UPDATED-GREET-BODY",
    );
    let outcome = skills.reload().unwrap();
    assert!(
        !outcome.prompt_changed,
        "disabled skills stay out of the prompt"
    );
    let stale_generation = skills.set_enabled("greet", true, &sorted).unwrap_err();
    assert!(stale_generation.to_string().contains("changed since"));
    assert_eq!(writer.writes.lock().unwrap().len(), writes_after_sort);

    let current = skills.catalog_snapshot().unwrap();
    writer.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    let persistence_error = skills.set_enabled("greet", true, &current).unwrap_err();
    assert!(
        persistence_error
            .to_string()
            .contains("fixture persistence failure")
    );
    assert!(!skills.catalog_snapshot().unwrap().records()[0].enabled());
}

#[tokio::test]
async fn skill_command_injects_body_into_the_turn_even_for_user_only() {
    let project = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "deploy",
        "name: deploy\ndescription: Deploy\ndisable-model-invocation: true",
        "STEP-ONE: run migrations",
    );
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![stop_text("deploying")]),
        sink: requests.clone(),
    });

    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(project.path().to_path_buf()),
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".heycode/skills")]),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let catalog = skills.catalog_snapshot().unwrap();
    assert_eq!(
        catalog.records()[0].admission(),
        heycode_skills::SkillAdmission::UserOnly
    );
    let widening = skills
        .set_admission("deploy", heycode_skills::SkillAdmission::On, &catalog)
        .unwrap_err();
    assert!(
        matches!(
            widening,
            heycode_skills::SkillPreferenceError::SourceRestricted { .. }
        ),
        "{widening}"
    );

    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let (name, args) = parse_slash("/skill deploy ship to staging now").unwrap();
    assert_eq!(name, "skill");
    commands
        .get("skill")
        .unwrap()
        .unwrap()
        .execute(&agent, &args)
        .await
        .unwrap();

    {
        let reqs = requests.lock().unwrap();
        let user_msg = reqs[0]
            .messages
            .iter()
            .find(|m| m.role == heycode_llm::Role::User)
            .unwrap();
        assert!(user_msg.content.contains("<skill name=\"deploy\">"));
        assert!(user_msg.content.contains("STEP-ONE: run migrations"));
        assert!(user_msg.content.contains("ship to staging now"));
    }
    let panels = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = panels.clone();
    ctx.events.on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::CapabilityPanelRequested { panel } = event {
            sink.lock().unwrap().push(panel.as_str().to_owned());
        }
    });
    let diagnostic = {
        let session = agent.session().lock().unwrap();
        heycode_skills::SkillDoctorSnapshot::capture(&skills, session.events()).unwrap()
    };
    let deploy = diagnostic
        .rows()
        .iter()
        .find(|row| row.name() == "deploy")
        .unwrap();
    assert_eq!(deploy.admission(), heycode_skills::SkillAdmission::UserOnly);
    assert_eq!(deploy.context_tokens(), None);
    assert_eq!(deploy.session_uses(), 1);
    assert_eq!(deploy.retained_in_context(), 1);
    assert!(!diagnostic.seven_day_history_available());
    let before = requests.lock().unwrap().len();
    commands
        .get("skill-doctor")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    assert_eq!(panels.lock().unwrap().as_slice(), ["skill-doctor"]);
    assert_eq!(before, requests.lock().unwrap().len());
}

/// `/skills` requests the capability-owned panel while the service retains the
/// exact catalog the front end will render.
#[tokio::test]
async fn skills_command_requests_the_catalog_panel() {
    let project = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Say hello warmly",
        "# Greet\nSay hello.",
    );
    write_skill(
        &project.path().join(".agents/skills"),
        "secret-deploy",
        "name: secret-deploy\ndescription: Deploy\ndisable-model-invocation: true",
        "# Deploy steps",
    );
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider::new(vec![stop_text("ok")]));
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(project.path().to_path_buf()),
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".agents/skills")]),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    ctx.events.on::<heycode_agent::UiEvent>(move |e| {
        if let heycode_agent::UiEvent::CapabilityPanelRequested { panel } = e {
            sink.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(panel.as_str().to_owned());
        }
    });

    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let (name, args) = parse_slash("/skills").unwrap();
    assert_eq!(name, "skills");
    commands
        .get("skills")
        .unwrap()
        .unwrap()
        .execute(&agent, &args)
        .await
        .unwrap();

    assert_eq!(
        seen.lock().unwrap_or_else(|p| p.into_inner()).as_slice(),
        ["skills"]
    );
    let catalog = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    assert!(
        catalog
            .snapshot()
            .unwrap()
            .iter()
            .any(|skill| { skill.name == "greet" && skill.description == "Say hello warmly" })
    );
    assert!(
        catalog
            .snapshot()
            .unwrap()
            .iter()
            .any(|skill| { skill.name == "secret-deploy" && skill.disable_model_invocation })
    );
}

#[cfg(unix)]
#[test]
fn discovery_refuses_symlinked_skill_directories_and_documents() {
    use std::os::unix::fs::symlink;

    let authority = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_skill(
        outside.path(),
        "directory-target",
        "name: escaped-directory\ndescription: Must stay outside",
        "OUTSIDE-DIRECTORY-TEXT",
    );
    std::fs::create_dir_all(authority.path().join("skills")).unwrap();
    symlink(
        outside.path().join("directory-target"),
        authority.path().join("skills/directory-link"),
    )
    .unwrap();

    let local = authority.path().join("skills/file-link");
    std::fs::create_dir_all(&local).unwrap();
    let outside_document = outside.path().join("outside-SKILL.md");
    std::fs::write(
        &outside_document,
        "---\nname: escaped-file\ndescription: Must stay outside\n---\nOUTSIDE-FILE-TEXT\n",
    )
    .unwrap();
    symlink(&outside_document, local.join("SKILL.md")).unwrap();

    let roots = vec![skill_root(authority.path(), "skills")];
    let skills = heycode_skills::discover(&roots);
    assert!(skills.is_empty(), "symlinks must not become skill text");
}

#[test]
fn late_skill_registration_is_atomic_and_exactly_disposable() {
    let skills = heycode_skills::SkillSet::new(Vec::new()).unwrap();
    let registration = skills
        .register_owned(heycode_skills::Skill {
            name: "acme-review".to_owned(),
            description: "Review one change".to_owned(),
            disable_model_invocation: false,
            body: "Review the supplied change carefully.".to_owned(),
        })
        .unwrap();

    assert_eq!(skills.snapshot().unwrap().len(), 1);
    assert!(skills.get("acme-review").unwrap().is_some());
    let duplicate = skills
        .register_owned(heycode_skills::Skill {
            name: "acme-review".to_owned(),
            description: String::new(),
            disable_model_invocation: false,
            body: String::new(),
        })
        .err()
        .unwrap();
    assert!(duplicate.to_string().contains("already registered"));

    drop(registration);
    assert!(skills.get("acme-review").unwrap().is_none());
}

#[test]
fn persisted_legacy_alias_disables_the_new_canonical_contribution_identity() {
    let mut documents = heycode_settings::SettingsDocuments::new();
    documents
        .set_user(
            heycode_skills::settings_namespace().unwrap(),
            serde_json::json!({
                "disabled": ["ext-review-deadbeefdeadbeef"],
                "sort": "name"
            }),
        )
        .unwrap();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        heycode_settings::settings_plugin(documents),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(Vec::new()),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let _registration = skills
        .register_owned_with_aliases(
            heycode_skills::Skill {
                name: "acme/product::review".to_owned(),
                description: "Review one change".to_owned(),
                disable_model_invocation: false,
                body: "Review carefully.".to_owned(),
            },
            vec!["ext-review-deadbeefdeadbeef".to_owned()],
        )
        .unwrap();
    assert_eq!(
        skills
            .get("ext-review-deadbeefdeadbeef")
            .unwrap()
            .unwrap()
            .name,
        "acme/product::review"
    );
    assert!(!skills.catalog_snapshot().unwrap().records()[0].enabled());
    assert!(matches!(
        skills.get_enabled("acme/product::review"),
        Err(heycode_skills::SkillRegistryError::Disabled { .. })
    ));
}

#[test]
fn imported_skill_scopes_shadow_and_restore_one_effective_winner_everywhere() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &home.path().join("skills"),
        "shared",
        "name: shared\ndescription: NATIVE USER",
        "NATIVE-USER-BODY",
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(heycode_skills::default_roots(project.path(), home.path()).unwrap()),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let prompt = ctx
        .get::<heycode_prompt::PromptRegistry>(heycode_prompt::SERVICE_PROMPT)
        .unwrap();
    let render = || {
        prompt.render(&heycode_prompt::RenderContext {
            cwd: project.path().to_path_buf(),
            model: "fixture".to_owned(),
            tool_names: Vec::new(),
            plan_active: false,
        })
    };
    let assert_winner = |body: &str, description: &str, scope: &str| {
        let snapshot = skills.snapshot().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].body, body);
        assert_eq!(skills.get("shared").unwrap().unwrap().body, body);
        assert_eq!(skills.get_enabled("shared").unwrap().unwrap().body, body);
        let catalog = skills.catalog_snapshot().unwrap();
        assert_eq!(catalog.records().len(), 1);
        assert_eq!(catalog.records()[0].record().skill.body, body);
        assert_eq!(catalog.records()[0].record().source.scope().as_str(), scope);
        let rendered = render();
        assert!(rendered.contains(description), "{rendered}");
    };

    assert_winner("NATIVE-USER-BODY\n", "NATIVE USER", "user");
    let imported_user = skills
        .register_imported_owned(
            heycode_skills::Skill {
                name: "shared".to_owned(),
                description: "IMPORTED USER".to_owned(),
                disable_model_invocation: false,
                body: "IMPORTED-USER-BODY".to_owned(),
            },
            heycode_skills::SkillSourceScope::User,
            "import-generation-1",
        )
        .unwrap();
    assert_winner("NATIVE-USER-BODY\n", "NATIVE USER", "user");

    let imported_project = skills
        .register_imported_owned(
            heycode_skills::Skill {
                name: "shared".to_owned(),
                description: "IMPORTED PROJECT".to_owned(),
                disable_model_invocation: false,
                body: "IMPORTED-PROJECT-BODY".to_owned(),
            },
            heycode_skills::SkillSourceScope::Project,
            "import-generation-2",
        )
        .unwrap();
    assert_winner("IMPORTED-PROJECT-BODY", "IMPORTED PROJECT", "project");
    drop(imported_project);
    assert_winner("NATIVE-USER-BODY\n", "NATIVE USER", "user");

    let old_imported_project = skills
        .register_imported_owned(
            heycode_skills::Skill {
                name: "shared".to_owned(),
                description: "IMPORTED PROJECT AGAIN".to_owned(),
                disable_model_invocation: false,
                body: "IMPORTED-PROJECT-BODY-2".to_owned(),
            },
            heycode_skills::SkillSourceScope::Project,
            "import-generation-3",
        )
        .unwrap();
    assert_winner(
        "IMPORTED-PROJECT-BODY-2",
        "IMPORTED PROJECT AGAIN",
        "project",
    );
    write_skill(
        &project.path().join(".heycode/skills"),
        "shared",
        "name: shared\ndescription: NATIVE PROJECT",
        "NATIVE-PROJECT-BODY",
    );
    skills.reload().unwrap();
    assert_winner("NATIVE-PROJECT-BODY\n", "NATIVE PROJECT", "project");
    drop(old_imported_project);
    assert_winner("NATIVE-PROJECT-BODY\n", "NATIVE PROJECT", "project");
    drop(imported_user);
    assert_winner("NATIVE-PROJECT-BODY\n", "NATIVE PROJECT", "project");
}

#[cfg(unix)]
#[test]
fn discovery_refuses_a_symlinked_relative_ancestor() {
    use std::os::unix::fs::symlink;

    let authority = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_skill(
        &outside.path().join("skills"),
        "escape",
        "name: escaped-ancestor\ndescription: Must stay outside",
        "OUTSIDE-ANCESTOR-TEXT",
    );
    symlink(outside.path(), authority.path().join(".agents")).unwrap();

    let roots = vec![skill_root(authority.path(), ".agents/skills")];
    let skills = heycode_skills::discover(&roots);
    assert!(
        skills.is_empty(),
        "relative ancestors must be opened no-follow"
    );
}

#[cfg(unix)]
#[test]
fn held_authority_survives_ambient_workspace_replacement() {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir().unwrap();
    let workspace = container.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    write_skill(
        &workspace.join(".agents/skills"),
        "review",
        "name: review\ndescription: Authorized copy",
        "AUTHORIZED-TEXT",
    );
    let root = skill_root(&workspace, ".agents/skills");

    let held_workspace = container.path().join("held-workspace");
    std::fs::rename(&workspace, &held_workspace).unwrap();
    let outside = container.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    write_skill(
        &outside.join(".agents/skills"),
        "review",
        "name: review\ndescription: Escaped copy",
        "OUTSIDE-TEXT",
    );
    symlink(&outside, &workspace).unwrap();

    let skills = heycode_skills::discover(&[root]);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].description, "Authorized copy");
    assert!(skills[0].body.contains("AUTHORIZED-TEXT"));
    assert!(!skills[0].body.contains("OUTSIDE-TEXT"));
}

#[cfg(unix)]
#[test]
fn discovery_refuses_hard_linked_documents() {
    let authority = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_document = outside.path().join("SKILL.md");
    std::fs::write(
        &outside_document,
        "---\nname: hard-link\ndescription: Must stay outside\n---\nOUTSIDE-HARD-LINK-TEXT\n",
    )
    .unwrap();
    let local = authority.path().join("skills/hard-link");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::hard_link(&outside_document, local.join("SKILL.md")).unwrap();

    let skills = heycode_skills::discover(&[skill_root(authority.path(), "skills")]);
    assert!(
        skills.is_empty(),
        "multiply linked files must not be admitted"
    );
}

#[cfg(unix)]
#[test]
fn discovered_body_is_an_immutable_authorized_snapshot() {
    use std::os::unix::fs::symlink;

    let authority = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_skill(
        &authority.path().join("skills"),
        "review",
        "name: review\ndescription: Authorized snapshot",
        "AUTHORIZED-SNAPSHOT",
    );
    let skills = heycode_skills::discover(&[skill_root(authority.path(), "skills")]);
    assert_eq!(skills.len(), 1);

    let document = authority.path().join("skills/review/SKILL.md");
    std::fs::remove_file(&document).unwrap();
    let outside_document = outside.path().join("SKILL.md");
    std::fs::write(&outside_document, "OUTSIDE-LATE-SWAP").unwrap();
    symlink(&outside_document, &document).unwrap();

    assert!(skills[0].body.contains("AUTHORIZED-SNAPSHOT"));
    assert!(!skills[0].body.contains("OUTSIDE-LATE-SWAP"));
}

#[test]
fn same_root_name_collision_is_resolved_by_sorted_directory_name() {
    let authority = tempfile::tempdir().unwrap();
    write_skill(
        &authority.path().join("skills"),
        "z-last",
        "name: duplicate\ndescription: Later directory",
        "LATER",
    );
    write_skill(
        &authority.path().join("skills"),
        "a-first",
        "name: duplicate\ndescription: Earlier directory",
        "EARLIER",
    );

    let skills = heycode_skills::discover(&[skill_root(authority.path(), "skills")]);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].description, "Earlier directory");
    assert!(skills[0].body.contains("EARLIER"));
}

#[test]
fn skill_root_rejects_ambient_or_traversing_relative_paths() {
    let authority = tempfile::tempdir().unwrap();
    assert!(SkillRoot::bind(authority.path(), "").is_err());
    assert!(SkillRoot::bind(authority.path(), "../skills").is_err());
    assert!(SkillRoot::bind(authority.path(), authority.path()).is_err());
    assert!(SkillRoot::bind(authority.path().join("../escape"), "skills").is_err());
}

#[test]
fn missing_authority_suffix_stays_beneath_the_existing_capability() {
    let container = tempfile::tempdir().unwrap();
    let future_home = container.path().join("state/heycode-home");
    let root = SkillRoot::bind(&future_home, "skills").unwrap();
    assert!(heycode_skills::discover(std::slice::from_ref(&root)).is_empty());

    write_skill(
        &future_home.join("skills"),
        "later",
        "name: later\ndescription: Created after binding",
        "LATE-BUT-AUTHORIZED",
    );
    let skills = heycode_skills::discover(&[root]);
    assert_eq!(skills.len(), 1);
    assert!(skills[0].body.contains("LATE-BUT-AUTHORIZED"));
}

#[cfg(unix)]
#[test]
fn missing_authority_suffix_cannot_be_replaced_by_a_symlink() {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let future_home = container.path().join("state/heycode-home");
    let root = SkillRoot::bind(&future_home, "skills").unwrap();
    write_skill(
        &outside.path().join("heycode-home/skills"),
        "escape",
        "name: escaped-missing-home\ndescription: Must stay outside",
        "OUTSIDE-MISSING-HOME-TEXT",
    );
    symlink(outside.path(), container.path().join("state")).unwrap();

    let skills = heycode_skills::discover(&[root]);
    assert!(
        skills.is_empty(),
        "an absent authority suffix must remain no-follow"
    );
}

/// One skill whose declared name is malformed is a skipped row, not a reason
/// for the whole product to refuse to start. The skipped row names the
/// directory and why, so the user can fix the file.
#[test]
fn a_malformed_skill_name_is_skipped_with_a_reason_and_never_aborts() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "good",
        "name: good\ndescription: Works",
        "# Good",
    );
    write_skill(
        &project.path().join(".heycode/skills"),
        "broken",
        "name: has spaces in it\ndescription: Bad name",
        "# Broken",
    );
    let roots = heycode_skills::default_roots(project.path(), home.path()).unwrap();
    let discovered = heycode_skills::discover_report(&roots);
    assert_eq!(
        discovered
            .skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["good"]
    );
    assert_eq!(discovered.skipped.len(), 1);
    assert_eq!(discovered.skipped[0].directory, "broken");
    assert!(
        discovered.skipped[0].reason.contains("name"),
        "{}",
        discovered.skipped[0].reason
    );
    // The legacy accessor keeps returning only the usable skills.
    assert_eq!(heycode_skills::discover(&roots).len(), 1);
    // And a registry built from the report can still be constructed and lists
    // what it skipped for the panel.
    let set = heycode_skills::SkillSet::from_discovery(discovered).unwrap();
    assert_eq!(set.snapshot().unwrap().len(), 1);
    assert_eq!(set.skipped().len(), 1);
}

#[tokio::test]
async fn reload_command_atomically_refreshes_prompt_loader_and_dynamic_availability() {
    let project = tempfile::tempdir().unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(Recording {
        inner: FakeProvider::new(vec![stop_text("first"), stop_text("second")]),
        sink: requests.clone(),
    });
    let roots = vec![skill_root(project.path(), ".agents/skills")];
    let plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(project.path().to_path_buf()),
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        heycode_llm::model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: "fake".into(),
                model: "m".into(),
            },
            vec![provider],
        ),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        skills_plugin(roots),
        agent_options_plugin(AgentOptions::default()),
        heycode_agent::compactions_plugin(),
        agent_plugin(),
    ];
    let ctx = compose(&plugins).unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let commands = ctx
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    assert!(
        !commands
            .get("skill")
            .unwrap()
            .unwrap()
            .availability()
            .is_available()
    );

    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Initial catalog description",
        "INITIAL-BODY",
    );
    let output = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = output.clone();
    ctx.events.on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::Info { text } = event {
            sink.lock().unwrap().push(text.clone());
        }
    });
    commands
        .get("reload-skills")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    let first_reload = output.lock().unwrap().last().unwrap().clone();
    assert!(first_reload.contains("1 added"), "{first_reload}");
    assert!(
        first_reload.contains("model catalog changed"),
        "{first_reload}"
    );
    assert!(
        commands
            .get("skill")
            .unwrap()
            .unwrap()
            .availability()
            .is_available()
    );
    agent.send("catalog one").await.unwrap();

    write_skill(
        &project.path().join(".agents/skills"),
        "greet",
        "name: greet\ndescription: Updated catalog description",
        "UPDATED-BODY",
    );
    commands
        .get("reload-skills")
        .unwrap()
        .unwrap()
        .execute(&agent, "")
        .await
        .unwrap();
    agent.send("catalog two").await.unwrap();

    {
        let recorded = requests.lock().unwrap();
        let first_system = recorded[0]
            .messages
            .iter()
            .find(|message| message.role == heycode_llm::Role::System)
            .unwrap();
        let second_system = recorded[1]
            .messages
            .iter()
            .find(|message| message.role == heycode_llm::Role::System)
            .unwrap();
        assert!(first_system.content.contains("Initial catalog description"));
        assert!(!first_system.content.contains("Updated catalog description"));
        assert!(
            second_system
                .content
                .contains("Updated catalog description")
        );
    }

    let tools = ctx
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    let loaded = tools
        .get("load_skill")
        .unwrap()
        .run(
            serde_json::json!({"name": "greet"}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    assert!(loaded.as_str().unwrap().contains("UPDATED-BODY"));
}

#[test]
fn reload_preserves_the_entire_previous_generation_on_live_contribution_collision() {
    let project = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "stable",
        "name: stable\ndescription: Stable",
        "OLD-STABLE-BODY",
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".heycode/skills")]),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let live = skills
        .register_owned(heycode_skills::Skill {
            name: "new-name".into(),
            description: "Live contribution".into(),
            disable_model_invocation: false,
            body: "LIVE-BODY".into(),
        })
        .unwrap();
    let generation = skills.generation().unwrap();

    write_skill(
        &project.path().join(".heycode/skills"),
        "stable",
        "name: stable\ndescription: Changed but must not publish",
        "UNPUBLISHED-BODY",
    );
    write_skill(
        &project.path().join(".heycode/skills"),
        "collision",
        "name: new-name\ndescription: Filesystem collision",
        "COLLISION-BODY",
    );
    let error = skills.reload().unwrap_err();
    assert!(error.to_string().contains("live contribution `new-name`"));
    assert_eq!(skills.generation().unwrap(), generation);
    assert_eq!(
        skills.get("stable").unwrap().unwrap().body,
        "OLD-STABLE-BODY\n"
    );
    assert_eq!(skills.get("new-name").unwrap().unwrap().body, "LIVE-BODY");
    assert_eq!(skills.reload_diagnostics().len(), 1);
    drop(live);
}

#[test]
fn reload_reports_source_scoped_deltas_partial_skips_and_honest_prompt_effect() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "project-only",
        "name: project-only\ndescription: Project skill\ndisable-model-invocation: true",
        "ONE",
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(heycode_skills::default_roots(project.path(), home.path()).unwrap()),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let records = skills.snapshot_records().unwrap();
    assert_eq!(records[0].source.scope().as_str(), "project");
    assert_eq!(records[0].source.root(), ".heycode/skills");
    assert_eq!(records[0].source.directory(), "project-only");

    write_skill(
        &project.path().join(".heycode/skills"),
        "project-only",
        "name: project-only\ndescription: Project skill\ndisable-model-invocation: true",
        "TWO",
    );
    write_skill(
        &home.path().join("skills"),
        "broken",
        "name: invalid name\ndescription: skipped",
        "BROKEN",
    );
    let outcome = skills.reload().unwrap();
    assert_eq!(outcome.added, 0);
    assert_eq!(outcome.removed, 0);
    assert_eq!(outcome.updated, 1);
    assert_eq!(outcome.skipped, 1);
    assert!(
        !outcome.prompt_changed,
        "user-only body is absent from prompt"
    );
    assert!(
        outcome
            .render()
            .contains("no prompt-cache invalidation is claimed")
    );
    let unchanged_generation = skills.generation().unwrap();
    let unchanged = skills.reload().unwrap();
    assert_eq!(unchanged.generation, unchanged_generation);
    assert_eq!(
        (unchanged.added, unchanged.removed, unchanged.updated),
        (0, 0, 0)
    );
}

#[test]
fn reload_reports_prompt_change_when_source_order_changes_catalog_order() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_skill(
        &project.path().join(".heycode/skills"),
        "zeta",
        "name: zeta\ndescription: Last alphabetically",
        "ZETA",
    );
    write_skill(
        &home.path().join("skills"),
        "alpha",
        "name: alpha\ndescription: First alphabetically",
        "ALPHA",
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        heycode_settings::settings_plugin(heycode_settings::SettingsDocuments::new()),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(heycode_skills::default_roots(project.path(), home.path()).unwrap()),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    assert_eq!(
        skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["zeta", "alpha"]
    );

    std::fs::remove_dir_all(project.path().join(".heycode/skills/zeta")).unwrap();
    write_skill(
        &home.path().join("skills"),
        "zeta",
        "name: zeta\ndescription: Last alphabetically",
        "ZETA",
    );
    let outcome = skills.reload().unwrap();
    assert_eq!((outcome.added, outcome.removed, outcome.updated), (0, 0, 1));
    assert!(outcome.prompt_changed);
    assert_eq!(
        skills
            .snapshot()
            .unwrap()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zeta"]
    );
}

#[test]
fn token_sort_uses_the_displayed_estimate_and_preserves_persisted_source_sort() {
    let project = tempfile::tempdir().unwrap();
    for (name, description) in [
        ("alpha", "Short".to_owned()),
        ("beta", "Long catalog entry ".repeat(24)),
        ("gamma", "Short".to_owned()),
    ] {
        write_skill(
            &project.path().join(".heycode/skills"),
            name,
            &format!("name: {name}\ndescription: {description}"),
            "Body",
        );
    }
    let writer = Arc::new(RecordingSettingsWriter::default());
    let settings = heycode_settings::SettingsService::with_writer(
        heycode_settings::SettingsDocuments::new(),
        writer.clone(),
    );
    let plugins: Vec<Box<dyn Plugin>> = vec![
        prompt_plugin(),
        Box::new(WritableSettingsPlugin(settings)),
        execution_plugin(),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        commands_plugin(),
        skills_plugin(vec![skill_root(project.path(), ".heycode/skills")]),
    ];
    let ctx = compose(&plugins).unwrap();
    let skills = ctx
        .get::<heycode_skills::SkillSet>(heycode_skills::SERVICE_SKILLS)
        .unwrap();
    let original = skills.catalog_snapshot().unwrap();
    let sorted = skills
        .set_sort(heycode_skills::SkillSort::Tokens, &original)
        .unwrap();
    let names = sorted
        .records()
        .iter()
        .map(|row| row.record().skill.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["beta", "alpha", "gamma"]);
    assert!(
        sorted.records()[0].estimated_catalog_tokens()
            > sorted.records()[1].estimated_catalog_tokens()
    );
    assert_eq!(
        writer.writes.lock().unwrap().last().unwrap()["sort"],
        "tokens"
    );
    assert!(
        skills
            .set_sort(heycode_skills::SkillSort::Name, &original)
            .is_err()
    );
    let disabled = skills
        .set_admission("beta", heycode_skills::SkillAdmission::Off, &sorted)
        .unwrap();
    assert_eq!(
        disabled.records()[0].record().skill.name,
        "beta",
        "catalog cost is independent of admission"
    );
    let source = skills
        .set_sort(heycode_skills::SkillSort::Source, &disabled)
        .unwrap();
    assert_eq!(source.sort(), heycode_skills::SkillSort::Source);
    assert_eq!(
        heycode_skills::SkillSort::Name.next(),
        heycode_skills::SkillSort::Tokens
    );
    assert_eq!(
        heycode_skills::SkillSort::Tokens.next(),
        heycode_skills::SkillSort::Name
    );
    assert_eq!(
        heycode_skills::SkillSort::Source.next(),
        heycode_skills::SkillSort::Name
    );
}
