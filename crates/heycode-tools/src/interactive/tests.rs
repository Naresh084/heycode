#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]
use super::*;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

fn world(root: &Path, config: Option<BrowserConfig>) -> Context {
    world_with_speech(root, config, None)
}
fn world_with_speech(
    root: &Path,
    config: Option<BrowserConfig>,
    speech: Option<SpeechCommandConfig>,
) -> Context {
    heycode_core::compose(&[
        heycode_exec::local_execution_plugin(
            heycode_exec::LocalShellConfig::platform(root.to_path_buf(), Duration::from_secs(30))
                .unwrap(),
        ),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        crate::tools_plugin(crate::ToolsConfig {
            cwd: root.to_path_buf(),
            web_enabled: false,
            ..Default::default()
        }),
        interactive_tools_plugin_with_speech(config, speech),
    ])
    .unwrap()
}
async fn call(
    world: &Context,
    root: &Path,
    name: &str,
    args: Value,
) -> anyhow::Result<crate::ToolOutcome> {
    crate::execute_tool(
        &world.get::<ToolRegistry>(crate::SERVICE_TOOLS).unwrap(),
        &world
            .get::<heycode_core::Waterfall<crate::PreToolDecision>>(crate::SEAM_PRE_TOOL)
            .unwrap(),
        crate::ToolCallInput {
            name: name.into(),
            args,
        },
        &ToolCtx::default().with_cwd(root.to_path_buf()),
    )
    .await
}
fn sample() -> Value {
    json!({"nbformat":4,"nbformat_minor":5,"metadata":{"kernelspec":{"name":"python3"},"custom":{"keep":42}},"future_extension":{"keep":true},"cells":[{"id":"one","cell_type":"code","metadata":{"tags":["keep"],"custom":7},"source":["print('old')\n"],"execution_count":9,"outputs":[{"output_type":"stream","name":"stdout","text":["old\n"]}]},{"id":"two","cell_type":"markdown","metadata":{},"source":"# Title","attachments":{"image.png":{"image/png":"AA=="}}}]})
}
#[tokio::test]
async fn notebook_metadata_conflicts_insert_delete_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("n.ipynb");
    let original = sample();
    std::fs::write(&file, original.to_string()).unwrap();
    let world = world(root, None);
    let read = call(&world, root, "notebook_read", json!({"path":"n.ipynb"}))
        .await
        .unwrap()
        .value;
    assert_eq!(read["cells"][0]["id"], "one");
    let edited=call(&world,root,"notebook_edit",json!({"path":"n.ipynb","action":"replace","expected_revision":read["revision"],"cell_index":0,"cell_id":"one","source":"print('new')\n"})).await.unwrap().value;
    let disk: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(disk["metadata"], original["metadata"]);
    assert_eq!(disk["future_extension"], original["future_extension"]);
    assert_eq!(disk["cells"][1], original["cells"][1]);
    assert_eq!(
        disk["cells"][0]["metadata"],
        original["cells"][0]["metadata"]
    );
    assert_eq!(disk["cells"][0]["source"], json!(["print('new')\n"]));
    assert_eq!(disk["cells"][0]["outputs"], json!([]));
    assert!(disk["cells"][0]["execution_count"].is_null());
    let before = std::fs::read(&file).unwrap();
    assert!(call(&world,root,"notebook_edit",json!({"path":"n.ipynb","action":"delete","expected_revision":read["revision"],"cell_index":0})).await.unwrap_err().to_string().contains("changed"));
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(call(&world,root,"notebook_edit",json!({"path":"n.ipynb","action":"delete","expected_revision":edited["revision"],"cell_index":0,"cell_id":"two"})).await.unwrap_err().to_string().contains("conflict"));
    let inserted=call(&world,root,"notebook_edit",json!({"path":"n.ipynb","action":"insert","expected_revision":edited["revision"],"cell_index":2,"cell_type":"code","source":"1+1"})).await.unwrap().value;
    assert_eq!(inserted["cells"], 3);
    call(&world,root,"notebook_edit",json!({"path":"n.ipynb","action":"delete","expected_revision":inserted["revision"],"cell_index":2})).await.unwrap();
    let cx = ToolCtx::default().with_cwd(root.to_path_buf());
    cx.cancellation.cancel();
    let tool = world
        .get::<ToolRegistry>(crate::SERVICE_TOOLS)
        .unwrap()
        .get("notebook_edit")
        .unwrap();
    assert!(tool.run(json!({"path":"n.ipynb","action":"delete","cell_index":0,"expected_revision":"ignored"}),&cx).await.is_err());
    assert_eq!(std::fs::read(&file).unwrap(), before);
}
#[tokio::test]
async fn notebook_invalid_cells_and_external_changes_fail_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("n.ipynb");
    let world = world(dir.path(), None);
    let mut invalid = sample();
    invalid["cells"][1]["id"] = json!("one");
    std::fs::write(&file, invalid.to_string()).unwrap();
    assert!(
        call(
            &world,
            dir.path(),
            "notebook_read",
            json!({"path":"n.ipynb"})
        )
        .await
        .is_err()
    );
    std::fs::write(&file, sample().to_string()).unwrap();
    let read = call(
        &world,
        dir.path(),
        "notebook_read",
        json!({"path":"n.ipynb"}),
    )
    .await
    .unwrap()
    .value;
    let mut changed = sample();
    changed["metadata"]["outside_edit"] = json!(true);
    let bytes = changed.to_string();
    std::fs::write(&file, &bytes).unwrap();
    assert!(call(&world,dir.path(),"notebook_edit",json!({"path":"n.ipynb","action":"replace","cell_index":0,"expected_revision":read["revision"],"source":"bad"})).await.is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), bytes);
    assert!(
        call(
            &world,
            dir.path(),
            "notebook_read",
            json!({"path":"../outside.ipynb"})
        )
        .await
        .is_err()
    );
}
#[tokio::test]
async fn artifacts_are_local_bounded_revision_checked_and_retired() {
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), None);
    let file = dir.path().join("report.html");
    std::fs::write(&file, "<h1>Local report</h1>").unwrap();
    let registered = call(
        &world,
        dir.path(),
        "artifact",
        json!({"action":"register","path":"report.html"}),
    )
    .await
    .unwrap()
    .value;
    let preview = call(
        &world,
        dir.path(),
        "artifact",
        json!({"action":"preview","id":registered["id"]}),
    )
    .await
    .unwrap();
    assert!(preview.rich_result.is_none());
    assert_eq!(preview.value["text"], "<h1>Local report</h1>");
    std::fs::write(&file, "Changed").unwrap();
    assert!(
        call(
            &world,
            dir.path(),
            "artifact",
            json!({"action":"preview","id":registered["id"]})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("changed")
    );
    call(
        &world,
        dir.path(),
        "artifact",
        json!({"action":"remove","id":registered["id"]}),
    )
    .await
    .unwrap();
    assert!(file.exists());
    let tool = world
        .get::<ToolRegistry>(crate::SERVICE_TOOLS)
        .unwrap()
        .get("artifact")
        .unwrap();
    world.shutdown();
    assert!(
        tool.run(json!({"action":"list"}), &ToolCtx::default())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn unavailable_browser_is_actionable_and_all_tools_pass_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), None);
    let status = call(&world, dir.path(), "browser", json!({"action":"status"}))
        .await
        .unwrap()
        .value;
    assert_eq!(status["installation_configured"], false);
    assert!(
        status["setup"]
            .as_str()
            .unwrap()
            .contains("HEYCODE_BROWSER_MODULE")
    );
    assert!(
        call(
            &world,
            dir.path(),
            "browser",
            json!({"action":"open","url":"https://example.com"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Playwright")
    );
    struct Deny;
    #[async_trait::async_trait]
    impl heycode_core::Layer<crate::PreToolDecision> for Deny {
        async fn handle(
            &self,
            input: &mut crate::PreToolDecision,
            _next: heycode_core::Next<'_, crate::PreToolDecision>,
        ) -> anyhow::Result<()> {
            input.verdict = crate::Verdict::Deny {
                reason: "fixture guard".into(),
            };
            Ok(())
        }
    }
    let mut seam = heycode_core::Waterfall::new();
    seam.push(Deny);
    for name in [
        "browser",
        "artifact",
        "notebook_read",
        "notebook_edit",
        "computer",
        "transcribe_audio",
    ] {
        let outcome = crate::execute_tool(
            &world.get::<ToolRegistry>(crate::SERVICE_TOOLS).unwrap(),
            &seam,
            crate::ToolCallInput {
                name: name.into(),
                args: json!({}),
            },
            &ToolCtx::default().with_cwd(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
        assert_eq!(outcome.denied_reason.as_deref(), Some("fixture guard"));
    }
}

struct Fixture {
    url: String,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    slow: Arc<tokio::sync::Notify>,
}
impl Fixture {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = CancellationToken::new();
        let token = stop.clone();
        let slow = Arc::new(tokio::sync::Notify::new());
        let notify = slow.clone();
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {biased;()=token.cancelled()=>break,result=listener.accept()=>{let(mut socket,_)=result.unwrap();let token=token.clone();let notify=notify.clone();clients.spawn(async move {let mut bytes=[0u8;16384];let count=socket.read(&mut bytes).await.unwrap_or(0);let request=String::from_utf8_lossy(&bytes[..count]);if request.starts_with("GET /slow "){notify.notify_one();token.cancelled().await;return;}let body=if request.starts_with("GET /next "){ "<html><body><h1>Second page</h1></body></html>" }else{include_str!("fixture.html")};let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(response.as_bytes()).await;});}}
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Self {
            url,
            stop,
            task,
            slow,
        }
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
fn element(value: &Value, tag: &str) -> String {
    value["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["tag"] == tag)
        .unwrap()["ref"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
#[ignore = "requires explicit HEYCODE_BROWSER_* installation paths; real local Chromium fixture"]
async fn browser_real_navigation_click_text_screenshot_preview_and_close() {
    let config = BrowserConfig::from_environment().expect("Set explicit browser paths");
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut world = world(dir.path(), Some(config));
    let opened = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    let session = opened["session"].clone();
    assert!(opened["text"].as_str().unwrap().contains("Browser fixture"));
    assert!(
        opened["accessibility"]
            .as_str()
            .unwrap()
            .contains("textbox")
    );
    let typed = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"type","session":session,"element":element(&opened,"input"),"text":"Ada"}),
    )
    .await
    .unwrap()
    .value;
    let clicked = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"click","session":session,"element":element(&typed,"button")}),
    )
    .await
    .unwrap()
    .value;
    assert!(clicked["text"].as_str().unwrap().contains("Hello Ada"));
    let screenshot = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"screenshot","session":session,"path":"fixture.png"}),
    )
    .await
    .unwrap();
    assert!(screenshot.rich_result.is_none());
    assert_eq!(screenshot.value["width"], 1280);
    assert!(
        std::fs::read(dir.path().join("fixture.png"))
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    let rich = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"screenshot","session":session,"path":"vision.png","include_image":true}),
    )
    .await
    .unwrap();
    assert!(rich.rich_result.is_some());
    let next = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"navigate","session":session,"url":format!("{}next",fixture.url)}),
    )
    .await
    .unwrap()
    .value;
    assert!(next["text"].as_str().unwrap().contains("Second page"));
    std::fs::write(
        dir.path().join("preview.html"),
        format!("<html><body><h1>Generated preview</h1><button onclick=\"fetch('{}next').then(() => document.body.dataset.network='allowed').catch(() => document.body.dataset.network='blocked')\">Probe</button></body></html>",fixture.url),
    )
    .unwrap();
    let preview = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"preview","session":session,"path":"preview.html"}),
    )
    .await
    .unwrap()
    .value;
    assert!(
        preview["text"]
            .as_str()
            .unwrap()
            .contains("Generated preview")
    );
    let probe = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"click","session":session,"element":element(&preview,"button")}),
    )
    .await
    .unwrap()
    .value;
    assert_eq!(
        probe["http_requests"], 0,
        "preview stays offline on later input"
    );
    call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"close","session":session}),
    )
    .await
    .unwrap();
    assert!(
        call(
            &world,
            dir.path(),
            "browser",
            json!({"action":"inspect","session":session})
        )
        .await
        .is_err()
    );
    let held = world
        .get::<ToolRegistry>(crate::SERVICE_TOOLS)
        .unwrap()
        .get("browser")
        .unwrap();
    world.shutdown();
    assert!(
        held.run(json!({"action":"status"}), &ToolCtx::default())
            .await
            .is_err()
    );
    fixture.close().await;
}
#[tokio::test]
#[ignore = "requires explicit HEYCODE_BROWSER_* installation paths; real cancellation fixture"]
async fn browser_cancelled_navigation_closes_session_and_can_reopen() {
    let config = BrowserConfig::from_environment().expect("Set explicit browser paths");
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), Some(config));
    let opened = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    let browser = world
        .get::<ToolRegistry>(crate::SERVICE_TOOLS)
        .unwrap()
        .get("browser")
        .unwrap();
    let cancellation = CancellationToken::new();
    let cx = ToolCtx {
        cwd: dir.path().to_path_buf(),
        cancellation: cancellation.clone(),
    };
    let url = format!("{}slow", fixture.url);
    let session = opened["session"].clone();
    let task = tokio::spawn(async move {
        browser
            .run(
                json!({"action":"navigate","session":session,"url":url}),
                &cx,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(15), fixture.slow.notified())
        .await
        .unwrap();
    cancellation.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        call(
            &world,
            dir.path(),
            "browser",
            json!({"action":"inspect","session":opened["session"]})
        )
        .await
        .is_err()
    );
    let reopened = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    assert_ne!(reopened["session"], opened["session"]);
    call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"close","session":reopened["session"]}),
    )
    .await
    .unwrap();
    fixture.close().await;
}
#[tokio::test]
#[ignore = "requires explicit browser installation and public network; live policy-mediated canary"]
async fn browser_public_canary() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(
        dir.path(),
        Some(BrowserConfig::from_environment().expect("Set browser paths")),
    );
    let opened = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"open","url":"https://example.com/"}),
    )
    .await
    .unwrap()
    .value;
    assert!(opened["text"].as_str().unwrap().contains("Example Domain"));
    call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"close","session":opened["session"]}),
    )
    .await
    .unwrap();
    let fixture = Fixture::start().await;
    let local = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"open","url":fixture.url,"allow_local":true}),
    )
    .await
    .unwrap()
    .value;
    let public_link = local["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["text"] == "Public canary")
        .unwrap()["ref"]
        .clone();
    let public = call(
        &world,
        dir.path(),
        "browser",
        json!({"action":"click","session":local["session"],"element":public_link}),
    )
    .await
    .unwrap()
    .value;
    assert!(public["text"].as_str().unwrap().contains("Example Domain"));
    assert!(
        call(
            &world,
            dir.path(),
            "browser",
            json!({"action":"navigate","session":local["session"],"url":fixture.url})
        )
        .await
        .is_err(),
        "public link navigation revokes the local origin grant"
    );
    fixture.close().await;
}

