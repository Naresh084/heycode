//! ACP end-to-end over the real `heycode acp` binary in offline fake mode.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};

fn bin_path() -> std::path::PathBuf {
    let exe = env!("CARGO_BIN_EXE_heycode");
    let path = std::path::PathBuf::from(exe);
    warm_once(&path);
    path
}

/// Execute the freshly built binary once, off the clock, before any test times
/// an interaction with it.
///
/// A newly created executable is scanned on its first execution — measured at
/// 11-23 seconds on the development machine, against 5ms for the same file a
/// second time. Every timeout in this file is smaller than that, so without
/// this the suite measures the host's scanner rather than heycode and fails for a
/// reason unrelated to the code under test. Runs once per test binary; the
/// result is ignored because only the exec itself matters.
fn warm_once(program: &std::path::Path) {
    static WARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARMED.get_or_init(|| {
        let _ = std::process::Command::new(program)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

struct Server {
    _home: tempfile::TempDir,
    child: Child,
    reader: BufReader<tokio::process::ChildStdout>,
    stdin: tokio::process::ChildStdin,
    next_id: u64,
}

impl Server {
    async fn spawn() -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(bin_path());
        cmd.args([
            "acp",
            "--restricted-workspace",
            "--set",
            "compaction.context_window=128000",
        ])
        .env("HEYCODE_FAKE", "1")
        .env("HEYCODE_HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn heycode acp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            _home: home,
            child,
            reader: BufReader::new(stdout),
            stdin,
            next_id: 1,
        }
    }

    async fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let frame = serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        self.stdin
            .write_all(frame.to_string().as_bytes())
            .await
            .unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
        self.stdin.flush().await.unwrap();

        loop {
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(20),
                self.reader.read_line(&mut line),
            )
            .await
            .expect("timeout reading ACP line")
            .expect("read line");
            let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            if value.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return value;
            }
            // notifications are consumed by dedicated readers in each test
        }
    }
}

#[tokio::test]
async fn initialize_session_prompt_round_trip_with_streaming() {
    let mut server = Server::spawn().await;

    let init = server
        .request(
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
        )
        .await;
    assert_eq!(
        init["result"]["protocolVersion"].as_i64(),
        Some(1),
        "{init}"
    );
    assert!(init["result"]["authMethods"].is_array());
    assert_eq!(
        init["result"]["agentCapabilities"]["promptCapabilities"]["image"].as_bool(),
        Some(true)
    );
    assert_eq!(
        init["result"]["agentCapabilities"]["promptCapabilities"]["embeddedContext"].as_bool(),
        Some(true)
    );

    let new = server
        .request(
            "session/new",
            serde_json::json!({
                "cwd":std::env::current_dir().unwrap(),
                "mcpServers":[],
            }),
        )
        .await;
    let session_id = new["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no sessionId: {new}"))
        .to_owned();

    // Prompt: single reader collects updates + the matching response.
    let id = server.next_id;
    server.next_id += 1;
    let frame = serde_json::json!({
        "jsonrpc":"2.0","id":id,
        "method":"session/prompt",
        "params":{"sessionId":session_id,"prompt":[
            {"type":"text","text":"say the thing"},
            {"type":"resource","resource":{
                "uri":"file:///virtual/context.txt",
                "mimeType":"text/plain",
                "text":"embedded context"
            }}
        ]}
    });
    server
        .stdin
        .write_all(frame.to_string().as_bytes())
        .await
        .unwrap();
    server.stdin.write_all(b"\n").await.unwrap();
    server.stdin.flush().await.unwrap();

    let mut chunks: Vec<String> = Vec::new();
    let mut update_types: Vec<String> = Vec::new();
    let mut saw_resource = false;
    let mut usage = None;
    let mut stop: Option<String> = None;
    for _ in 0..500 {
        let mut line = String::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            server.reader.read_line(&mut line),
        )
        .await
        .expect("timeout awaiting ACP line")
        .expect("read");
        assert!(
            read > 0,
            "`heycode acp` closed stdout before answering; chunks so far: {chunks:?}"
        );
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("method").and_then(|m| m.as_str()) == Some("session/update") {
            if let Some(kind) = value["params"]["update"]["sessionUpdate"].as_str() {
                update_types.push(kind.to_owned());
            }
            if let Some(text) = value["params"]["update"]["content"]["text"].as_str() {
                chunks.push(text.to_owned());
            }
            saw_resource |= value["params"]["update"]["content"]["type"] == "resource";
            if value["params"]["update"]["sessionUpdate"] == "usage_update" {
                usage = value["params"]["update"]["used"].as_u64();
            }
        } else if value.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
            stop = value["result"]["stopReason"].as_str().map(str::to_owned);
            break;
        }
    }

    assert_eq!(stop.as_deref(), Some("end_turn"));
    let joined = chunks.join("");
    assert!(
        joined.contains("FAKE-REPLY"),
        "streamed chunks missing fake text: {joined}"
    );
    assert!(update_types.contains(&"user_message_chunk".to_owned()));
    assert!(update_types.contains(&"agent_message_chunk".to_owned()));
    assert!(update_types.contains(&"usage_update".to_owned()));
    assert!(
        saw_resource,
        "embedded resource was not echoed: {update_types:?}"
    );
    assert!(
        usage.is_some_and(|used| used > 12),
        "context usage must include the complete request, not just fake billing tokens: {usage:?}"
    );

    server.child.kill().await.ok();
}

#[tokio::test]
async fn ask_mode_rejects_an_unknown_session_without_hanging() {
    let _home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(bin_path());
    cmd.args([
        "acp",
        "--restricted-workspace",
        "--set",
        "compaction.context_window=128000",
    ])
    .env("HEYCODE_FAKE", "1")
    .env("HEYCODE_ACP_APPROVAL", "ask")
    .env("HEYCODE_HOME", _home.path())
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    // initialize + session/new (ids 1,2), consuming replies.
    for (id, method, params) in [
        (
            1u64,
            "initialize",
            serde_json::json!({"protocolVersion": 1}),
        ),
        (2u64, "session/new", serde_json::json!({"cwd": "."})),
    ] {
        stdin
            .write_all(
                serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                    .to_string()
                    .as_bytes(),
            )
            .await
            .unwrap();
        stdin.write_all(b"\n").await.unwrap();
        stdin.flush().await.unwrap();
        loop {
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                reader.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
                && v.get("id").and_then(|i| i.as_u64()) == Some(id)
            {
                break;
            }
        }
    }

    // The binary fixture has no tool-call script. The full permission loop is
    // exercised by the in-process duplex fixture in `heycode_cli::acp`; this cell
    // retains only the ask-mode dispatch/error boundary.
    stdin
        .write_all(
            serde_json::json!({
                "jsonrpc":"2.0","id":3,"method":"session/prompt",
                "params":{"sessionId":"__pending__","prompt":"hi"}
            })
            .to_string()
            .as_bytes(),
        )
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
    // Unknown session error arrives (proves dispatch works with ask mode on).
    loop {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
            && v.get("id").and_then(|i| i.as_u64()) == Some(3)
        {
            assert!(v.get("error").is_some(), "{v}");
            break;
        }
    }
    child.kill().await.ok();
}
