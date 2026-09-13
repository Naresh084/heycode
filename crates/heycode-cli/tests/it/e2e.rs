//! CLI-level end-to-end proofs: credential ladder, resume, and REAL file
//! mutations flowing through the full plugin stack on the fake provider.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use heycode_cli::{
    WorldOptions, compose_world, find_latest_session, lookup_credential_at, parse_credentials,
};
use heycode_config::{CONFIG_SCHEMA_VERSION, Config};
use heycode_llm::StreamChunk;
use heycode_llm::testing::FakeProvider;
use heycode_session::{Session, SessionEventKind};

const GENERATED_CONFIG_V0: &str = "[profile]\nplugins = [\"session\", \"prompt\", \"tools\", \"llm\", \"approval\", \"commands\", \"agent\", \"tui\"]\n\n[llm]\nprovider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-4\"\n";
const CUSTOM_CONFIG_V0: &str = "[profile]\nplugins = [\"session\", \"prompt\", \"tools\", \"llm\", \"approval\", \"commands\", \"skills\", \"agent\", \"tui\"]\n\n[llm]\nprovider = \"openrouter\"\nmodel = \"custom/model\"\n";

fn call(id: &str, tool: &str, args: serde_json::Value) -> StreamChunk {
    StreamChunk::ToolCallDelta {
        index: 0,
        id: Some(id.to_owned()),
        name: Some(tool.to_owned()),
        arguments_delta: args.to_string(),
    }
}

fn stop_text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta(text.to_owned()),
        StreamChunk::Finish(heycode_llm::FinishReason::Stop),
    ]
}

// The helper above got clever; keep it simple instead:
fn tool_script(call_chunk: StreamChunk) -> Vec<StreamChunk> {
    vec![
        call_chunk,
        StreamChunk::Finish(heycode_llm::FinishReason::ToolCalls),
    ]
}

#[tokio::test]
async fn agent_mutates_real_files_through_write_edit_bash_chain() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("e2e.txt");

    let scripts = vec![
        // turn 1 step 1: model asks to WRITE
        tool_script(call(
            "c1",
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "v1-line\n"}),
        )),
        // turn 1 step 2: confirms
        stop_text("written"),
        // turn 2 step 1: model EDITS
        tool_script(call(
            "c2",
            "edit",
            serde_json::json!({
                "path": target.to_str().unwrap(),
                "old_string": "v1-line",
                "new_string": "v2-EDITED"
            }),
        )),
        stop_text("edited"),
        // turn 3 step 1: model runs BASH to read it back
        tool_script(call(
            "c3",
            "bash",
            serde_json::json!({"command": format!("cat {}", target.display())}),
        )),
        stop_text("verified"),
    ];

    let cfg = Config::defaults();
    let ctx = compose_world(&WorldOptions {
        config: &cfg,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().to_path_buf(),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("credentials-home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: Some(Arc::new(FakeProvider::new(scripts))),
        resume: None,
    })
    .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();

    agent.send("create it").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1-line\n");

    agent.send("edit it").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "v2-EDITED\n",
        "the edit tool must mutate the real file"
    );

    agent.send("verify with shell").await.unwrap();
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    let saw_shell_output = s.events().iter().any(|e| {
        matches!(
            &e.kind,
            SessionEventKind::ToolResult { content, is_error: false, .. }
                if content.contains("v2-EDITED") && content.contains("[exit code: 0]")
        )
    });
    assert!(saw_shell_output, "bash must observe the mutated file");
}

#[test]
fn workspace_trust_precedes_automatic_project_config_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("heycode.toml"),
        "project-secret-canary = [malformed",
    )
    .unwrap();

    let run = |trust: Option<&str>| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"));
        if let Some(trust) = trust {
            command.arg(trust);
        }
        command
            .args(["--fake", "run", "trust smoke"])
            .env("HEYCODE_HOME", &home)
            .current_dir(&workspace)
            .output()
            .unwrap()
    };

    let unknown = run(None);
    assert_eq!(unknown.status.code(), Some(1));
    let unknown_error = String::from_utf8(unknown.stderr).unwrap();
    assert!(unknown_error.contains("workspace trust is required"));
    assert!(!unknown_error.contains("project-secret-canary"));

    let restricted = run(Some("--restricted-workspace"));
    assert!(
        restricted.status.success(),
        "{}",
        String::from_utf8_lossy(&restricted.stderr)
    );
    assert!(String::from_utf8_lossy(&restricted.stdout).contains("FAKE-REPLY"));

    let trusted = run(Some("--trust-workspace"));
    assert_eq!(trusted.status.code(), Some(1));
    let trusted_error = String::from_utf8(trusted.stderr).unwrap();
    assert!(trusted_error.contains("invalid config"), "{trusted_error}");
}