fn wav() -> Vec<u8> {
    let data = vec![0u8; 320];
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}
#[cfg(unix)]
#[tokio::test]
async fn speech_adapter_validates_audio_bounds_output_and_cancels() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fixture.wav"), wav()).unwrap();
    let filesystem = Arc::new(crate::builtins::test_filesystem(dir.path()));
    let subprocess = Arc::new(heycode_exec::SubprocessService::local());
    let closed = CancellationToken::new();
    let cx = ToolCtx::default().with_cwd(dir.path().to_path_buf());
    let make = |command: &str| speech::Speech {
        config: Some(
            SpeechCommandConfig::new("/bin/sh".into(), vec!["-c".into(), command.into()]).unwrap(),
        ),
        filesystem: filesystem.clone(),
        subprocess: subprocess.clone(),
        closed: closed.clone(),
    };
    let args = json!({"action":"transcribe","path":"fixture.wav"});
    let output = make("cat >/dev/null; printf 'fixture transcript'")
        .run(args.clone(), &cx)
        .await
        .unwrap();
    assert_eq!(output["text"], "fixture transcript");
    assert_eq!(output["sample_rate_hz"], 16000);
    assert_eq!(output["microphone_recorded"], false);
    assert!(
        make("cat >/dev/null; printf 'bad'; exit 1")
            .run(args.clone(), &cx)
            .await
            .unwrap_err()
            .message
            .contains("unsuccessfully")
    );
    assert!(
        make("cat >/dev/null; head -c 40000 /dev/zero")
            .run(args.clone(), &cx)
            .await
            .unwrap_err()
            .message
            .contains("32 KiB")
    );
    assert!(
        make("cat >/dev/null; printf transcript; exec 1>&-; exec sleep 30")
            .run(args.clone(), &cx)
            .await
            .unwrap_err()
            .message
            .contains("did not exit")
    );
    std::fs::write(dir.path().join("bad.wav"), b"RIFF-not-real").unwrap();
    assert!(
        make("touch launched; printf 'bad'")
            .run(json!({"action":"transcribe","path":"bad.wav"}), &cx)
            .await
            .is_err()
    );
    assert!(!dir.path().join("launched").exists());
    let adapter = make("cat >/dev/null; touch waiting; exec sleep 30");
    let cancellation = CancellationToken::new();
    let cx = ToolCtx {
        cwd: dir.path().to_path_buf(),
        cancellation: cancellation.clone(),
    };
    let task = tokio::spawn(async move { adapter.run(args, &cx).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dir.path().join("waiting").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires configured real local recognizer and documented generated-speech WAV; no microphone"]
async fn speech_real_generated_speech_canary() {
    let dir = tempfile::tempdir().unwrap();
    let source = std::env::var_os("HEYCODE_STT_CANARY_WAV")
        .expect("Generate the documented synthetic speech fixture and set HEYCODE_STT_CANARY_WAV");
    std::fs::copy(source, dir.path().join("generated.wav")).unwrap();
    let config = SpeechCommandConfig::from_environment()
        .unwrap()
        .expect("Configure the real installed recognizer with HEYCODE_STT_COMMAND");
    let world = world_with_speech(dir.path(), None, Some(config));
    let started = std::time::Instant::now();
    let output = call(
        &world,
        dir.path(),
        "transcribe_audio",
        json!({"action":"transcribe","path":"generated.wav"}),
    )
    .await
    .unwrap()
    .value;
    let text = output["text"].as_str().unwrap();
    let normalized = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    let reference="the quick brown fox jumps over the lazy dog please save the meeting notes in the local project folder".split_whitespace().collect::<Vec<_>>();
    let recognized = normalized.split_whitespace().collect::<Vec<_>>();
    let mut previous = (0..=recognized.len()).collect::<Vec<_>>();
    for (i, expected) in reference.iter().enumerate() {
        let mut row = vec![i + 1; recognized.len() + 1];
        for (j, actual) in recognized.iter().enumerate() {
            row[j + 1] = (row[j] + 1)
                .min(previous[j + 1] + 1)
                .min(previous[j] + usize::from(expected != actual));
        }
        previous = row;
    }
    let errors = previous[recognized.len()];
    println!(
        "Real local recognition: {} ms; word_errors={errors}/{}; transcript={text:?}",
        started.elapsed().as_millis(),
        reference.len()
    );
    // A small recognizer need not produce a perfect transcript. This acoustic smoke test
    // requires <=10% normalized word error, while reporting every measured error explicitly.
    assert!(
        errors * 10 <= reference.len(),
        "Generated-speech canary exceeds 10% word error"
    );
    assert_eq!(output["sample_rate_hz"], 16000);
    assert_eq!(output["microphone_recorded"], false);
    assert_eq!(output["inserted"], false);
    assert_eq!(output["source"], "local_stt_command");
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires Apple Command Line Tools; readiness only, no capture or app interaction"]
async fn macos_computer_readiness_without_capture() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), None);
    let status = call(&world, dir.path(), "computer", json!({"action":"status"}))
        .await
        .unwrap()
        .value;
    assert_eq!(status["platform"], "macos");
    assert!(status["accessibility"].is_boolean());
    assert!(status["screen_recording"].is_boolean());
    assert_eq!(status["microphone"], false);
    println!(
        "macOS readiness: accessibility={}, screen_recording={}",
        status["accessibility"], status["screen_recording"]
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires macOS permissions; creates and controls only an owned temporary fixture app"]
async fn macos_owned_app_capture_and_available_input() {
    let dir = tempfile::tempdir().unwrap();
    let world = world(dir.path(), None);
    let status = call(&world, dir.path(), "computer", json!({"action":"status"}))
        .await
        .unwrap()
        .value;
    assert_eq!(
        status["accessibility"], true,
        "Grant Accessibility to run this explicitly optional test"
    );
    assert_eq!(
        status["screen_recording"], true,
        "Grant Screen Recording to run this explicitly optional test"
    );
    let contents = dir.path().join("HeycodeComputerFixture.app/Contents");
    let macos = contents.join("MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(contents.join("Info.plist"),r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.heycode.audit-computer-fixture</string><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleName</key><string>HeycodeComputerFixture</string><key>CFBundleVersion</key><string>1.0</string><key>NSPrincipalClass</key><string>NSApplication</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>"#).unwrap();
    let source = dir.path().join("fixture.swift");
    std::fs::write(&source, include_str!("computer-fixture.swift")).unwrap();
    let executable = macos.join("fixture");
    let subprocess = world
        .get::<heycode_exec::SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
        .unwrap();
    let compile = heycode_exec::ProcessSpec::new("/usr/bin/swiftc", dir.path())
        .unwrap()
        .with_args([
            source.as_os_str(),
            std::ffi::OsStr::new("-o"),
            executable.as_os_str(),
        ])
        .unwrap()
        .with_timeout(Some(Duration::from_secs(45)))
        .unwrap();
    let output = subprocess
        .output(compile, CancellationToken::new())
        .await
        .unwrap();
    assert!(output.exit().is_success(), "{}", output.stderr());
    let process = subprocess
        .spawn(
            heycode_exec::ProcessSpec::new(&executable, dir.path()).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let capture_args = json!({"action":"screenshot","bundle_id":"org.heycode.audit-computer-fixture","path":"native.png"});
    let mut capture = None;
    for _ in 0..5 {
        match call(&world, dir.path(), "computer", capture_args.clone()).await {
            Ok(result) => {
                capture = Some(result.value);
                break;
            }
            Err(error) => println!("fixture capture waiting: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let capture = capture.expect("owned fixture screenshot");
    assert_eq!(
        capture["width"], 420,
        "select the titled fixture, not its hidden utility window"
    );
    assert!(
        std::fs::read(dir.path().join("native.png"))
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    let inspect_args = json!({"action":"inspect","bundle_id":"org.heycode.audit-computer-fixture"});
    let inspected = call(&world, dir.path(), "computer", inspect_args.clone())
        .await
        .ok()
        .map(|output| output.value)
        .filter(|value| {
            value["nodes"]
                .as_array()
                .is_some_and(|nodes| nodes.iter().any(|node| node["role"] == "AXTextField"))
        });
    if inspected.is_none() {
        println!(
            "Owned AppKit fixture does not export AX windows on this host; exercising the app-only pixel/input fallback."
        );
        let revision = &capture["revision"];
        if capture["on_screen"] != true {
            assert_ne!(
                std::env::var("HEYCODE_REQUIRE_NATIVE_INPUT").as_deref(),
                Ok("1"),
                "Strict GUI canary requires the owned app to be visible on screen"
            );
            for action in ["click", "type", "key"] {
                assert!(call(&world,dir.path(),"computer",json!({"action":action,"bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"text":"Ada","key":"backspace","x":capture["frame"]["x"],"y":capture["frame"]["y"]})).await.unwrap_err().to_string().contains("off screen"));
            }
            println!(
                "Owned app is off screen: capture transport and input refusal verified; visible-window input and AX actions remain unverified on this host."
            );
            process.kill().await.unwrap();
            return;
        }

        call(&world,dir.path(),"computer",json!({"action":"type","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"text":"Ada"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(dir.path().join("fixture-state.txt"))
                    .unwrap_or_default()
                    .contains("text=Ada")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let x = capture["frame"]["x"].as_f64().unwrap() + 80.0;
        let y = capture["frame"]["y"].as_f64().unwrap()
            + capture["frame"]["height"].as_f64().unwrap()
            - 110.0;
        call(&world,dir.path(),"computer",json!({"action":"click","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"x":x,"y":y})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(dir.path().join("fixture-state.txt"))
                    .unwrap_or_default()
                    .contains("label=Hello Ada")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        call(&world,dir.path(),"computer",json!({"action":"key","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"key":"backspace"})).await.unwrap();
        call(&world,dir.path(),"computer",json!({"action":"type","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"text":"Grace"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(dir.path().join("fixture-state.txt"))
                    .unwrap_or_default()
                    .contains("text=AdGrace")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        assert!(call(&world,dir.path(),"computer",json!({"action":"click","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":revision,"x":-99999,"y":-99999})).await.is_err());
        process.kill().await.unwrap();
        return;
    }
    let inspected = inspected.expect("owned fixture accessibility tree");
    let text_ref = inspected["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["role"] == "AXTextField")
        .unwrap()["ref"]
        .clone();
    call(&world,dir.path(),"computer",json!({"action":"type","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":inspected["revision"],"element":text_ref,"text":"Ada"})).await.unwrap();
    let updated = call(&world, dir.path(), "computer", inspect_args.clone())
        .await
        .unwrap()
        .value;
    assert!(
        updated["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["value"] == "Ada")
    );
    let button = updated["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["title"] == "Fixture greet")
        .unwrap()["ref"]
        .clone();
    call(&world,dir.path(),"computer",json!({"action":"click","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":updated["revision"],"element":button})).await.unwrap();
    let greeted = call(&world, dir.path(), "computer", inspect_args.clone())
        .await
        .unwrap()
        .value;
    assert!(
        greeted["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["value"] == "Hello Ada")
    );
    call(&world,dir.path(),"computer",json!({"action":"key","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":greeted["revision"],"key":"tab"})).await.unwrap();
    let screenshot=call(&world,dir.path(),"computer",json!({"action":"screenshot","bundle_id":"org.heycode.audit-computer-fixture","path":"native-second.png"})).await.unwrap();
    assert!(screenshot.rich_result.is_none());
    assert!(
        std::fs::read(dir.path().join("native.png"))
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert!(call(&world,dir.path(),"computer",json!({"action":"click","bundle_id":"org.heycode.audit-computer-fixture","expected_revision":"0".repeat(64),"element":"0"})).await.unwrap_err().to_string().contains("changed"));
    process.kill().await.unwrap();
}

#[path = "workspace-tests.rs"]
mod workspace;
