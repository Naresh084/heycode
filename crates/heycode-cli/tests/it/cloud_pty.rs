//! Production-binary pseudo-terminal proof for managed-cloud onboarding.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsString;
use std::time::Duration;

use heycode_exec::{
    MAX_TERMINAL_READ_BYTES, ProcessSpec, SubprocessService, TerminalId, TerminalOwner,
    TerminalService, TerminalSize, TerminalSpec,
};

async fn read_until(
    service: &TerminalService,
    owner: &TerminalOwner,
    id: &TerminalId,
    needle: &str,
) -> String {
    let mut seen = Vec::new();
    for _ in 0..500 {
        let read = service
            .read(owner, id, MAX_TERMINAL_READ_BYTES)
            .await
            .unwrap();
        seen.extend_from_slice(read.bytes());
        let rendered = String::from_utf8_lossy(&seen);
        if rendered.contains(needle) {
            return rendered.into_owned();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "terminal did not render {needle:?}: {}",
        String::from_utf8_lossy(&seen)
    );
}

#[tokio::test]
async fn production_tui_reaches_bedrock_region_form_through_a_real_pty() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let heycode_home = root.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&heycode_home).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_heycode"))
        .canonicalize()
        .unwrap();
    let path = std::env::var_os("PATH").expect("test runner must provide PATH");
    let spec = ProcessSpec::new(binary, &workspace)
        .unwrap()
        .with_args([OsString::from("--trust-workspace")])
        .unwrap()
        .with_environment([
            (
                OsString::from("HEYCODE_HOME"),
                heycode_home.into_os_string(),
            ),
            (OsString::from("PATH"), path),
        ])
        .unwrap()
        .with_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .with_interactive_stdio();
    let service = TerminalService::new(SubprocessService::local());
    let owner = TerminalOwner::new("cloud-onboarding-pty").unwrap();
    let id = service
        .open(
            &owner,
            TerminalSpec::new(spec)
                .unwrap()
                .with_size(TerminalSize::new(100, 32).unwrap()),
        )
        .await
        .unwrap();

    let welcome = read_until(&service, &owner, &id, "Select a provider").await;
    assert!(welcome.contains("Welcome to heycode"), "{welcome:?}");

    service.write(&owner, &id, b"\x1b[B\x1b[B\r").await.unwrap();
    read_until(&service, &owner, &id, "Select a provider").await;
    service.write(&owner, &id, b"bedrock").await.unwrap();
    read_until(&service, &owner, &id, "Amazon Bedrock").await;
    service.write(&owner, &id, b"\r").await.unwrap();
    let form = read_until(&service, &owner, &id, "AWS region").await;
    assert!(form.contains("us-east-1"), "{form:?}");
    assert!(
        form.contains("region-bound Amazon Bedrock API key"),
        "{form:?}"
    );

    service
        .write(&owner, &id, b"\x1b[200~ap-southeast-2\x1b[201~")
        .await
        .unwrap();
    let entered = read_until(&service, &owner, &id, "ap-southeast-2").await;
    assert!(entered.contains("ap-southeast-2"), "{entered:?}");

    service.kill(&owner, &id).await.unwrap();
}

#[tokio::test]
async fn production_tui_reaches_vertex_coordinates_without_a_masked_secret_action() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let heycode_home = root.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&heycode_home).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_heycode"))
        .canonicalize()
        .unwrap();
    let path = std::env::var_os("PATH").expect("test runner must provide PATH");
    let spec = ProcessSpec::new(binary, &workspace)
        .unwrap()
        .with_args([OsString::from("--trust-workspace")])
        .unwrap()
        .with_environment([
            (
                OsString::from("HEYCODE_HOME"),
                heycode_home.into_os_string(),
            ),
            (OsString::from("PATH"), path),
        ])
        .unwrap()
        .with_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .with_interactive_stdio();
    let service = TerminalService::new(SubprocessService::local());
    let owner = TerminalOwner::new("vertex-onboarding-pty").unwrap();
    let id = service
        .open(
            &owner,
            TerminalSpec::new(spec)
                .unwrap()
                .with_size(TerminalSize::new(110, 34).unwrap()),
        )
        .await
        .unwrap();

    read_until(&service, &owner, &id, "Select a provider").await;
    service.write(&owner, &id, b"\x1b[B\x1b[B\r").await.unwrap();
    read_until(&service, &owner, &id, "Select a provider").await;
    service.write(&owner, &id, b"vertex").await.unwrap();
    read_until(&service, &owner, &id, "Vertex Gemini").await;
    service.write(&owner, &id, b"\r").await.unwrap();
    let project = read_until(&service, &owner, &id, "Google Cloud project").await;
    assert!(
        project.contains("Application Default Credentials"),
        "{project:?}"
    );
    service
        .write(&owner, &id, b"\x1b[200~vertex-fixture\x1b[201~\r")
        .await
        .unwrap();
    let location = read_until(&service, &owner, &id, "Vertex AI location").await;
    assert!(location.contains("Vertex AI location"), "{location:?}");
    assert!(location.contains("before saving"), "{location:?}");
    assert!(!location.contains("masked credential"), "{location:?}");
    service
        .write(&owner, &id, b"\x1b[200~us-central1\x1b[201~")
        .await
        .unwrap();
    let ready = read_until(&service, &owner, &id, "us-central1").await;
    assert!(ready.contains("us-central1"), "{ready:?}");

    service.kill(&owner, &id).await.unwrap();
}