#[test]
fn relative_heycode_home_never_becomes_project_owned_authority() {
    let workspace = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--fake",
            "run",
            "home boundary smoke",
        ])
        .env("HEYCODE_HOME", "project-controlled-home")
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        error.contains("HEYCODE_HOME must be an absolute path"),
        "{error}"
    );
    assert!(!workspace.path().join("project-controlled-home").exists());
}

#[test]
fn credentials_file_parses_and_env_wins_is_documented_by_lookup_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials");
    std::fs::write(
        &path,
        "# comment\nDEEPSEEK_API_KEY=sk-from-file\nBAD LINE WITHOUT EQUALS\nOPENROUTER_API_KEY=sk-or-x\n",
    )
    .unwrap();
    let err = parse_credentials(&path).unwrap_err();
    assert!(err.to_string().contains("expected KEY=value"), "{err}");

    std::fs::write(
        &path,
        "DEEPSEEK_API_KEY=sk-from-file\nOPENROUTER_API_KEY=sk-or-x\n",
    )
    .unwrap();
    let creds = parse_credentials(&path).unwrap();
    assert_eq!(creds.len(), 2);
    assert!(
        creds
            .iter()
            .any(|(k, v)| k == "DEEPSEEK_API_KEY" && v == "sk-from-file")
    );
}

#[test]
fn bootstrap_lookup_migrates_legacy_and_reads_the_new_provider_stack() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("home");
    std::fs::create_dir_all(&root).unwrap();
    let reference = "HEYCODE_TEST_BOOTSTRAP_KEY_A18F";
    std::fs::write(
        root.join("credentials"),
        format!("{reference}=bootstrap-secret\n"),
    )
    .unwrap();

    assert_eq!(
        lookup_credential_at(reference, &root).unwrap().as_deref(),
        Some("bootstrap-secret")
    );
    assert!(!root.join("credentials").exists());
    assert!(root.join("credentials.toml").is_file());
    assert_eq!(
        lookup_credential_at(reference, &root).unwrap().as_deref(),
        Some("bootstrap-secret")
    );
}

#[test]
fn real_binary_migrates_the_setup_generated_home_profile_before_composition() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = home.join("config.toml");
    std::fs::write(&config_path, GENERATED_CONFIG_V0).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args(["--restricted-workspace", "--fake", "run", "migration smoke"])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let migrated = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        migrated.contains(&format!("schema_version = {CONFIG_SCHEMA_VERSION}")),
        "{migrated}"
    );
    assert!(!migrated.contains("[profile]"), "{migrated}");
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml.unversioned.bak")).unwrap(),
        GENERATED_CONFIG_V0
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("migrated config schema"), "{stderr}");
    assert!(stderr.contains("skills"), "{stderr}");
    assert!(stderr.contains("mcp"), "{stderr}");
    assert!(stderr.contains("plan"), "{stderr}");
}

#[test]
fn real_binary_versions_but_preserves_an_intentional_home_profile() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = home.join("config.toml");
    std::fs::write(&config_path, CUSTOM_CONFIG_V0).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--fake",
            "run",
            "custom migration smoke",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let migrated = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        migrated.contains(&format!("schema_version = {CONFIG_SCHEMA_VERSION}")),
        "{migrated}"
    );
    assert!(migrated.contains("[profile]"), "{migrated}");
    assert!(migrated.contains("\"skills\""), "{migrated}");
    assert!(migrated.contains("\"agent-options\""), "{migrated}");
    assert!(migrated.contains("\"ui\""), "{migrated}");
    assert!(
        !migrated.contains("\"subagent\""),
        "minimal profiles must not gain optional inspection runtime plugins: {migrated}"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("restored built-in profile"));
}

#[test]
fn real_binary_migrates_only_the_setup_generated_v1_deepseek_default() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = home.join("config.toml");
    let original = "schema_version = 1\n\n[llm]\nprovider = \"deepseek\"\nmodel = \"deepseek-chat\"\n\n[tools]\nbash_timeout_ms = 30000\nread_max_bytes = 262144\nread_max_lines = 2000\n";
    std::fs::write(&config_path, original).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--fake",
            "run",
            "deepseek default migration smoke",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let migrated = std::fs::read_to_string(&config_path).unwrap();
    assert!(migrated.contains(&format!("schema_version = {CONFIG_SCHEMA_VERSION}")));
    assert!(migrated.contains("model = \"deepseek-v4-flash\""));
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml.v1.bak")).unwrap(),
        original
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("replaced retired DeepSeek default"),
        "{stderr}"
    );
}

