//! CMD05 real composition → command → UI → workspace proof.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_cli::testing::RealCompositionHarness;

#[tokio::test]
async fn production_init_command_previews_then_applies_without_model_history() {
    let harness = RealCompositionHarness::new().unwrap();
    let workspace = harness.root().join("workspace");
    std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
    let world = harness.compose().unwrap();
    let context = world.context();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let output = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = output.clone();
    agent.ui().on::<heycode_agent::UiEvent>(move |event| {
        if let heycode_agent::UiEvent::Info { text } = event
            && let Ok(mut output) = captured.lock()
        {
            output.push(text.clone());
        }
    });

    let command = commands.get("init").unwrap().unwrap();
    command.execute(&agent, "").await.unwrap();
    assert!(!workspace.join("AGENTS.md").exists());
    let preview = output.lock().unwrap()[0].clone();
    let token = preview
        .split("`/init apply ")
        .nth(1)
        .and_then(|suffix| suffix.split('`').next())
        .expect("preview must contain a copyable apply token");

    command
        .execute(&agent, &format!("apply {token}"))
        .await
        .unwrap();
    let document = std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap();
    assert!(document.contains(heycode_init::MANAGED_START));
    assert!(document.contains("cargo test --workspace"));
    assert!(output.lock().unwrap()[1].contains("Created AGENTS.md"));
    let session = agent
        .session()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(
        heycode_session::derive_messages(session.events()).is_empty(),
        "/init is a human command and must not enter model history"
    );

    world.shutdown();
}