#[tokio::test]
async fn production_tui_reaches_azure_resource_and_deployment_before_secret_or_network() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let heycode_home = root.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&heycode_home).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_heycode"))
        .canonicalize()
        .unwrap();
    let path = std::env::var_os("PATH").expect("test runner must provide PATH");
    let spec = ProcessSpec::new(binary, &workspace)
        .unwrap()
        .with_args([OsString::from("--trust-workspace")])
        .unwrap()
        .with_environment([
            (
                OsString::from("HEYCODE_HOME"),
                heycode_home.into_os_string(),
            ),
            (OsString::from("PATH"), path),
        ])
        .unwrap()
        .with_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .with_interactive_stdio();
    let service = TerminalService::new(SubprocessService::local());
    let owner = TerminalOwner::new("azure-onboarding-pty").unwrap();
    let id = service
        .open(
            &owner,
            TerminalSpec::new(spec)
                .unwrap()
                .with_size(TerminalSize::new(110, 34).unwrap()),
        )
        .await
        .unwrap();

    read_until(&service, &owner, &id, "Select a provider").await;
    service.write(&owner, &id, b"\x1b[B\x1b[B\r").await.unwrap();
    read_until(&service, &owner, &id, "Select a provider").await;
    service.write(&owner, &id, b"azure").await.unwrap();
    read_until(&service, &owner, &id, "Microsoft Azure OpenAI").await;
    service.write(&owner, &id, b"\r").await.unwrap();
    let resource = read_until(&service, &owner, &id, "Azure OpenAI resource").await;
    assert!(resource.contains(".openai.azure.com"), "{resource:?}");
    service
        .write(&owner, &id, b"\x1b[200~team-agent\x1b[201~\r")
        .await
        .unwrap();
    let deployment = read_until(&service, &owner, &id, "Azure OpenAI deployment").await;
    assert!(
        deployment.contains("Responses model field"),
        "{deployment:?}"
    );
    service
        .write(&owner, &id, b"\x1b[200~prod-gpt\x1b[201~")
        .await
        .unwrap();
    // Do not confirm the final field: that is the boundary which would begin
    // the masked-key/readiness path. This PTY case proves the production form
    // without reading a real credential or making a network request.
    let ready = read_until(&service, &owner, &id, "prod-gpt").await;
    assert!(ready.contains("prod-gpt"), "{ready:?}");

    service.kill(&owner, &id).await.unwrap();
}

#[tokio::test]
async fn production_tui_reaches_custom_chat_server_url_and_optional_key_choice_without_io() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let heycode_home = root.path().join("home");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&heycode_home).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_heycode"))
        .canonicalize()
        .unwrap();
    let path = std::env::var_os("PATH").expect("test runner must provide PATH");
    let spec = ProcessSpec::new(binary, &workspace)
        .unwrap()
        .with_args([OsString::from("--trust-workspace")])
        .unwrap()
        .with_environment([
            (
                OsString::from("HEYCODE_HOME"),
                heycode_home.into_os_string(),
            ),
            (OsString::from("PATH"), path),
        ])
        .unwrap()
        .with_timeout(Some(Duration::from_secs(30)))
        .unwrap()
        .with_interactive_stdio();
    let service = TerminalService::new(SubprocessService::local());
    let owner = TerminalOwner::new("custom-local-onboarding-pty").unwrap();
    let id = service
        .open(
            &owner,
            TerminalSpec::new(spec)
                .unwrap()
                .with_size(TerminalSize::new(112, 34).unwrap()),
        )
        .await
        .unwrap();

    read_until(&service, &owner, &id, "Use a local model").await;
    service.write(&owner, &id, b"\x1b[B\r").await.unwrap();
    let servers = read_until(&service, &owner, &id, "Custom OpenAI-compatible").await;
    assert!(servers.contains("LM Studio"), "{servers:?}");
    assert!(servers.contains("Ollama"), "{servers:?}");
    service.write(&owner, &id, b"\r").await.unwrap();
    let form = read_until(&service, &owner, &id, "Enter the server address").await;
    assert!(form.contains("Find models"), "{form:?}");
    assert!(form.contains("optional masked bearer key"), "{form:?}");
    assert!(form.contains("POST /chat/completions"), "{form:?}");
    service
        .write(&owner, &id, b"\x1b[200~http://localhost:8000/v1\x1b[201~")
        .await
        .unwrap();
    // Do not confirm: that boundary starts canonical `/models` discovery.
    let entered = read_until(&service, &owner, &id, "http://localhost:8000/v1").await;
    assert!(entered.contains("http://localhost:8000/v1"), "{entered:?}");

    service.kill(&owner, &id).await.unwrap();
}