#[test]
fn real_binary_selects_named_profile_through_the_shared_store() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        home.join("profiles/minimal.toml"),
        "schema_version = 1\nname = \"minimal\"\n\n[[plugins]]\nid = \"mcp\"\nenabled = false\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--profile",
            "minimal",
            "--fake",
            "run",
            "profile smoke",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("FAKE-REPLY"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn find_latest_session_uses_durable_event_activity_not_file_mtime() {
    let root = tempfile::tempdir().unwrap();
    assert!(find_latest_session(root.path()).unwrap().is_none());

    let mut first = Session::create(root.path()).unwrap();
    first
        .append(SessionEventKind::SessionTitle {
            title: "first".to_owned(),
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let mut second = Session::create(root.path()).unwrap();
    second
        .append(SessionEventKind::SessionTitle {
            title: "second".to_owned(),
        })
        .unwrap();
    assert_eq!(
        find_latest_session(root.path()).unwrap().unwrap(),
        second.path()
    );

    std::thread::sleep(std::time::Duration::from_millis(2));
    first.append(SessionEventKind::SessionActivated {}).unwrap();
    assert_eq!(
        find_latest_session(root.path()).unwrap().unwrap(),
        first.path()
    );
}

/// A corrupt log is one unreadable session, not a dead store: `-c` skips it
/// and lands on the newest session it can actually open — or on nothing, when
/// there is none — rather than refusing to start.
#[test]
fn find_latest_session_skips_a_corrupt_store_entry() {
    let root = tempfile::tempdir().unwrap();
    let corrupt = root.path().join("valid-looking-id");
    std::fs::create_dir(&corrupt).unwrap();
    std::fs::write(corrupt.join("session.jsonl"), "not-json\n").unwrap();
    assert_eq!(find_latest_session(root.path()).unwrap(), None);

    let healthy = heycode_session::Session::create(root.path()).unwrap();
    let healthy_path = healthy.path().to_path_buf();
    drop(healthy);
    assert_eq!(
        find_latest_session(root.path()).unwrap(),
        Some(healthy_path),
        "the readable session wins over the corrupt one"
    );
}

#[tokio::test]
async fn resume_replays_prior_events_into_a_live_agent() {
    let dir = tempfile::tempdir().unwrap();
    let sess_dir = dir.path().join("s1");
    let log_path;
    {
        let mut s = Session::create(&sess_dir).unwrap();
        log_path = s.path().to_path_buf();
        s.append(SessionEventKind::UserMessage {
            text: "remember this".into(),
        })
        .unwrap();
        s.append(SessionEventKind::AssistantMessage {
            turn: 1,
            step: 1,
            content: "noted".into(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        })
        .unwrap();
    }

    let cfg = Config::defaults();
    let ctx = compose_world(&WorldOptions {
        config: &cfg,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().to_path_buf(),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::Proxied,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("credentials-home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: Some(Arc::new(FakeProvider::new(vec![stop_text("continued")]))),
        resume: Some(log_path),
    })
    .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    {
        let s = agent.session().lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(s.events().len(), 2, "prior events are live again");
    }
    let report = agent.send("and continue").await.unwrap();
    assert_eq!(report.text, "continued");
}

/// The documented offline demo must survive more than one message: every turn
/// of a resumed `--fake` session gets a real reply, never a bare empty stop.
#[test]
fn fake_provider_replies_on_every_turn_of_a_resumed_session() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
            .args(["--restricted-workspace", "--fake"])
            .args(args)
            .env("HEYCODE_HOME", home.path())
            .current_dir(workspace.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(run(&["run", "first"]).contains("FAKE-REPLY"));
    assert!(
        run(&["-c", "run", "second"]).contains("FAKE-REPLY"),
        "second turn"
    );
    assert!(
        run(&["-c", "run", "third"]).contains("FAKE-REPLY"),
        "third turn"
    );

    let log = std::fs::read_dir(home.path().join("sessions"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("session.jsonl"))
        .find(|path| path.is_file())
        .expect("one session log");
    let text = std::fs::read_to_string(log).unwrap();
    let replies: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("\"assistant/message\""))
        .collect();
    assert_eq!(replies.len(), 3, "{text}");
    assert!(
        replies.iter().all(|line| line.contains("FAKE-REPLY")),
        "every assistant message carries the reply: {text}"
    );
}

/// Exiting with a connected stdio MCP server must not panic: the driver
/// runtime's last reference is released on a plain thread, never inside the
/// main runtime's shutdown.
#[test]
fn a_headless_run_with_a_connected_stdio_mcp_server_exits_cleanly() {
    let Ok(python) = which_python3() else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let server = workspace.path().join("mcp.py");
    std::fs::write(
        &server,
        r#"import sys, json
for line in sys.stdin:
    req=json.loads(line); m=req.get("method"); i=req.get("id")
    if m=="initialize": r={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"t","version":"1"}}
    elif m=="tools/list": r={"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","additionalProperties":False}}]}
    else: r={}
    if i is not None: sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":i,"result":r})+"\n"); sys.stdout.flush()
"#,
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("heycode.toml"),
        format!(
            "schema_version = 28\n[mcp.servers.t]\ncommand = \"{python}\"\nargs = [\"-u\", \"{}\"]\n",
            server.display()
        ),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args(["--trust-workspace", "--fake", "run", "hi"])
        .env("HEYCODE_HOME", home.path())
        .current_dir(workspace.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains("panicked"),
        "exit must not panic:\n{stderr}"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FAKE-REPLY"));
}

fn which_python3() -> Result<String, ()> {
    for candidate in ["python3", "python"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Ok(candidate.to_owned());
        }
    }
    Err(())
}

/// A user installs a declarative package by pointing at its directory — no
/// administrator policy, no marketplace. The package is verified into the
/// cache, listed, live in the next session, and removable.
#[cfg(unix)]
#[test]
fn a_declarative_package_installs_from_a_directory_and_its_skill_goes_live() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let package = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(package.path().join(".heycode-plugin")).unwrap();
    std::fs::create_dir_all(package.path().join("skills")).unwrap();
    std::fs::write(
        package.path().join(".heycode-plugin/plugin.toml"),
        r##"schema_version = 1
id = "me/hello"
name = "Hello skill"
version = "1.0.0"
description = "Concrete product activation fixture."
license = "MIT"
default_enabled = true
requested_permissions = ["network_access", "credential_use", "hook_registration", "mcp_connect", "process_spawn"]
platforms = [
  { os = "macos", architecture = "aarch64" },
  { os = "macos", architecture = "x86_64" },
  { os = "linux", architecture = "aarch64" },
  { os = "linux", architecture = "x86_64" },
  { os = "freebsd", architecture = "aarch64" },
  { os = "freebsd", architecture = "x86_64" },
]
dependencies = []
conflicts = []
[[contributions]]
kind = "skill"
id = "hello"
path = "skills/hello.md"
exposure = { mode = "namespaced" }

[api]
minimum = 1
maximum = 1

[source]
kind = "local"
locator = "pkg"
revision = "v1"
update_channel = "pinned"

[authentication]
policy = "none"
credentials = []
"##,
    )
    .unwrap();
    std::fs::write(
        package.path().join("skills/hello.md"),
        "---\nname: hello\ndescription: Say hello\n---\nAlways greet the user.\n",
    )
    .unwrap();
    let heycode = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
            .arg("--restricted-workspace")
            .args(args)
            .env("HEYCODE_HOME", home.path())
            .current_dir(workspace.path())
            .output()
            .unwrap();
        (
            output.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        )
    };
    let (ok, text) = heycode(&["plugin", "install", package.path().to_str().unwrap()]);
    assert!(ok, "{text}");
    assert!(text.contains("installed `me/hello` 1.0.0"), "{text}");
    let (ok, text) = heycode(&["plugin", "list"]);
    assert!(ok && text.contains("me/hello"), "{text}");
    let (ok, _) = heycode(&["--fake", "run", "hi"]);
    assert!(ok, "a session composes with the installed package");
    let (ok, text) = heycode(&["plugin", "remove", "me/hello"]);
    assert!(ok && text.contains("removed `me/hello`"), "{text}");
}

