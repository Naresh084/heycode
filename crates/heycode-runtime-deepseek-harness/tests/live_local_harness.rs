//! Explicitly gated local DeepSeek Harness SDK composition canary.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use heycode_exec::{SandboxMode, SandboxService, local_subprocess_plugin, sandbox_service_plugin};
use heycode_runtime::{
    AgentRuntimeRegistry, RuntimeEventKind, RuntimeInput, RuntimeStart, runtime_registry_plugin,
};
use heycode_runtime_deepseek_harness::{
    DEEPSEEK_HARNESS_RUNTIME_ID, DeepSeekHarnessRuntimeConfig, deepseek_harness_runtime_plugin,
};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

struct MockModelServer {
    address: String,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl MockModelServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_requests = Arc::clone(&requests);
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(45);
            while !worker_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _peer)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0_u8; 8192];
                        let mut expected = None;
                        loop {
                            let read = socket.read(&mut buffer).unwrap_or(0);
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&buffer[..read]);
                            if expected.is_none()
                                && let Some(headers_end) = find_bytes(&request, b"\r\n\r\n")
                            {
                                let headers = String::from_utf8_lossy(&request[..headers_end]);
                                let content_length = headers.lines().find_map(|line| {
                                    line.split_once(':').and_then(|(name, value)| {
                                        name.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                });
                                expected = content_length.map(|length| headers_end + 4 + length);
                            }
                            if expected.is_some_and(|length| request.len() >= length) {
                                break;
                            }
                        }
                        worker_requests.fetch_add(1, Ordering::SeqCst);
                        let body = concat!(
                            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{\"content\":\"local dsh runtime\"}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
                            "data: [DONE]\n\n",
                        );
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body,
                        );
                        socket.write_all(response.as_bytes()).unwrap();
                        socket.flush().unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address: format!("http://{address}"),
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn stop(mut self) -> usize {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockModelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn assert_sha256(path: &std::path::Path, expected: &str) {
    assert_eq!(expected.len(), 64);
    assert!(expected.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let mut file = File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(actual, expected.to_ascii_lowercase());
}

#[tokio::test]
async fn local_harness_sdk_delegation_and_lifecycle_are_explicitly_gated() {
    if std::env::var("HEYCODE_DSH_E2E").ok().as_deref() != Some("1") {
        return;
    }
    let node = PathBuf::from(
        std::env::var_os("HEYCODE_DSH_NODE")
            .expect("HEYCODE_DSH_NODE is required for the explicit canary"),
    );
    let server = PathBuf::from(
        std::env::var_os("HEYCODE_DSH_SERVER")
            .expect("HEYCODE_DSH_SERVER is required for the explicit canary"),
    );
    let cordis = PathBuf::from(
        std::env::var_os("HEYCODE_DSH_CORDIS")
            .expect("HEYCODE_DSH_CORDIS is required for the explicit canary"),
    );
    let safe_path = std::env::var_os("HEYCODE_DSH_SAFE_PATH")
        .expect("HEYCODE_DSH_SAFE_PATH is required for the explicit canary");
    assert!(node.is_file() && server.is_file() && cordis.is_file());
    assert_sha256(
        &node,
        &std::env::var("HEYCODE_DSH_NODE_SHA256")
            .expect("HEYCODE_DSH_NODE_SHA256 is required for the explicit canary"),
    );
    assert_sha256(
        &server,
        &std::env::var("HEYCODE_DSH_SERVER_SHA256")
            .expect("HEYCODE_DSH_SERVER_SHA256 is required for the explicit canary"),
    );
    assert_sha256(
        &cordis,
        &std::env::var("HEYCODE_DSH_CORDIS_SHA256")
            .expect("HEYCODE_DSH_CORDIS_SHA256 is required for the explicit canary"),
    );

    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let home = workspace.join("home");
    let sessions = workspace.join("sessions");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&sessions).unwrap();
    let model = MockModelServer::start();
    let environment = vec![
        (OsString::from("PATH"), safe_path),
        (OsString::from("HOME"), home.as_os_str().to_os_string()),
        (
            OsString::from("DEEPSEEK_API_KEY"),
            OsString::from("heycode-local-canary-not-a-credential"),
        ),
        (
            OsString::from("DEEPSEEK_BASE_URL"),
            OsString::from(&model.address),
        ),
        (
            OsString::from("DSH_CWD"),
            workspace.as_os_str().to_os_string(),
        ),
        (
            OsString::from("DSH_SESSION_ROOT"),
            sessions.as_os_str().to_os_string(),
        ),
        (OsString::from("DSH_SNAPSHOT"), OsString::from("1")),
    ];
    let config = DeepSeekHarnessRuntimeConfig::new()
        .with_program(node.as_os_str())
        .unwrap()
        .with_args(vec![
            server.clone().into_os_string(),
            cordis.clone().into_os_string(),
        ])
        .unwrap()
        .with_artifacts(vec![server, cordis])
        .unwrap()
        .with_environment(environment)
        .unwrap();
    let plugins = vec![
        runtime_registry_plugin(),
        sandbox_service_plugin(SandboxService::new(SandboxMode::Off, &workspace, None).unwrap()),
        local_subprocess_plugin(),
        deepseek_harness_runtime_plugin(config),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let runtimes = context
        .get::<AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap();
    let runtime = runtimes.get(DEEPSEEK_HARNESS_RUNTIME_ID).unwrap().unwrap();
    let request = RuntimeStart::new(
        heycode_core::SessionId::from_raw("heycode-local-harness"),
        &workspace,
    )
    .unwrap()
    .with_model("deepseek-v4-pro")
    .unwrap();
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        runtime.start(request, CancellationToken::new()),
    )
    .await
    .expect("local Harness initialization timed out")
    .unwrap();
    let mut events = session.subscribe();
    tokio::time::timeout(
        Duration::from_secs(30),
        session.send(
            RuntimeInput::new("answer with the fixture text").unwrap(),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("local Harness turn timed out")
    .unwrap();
    let mut final_text = None;
    while let Some(event) = events.next().await {
        let event = event.unwrap();
        match event.kind() {
            RuntimeEventKind::FinalMessage { text } => final_text = Some(text.clone()),
            RuntimeEventKind::TurnFinished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(final_text.as_deref(), Some("local dsh runtime"));
    session.close(CancellationToken::new()).await.unwrap();
    context.shutdown();
    assert!(model.stop() >= 1);
}