/// `approval.mode = ask` in a headless `heycode run` used to park the first tool
/// call on a dialog nobody could answer: the process hung forever with zero
/// output. Now the call is denied with a reason that names the fix, the turn
/// settles, and the reply still arrives.
#[tokio::test]
async fn headless_ask_denies_tool_calls_with_an_explanation_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("must-not-exist.txt");
    let scripts = vec![
        tool_script(call(
            "c1",
            "write",
            serde_json::json!({"path": target.to_str().unwrap(), "content": "nope\n"}),
        )),
        stop_text("understood"),
    ];
    let mut cfg = Config::defaults();
    cfg.approval.mode = Some(heycode_config::ApprovalMode::Ask);
    let ctx = compose_world(&WorldOptions {
        config: &cfg,
        trust: heycode_trust::WorkspaceTrustService::memory(
            dir.path(),
            heycode_cli::project_content_policy(),
        )
        .unwrap(),
        config_migration: None,
        profile_layers: &[],
        sessions_dir: dir.path().join("sessions"),
        attachments_dir: dir.path().join("attachments"),
        attachment_max_bytes: heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
        session_source: heycode_session::SessionSource::Headless,
        approval_prompter: heycode_cli::ApprovalPrompter::None,
        settings_user_path: dir.path().join("settings.toml"),
        credentials_root: dir.path().join("home"),
        catalog_cache_path: dir.path().join("cache/models.json"),
        settings_watch: false,
        onboarding_required: false,
        credential_validated_at_ms: None,
        cwd: dir.path().to_path_buf(),
        fake: Some(Arc::new(FakeProvider::new(scripts))),
        resume: None,
    })
    .unwrap();
    let agent = ctx
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let report = tokio::time::timeout(std::time::Duration::from_secs(10), agent.send("write it"))
        .await
        .expect("the turn settles instead of hanging on an unanswerable prompt")
        .unwrap();
    assert_eq!(report.reason, "stop");
    assert!(!target.exists(), "a denied write never happens");
    assert_eq!(
        agent.approval_kind(),
        heycode_agent::ApprovalPolicyKind::Ask
    );
    let session = ctx
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let s = session.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        s.events().iter().any(|e| matches!(
            &e.kind,
            SessionEventKind::ToolResult { content, is_error: true, .. }
                if content.contains("--approval full_access")
        )),
        "the model is told why and what would allow it"
    );
}

/// An interactive shell asks by default; a headless run auto-approves unless
/// told otherwise. Both surfaces honour an explicit setting.
#[test]
fn approval_defaults_follow_the_surface() {
    let unset = heycode_config::ApprovalSection::default();
    assert_eq!(unset.effective(true), heycode_config::ApprovalMode::Ask);
    assert_eq!(
        unset.effective(false),
        heycode_config::ApprovalMode::FullAccess
    );
    let explicit = heycode_config::ApprovalSection {
        mode: Some(heycode_config::ApprovalMode::Deny),
    };
    assert_eq!(explicit.effective(true), heycode_config::ApprovalMode::Deny);
    assert_eq!(
        explicit.effective(false),
        heycode_config::ApprovalMode::Deny
    );
}

#[test]
fn continue_uses_the_last_opened_conversation_in_this_folder() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(workspace.path()).unwrap();
    let other_cwd = std::fs::canonicalize(other_workspace.path()).unwrap();
    let service = heycode_session::SessionQueryService::local(root.path().to_path_buf());
    let create = |dir: &std::path::Path| {
        service
            .create(&heycode_session::SessionCreateRequest::new(
                heycode_session::SessionCreationMetadata::new(
                    Some(dir.to_path_buf()),
                    Some("native".into()),
                    heycode_session::SessionSource::Interactive,
                )
                .unwrap(),
            ))
            .unwrap()
    };
    let mut older = create(&cwd);
    older
        .append(SessionEventKind::UserMessage {
            text: "Fix login redirects".into(),
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let mut newer = create(&cwd);
    newer
        .append(SessionEventKind::UserMessage {
            text: "Add dark mode".into(),
        })
        .unwrap();
    assert_eq!(
        heycode_cli::find_latest_session_in(root.path(), &cwd)
            .unwrap()
            .as_deref(),
        Some(newer.path())
    );
    std::thread::sleep(std::time::Duration::from_millis(2));
    older.append(SessionEventKind::SessionActivated {}).unwrap();
    // Renaming another saved conversation must not steal continuation.
    newer
        .append(SessionEventKind::SessionTitle {
            title: "A saved name".into(),
        })
        .unwrap();
    let mut other = create(&other_cwd);
    other
        .append(SessionEventKind::UserMessage {
            text: "Work elsewhere".into(),
        })
        .unwrap();
    let _empty = create(&cwd);
    assert_eq!(
        heycode_cli::find_latest_session_in(root.path(), &cwd)
            .unwrap()
            .as_deref(),
        Some(older.path())
    );
    assert_eq!(
        heycode_cli::find_latest_session_in(root.path(), &other_cwd)
            .unwrap()
            .as_deref(),
        Some(other.path())
    );
}
