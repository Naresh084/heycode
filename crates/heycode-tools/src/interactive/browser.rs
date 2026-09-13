//! Owned isolated browser with a fixed stdio adapter and policy-mediated HTTP.
use super::{file_error, image_output};
use crate::builtins::{arg_str, resolve_path};
use crate::{Tool, ToolCtx, ToolError, ToolOutput};
use base64::Engine as _;
use heycode_core::ToolSpec;
use heycode_exec::{
    FileSystemService, ManagedProcess, ProcessInput, ProcessOutputChunk, ProcessOutputReader,
    ProcessSpec, SubprocessService,
};
use heycode_web::{BrowserHttpRequest, BrowserLocalOrigin, WebRegistry};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const DRIVER: &str = include_str!("browser-driver.cjs");
const MAX_FRAME: usize = 12 * 1024 * 1024;
const SETUP: &str = "Browser needs Node.js, Playwright >=1.51 and Chromium/Chrome. Install Playwright in a trusted directory (npm install --prefix <directory> playwright); set HEYCODE_BROWSER_NODE, HEYCODE_BROWSER_MODULE (absolute playwright package directory), HEYCODE_BROWSER_EXECUTABLE (absolute Chrome/Chromium executable), then restart heycode. Never use a personal profile.";

/// Host-owned optional installation configuration. Never accepts model-supplied executable code.
#[derive(Clone, Debug)]
pub struct BrowserConfig {
    /// Absolute Node executable.
    pub node: PathBuf,
    /// Absolute installed Playwright package directory.
    pub module: PathBuf,
    /// Absolute Chromium or Google Chrome executable; profile reuse is not supported.
    pub executable: PathBuf,
}
impl BrowserConfig {
    /// Read explicit process-level installation selections. Absence means capability unavailable.
    #[must_use]
    pub fn from_environment() -> Option<Self> {
        Some(Self {
            node: std::env::var_os("HEYCODE_BROWSER_NODE")?.into(),
            module: std::env::var_os("HEYCODE_BROWSER_MODULE")?.into(),
            executable: std::env::var_os("HEYCODE_BROWSER_EXECUTABLE")?.into(),
        })
    }
    fn valid(&self) -> bool {
        self.node.is_absolute()
            && self.node.is_file()
            && self.module.is_absolute()
            && self.module.join("package.json").is_file()
            && self.executable.is_absolute()
            && self.executable.is_file()
    }
}

struct Connection {
    process: ManagedProcess,
    input: ProcessInput,
    output: ProcessOutputReader,
    buffer: Vec<u8>,
    id: u64,
    local: Option<BrowserLocalOrigin>,
}
impl Connection {
    async fn stop(mut self) -> Result<(), ToolError> {
        // Normal close/cancellation gives Playwright a bounded chance to remove its temporary
        // browser profile. The process owner still enforces hard tree settlement afterwards.
        let shutdown = async {
            self.input
                .write_line("{\"kind\":\"shutdown\"}")
                .await
                .map_err(process_error)?;
            loop {
                let frame = self.frame(CancellationToken::new()).await?;
                if frame["kind"] == "shutdown" {
                    return Ok::<(), ToolError>(());
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(3), shutdown).await;
        let Self {
            process,
            input,
            output,
            ..
        } = self;
        drop(output);
        drop(input);
        process.kill().await.map_err(process_error)?;
        Ok(())
    }
    async fn frame(&mut self, cancellation: CancellationToken) -> Result<Value, ToolError> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let value = serde_json::from_slice(&self.buffer[..end])
                    .map_err(|_| ToolError::new("Browser protocol invalid; session closed."))?;
                self.buffer.drain(..=end);
                return Ok(value);
            }
            match self
                .output
                .read_chunk(cancellation.clone())
                .await
                .map_err(process_error)?
            {
                ProcessOutputChunk::Data(bytes) => {
                    if self.buffer.len() + bytes.len() > MAX_FRAME {
                        return Err(ToolError::new(
                            "Browser output exceeds 12 MiB; session closed.",
                        ));
                    }
                    self.buffer.extend(bytes);
                }
                ProcessOutputChunk::Eof => {
                    return Err(ToolError::new(format!("Browser exited. {SETUP}")));
                }
            }
        }
    }
    async fn call(
        &mut self,
        request: Value,
        web: &WebRegistry,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError> {
        self.input
            .write_line(&request.to_string())
            .await
            .map_err(process_error)?;
        let mut requests = 0usize;
        let mut bytes = 0usize;
        let mut blocked = 0usize;
        loop {
            let frame = self.frame(cancellation.clone()).await?;
            match frame["kind"].as_str() {
                Some("result") => {
                    if let Some(error) = frame["error"].as_str() {
                        return Err(ToolError::new(match error {
                            "stale_element" => "Browser element is stale; inspect again.",
                            "timeout" => {
                                "Browser action timed out; session closed. Open a new session."
                            }
                            "browser_sandbox_failed" => {
                                "Chromium could not initialize its sandbox under the supplied process policy. macOS Seatbelt cannot nest Chromium's sandbox. Session closed without changing either policy; use a compatible host-provided browser executor."
                            }
                            _ => {
                                "Browser action failed; session closed. Check installation, URL policy and element state."
                            }
                        }));
                    }
                    let mut value = frame["value"].clone();
                    value["http_requests"] = json!(requests);
                    value["http_blocked"] = json!(blocked);
                    return Ok(value);
                }
                Some("http") => {
                    requests += 1;
                    if requests > 128 {
                        return Err(ToolError::new(
                            "Browser request budget exceeded (128 per operation); session closed.",
                        ));
                    }
                    let id = frame["id"]
                        .as_u64()
                        .ok_or_else(|| ToolError::new("Invalid browser request id."))?;
                    // A link/script/redirect navigation revokes local authority before its new
                    // document can fetch subresources. Explicit navigate is not the only crossing.
                    if frame["main_navigation"] == true
                        && self.local.as_ref().is_some_and(|local| {
                            url::Url::parse(frame["url"].as_str().unwrap_or("")).is_ok_and(
                                |target| {
                                    url::Url::parse(local.as_str())
                                        .is_ok_and(|origin| origin.origin() != target.origin())
                                },
                            )
                        })
                    {
                        self.local = None;
                    }
                    let request = parse_http(frame)?;
                    let result = web
                        .browser_request(request, self.local.as_ref(), cancellation.clone())
                        .await;
                    let reply = match result {
                        Ok(response) => {
                            bytes += response.body.len();
                            if bytes > 32 * 1024 * 1024 {
                                return Err(ToolError::new(
                                    "Browser response budget exceeded (32 MiB per operation); session closed.",
                                ));
                            }
                            let mut headers = serde_json::Map::<String, Value>::new();
                            for (key, value) in response.headers {
                                if let Some(old) = headers.get(&key).and_then(Value::as_str) {
                                    let separator = if key == "set-cookie" { "\n" } else { ", " };
                                    headers.insert(key, json!(format!("{old}{separator}{value}")));
                                } else {
                                    headers.insert(key, json!(value));
                                }
                            }
                            json!({"kind":"http_result","id":id,"status":response.status,"headers":headers,"body":base64::engine::general_purpose::STANDARD.encode(&response.body)})
                        }
                        Err(_) => {
                            blocked += 1;
                            json!({"kind":"http_result","id":id,"error":true})
                        }
                    };
                    self.input
                        .write_line(&reply.to_string())
                        .await
                        .map_err(process_error)?;
                }
                _ => return Err(ToolError::new("Browser protocol invalid; session closed.")),
            }
        }
    }
}
fn parse_http(frame: Value) -> Result<BrowserHttpRequest, ToolError> {
    let headers = frame["headers"]
        .as_object()
        .ok_or_else(|| ToolError::new("Invalid browser headers."))?
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|v| (k.clone(), v.to_owned()))
                .ok_or_else(|| ToolError::new("Invalid browser header."))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(arg_str(&frame, "body")?)
        .map_err(|_| ToolError::new("Invalid browser body."))?;
    Ok(BrowserHttpRequest {
        url: arg_str(&frame, "url")?.into(),
        method: arg_str(&frame, "method")?.into(),
        headers,
        body,
    })
}
fn process_error(_: heycode_exec::ProcessError) -> ToolError {
    ToolError::new(
        "Browser process failed or was cancelled; session closed. Check browser setup and active sandbox policy.",
    )
}

pub(super) struct Browser {
    config: Option<BrowserConfig>,
    filesystem: Arc<FileSystemService>,
    subprocess: Arc<SubprocessService>,
    web: Arc<WebRegistry>,
    state: Mutex<Option<Connection>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    closed: CancellationToken,
}
impl Browser {
    pub(super) fn new(
        config: Option<BrowserConfig>,
        filesystem: Arc<FileSystemService>,
        subprocess: Arc<SubprocessService>,
        web: Arc<WebRegistry>,
        closed: CancellationToken,
    ) -> Self {
        Self {
            config,
            filesystem,
            subprocess,
            web,
            state: Mutex::new(None),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            closed,
        }
    }
    async fn execute(&self, mut args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        if self.closed.is_cancelled() || cx.cancellation.is_cancelled() {
            return Err(ToolError::new(
                "Browser operation cancelled or service stopped.",
            ));
        }
        let action = arg_str(&args, "action")?.to_owned();
        if action == "status" {
            return Ok(ToolOutput::plain(
                json!({"installation_configured":self.config.as_ref().is_some_and(BrowserConfig::valid),"readiness":"configured paths only; open verifies launch","setup":SETUP,"controls":["open","navigate","inspect","click","type","screenshot","preview","close"],"text_models":true,"screenshots_require_vision_only_when_include_image":true,"network":"policy-mediated HTTP(S), explicit numeric loopback origin for local apps; WebSockets, downloads, personal profiles unavailable","native_computer_use":"Use computer status for macOS permission readiness","voice_dictation":"Use transcribe_audio status for optional local STT; microphone recording unavailable"}),
            ));
        }
        if !matches!(
            action.as_str(),
            "open" | "navigate" | "inspect" | "click" | "type" | "screenshot" | "preview" | "close"
        ) {
            return Err(ToolError::new("Unknown browser action."));
        }
        if args.to_string().len() > 512 * 1024 {
            return Err(ToolError::new("Browser arguments exceed 512 KiB."));
        }
        if matches!(action.as_str(), "open" | "navigate") {
            let url = url::Url::parse(arg_str(&args, "url")?)
                .map_err(|_| ToolError::new("Browser URL must be HTTP(S)."))?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.as_str().len() > 8192
            {
                return Err(ToolError::new(
                    "Browser URL must be bounded HTTP(S) without userinfo.",
                ));
            }
        }
        if action == "type" && arg_str(&args, "text")?.len() > 16384 {
            return Err(ToolError::new("Browser typed text exceeds 16 KiB."));
        }
        if action == "preview" {
            let path = resolve_path(&self.filesystem, cx, arg_str(&args, "path")?)?;
            let read = self
                .filesystem
                .read(
                    heycode_exec::ReadFileSpec::new(path, 2 * 1024 * 1024).map_err(file_error)?,
                    cx.cancellation.clone(),
                )
                .await
                .map_err(file_error)?;
            if read.truncated() {
                return Err(ToolError::new("HTML preview exceeds 2 MiB."));
            }
            args["html"] = json!(
                std::str::from_utf8(read.bytes())
                    .map_err(|_| ToolError::new("HTML preview must be UTF-8."))?
            );
        }
        let mut state = tokio::select! {biased;()=cx.cancellation.cancelled()=>return Err(ToolError::new("Browser call cancelled while waiting.")),()=self.closed.cancelled()=>return Err(ToolError::new("Browser service stopped.")),state=self.state.lock()=>state};
        if self.closed.is_cancelled() {
            return Err(ToolError::new("Browser service stopped."));
        }
        if action == "open" {
            if state.is_some() {
                return Err(ToolError::new(
                    "A browser is already open; close its session first.",
                ));
            }
            let config = self
                .config
                .as_ref()
                .filter(|c| c.valid())
                .ok_or_else(|| ToolError::new(SETUP))?;
            let url = url::Url::parse(arg_str(&args, "url")?)
                .map_err(|_| ToolError::new("Invalid browser URL."))?;
            let local = if args["allow_local"] == true {
                Some(BrowserLocalOrigin::new(&format!("{}/",url.origin().ascii_serialization())).map_err(|_|ToolError::new("allow_local requires a numeric loopback HTTP application URL with an explicit unprivileged port."))?)
            } else {
                None
            };
            let cwd = resolve_path(&self.filesystem, cx, ".")?;
            // Validate write authority before starting a helper that creates a private profile.
            self.filesystem
                .create_dir_all(cwd.clone(), cx.cancellation.clone())
                .await
                .map_err(file_error)?;
            let spec = ProcessSpec::new(&config.node, cwd.as_path())
                .and_then(|s| {
                    s.with_args([
                        std::ffi::OsString::from("-e"),
                        DRIVER.into(),
                        config.module.as_os_str().to_owned(),
                        config.executable.as_os_str().to_owned(),
                    ])
                })
                .map_err(process_error)?
                .with_interactive_stdio();
            let raw = tokio::select! {biased;()=cx.cancellation.cancelled()=>return Err(ToolError::new("Browser open cancelled.")),result=self.subprocess.spawn_interactive_raw(spec,self.closed.child_token())=>result.map_err(process_error)?};
            let (process, input, output) = raw.into_raw_parts();
            *state = Some(Connection {
                process,
                input,
                output,
                buffer: Vec::new(),
                id: self
                    .next_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                local,
            });
        } else {
            let id = args["session"]
                .as_u64()
                .ok_or_else(|| ToolError::new("Pass session from browser open."))?;
            if state.as_ref().is_none_or(|s| s.id != id) {
                return Err(ToolError::new("Unknown browser session."));
            }
        }
        let mut connection = state
            .take()
            .ok_or_else(|| ToolError::new("Browser is not open."))?;
        if action == "navigate" {
            let url = url::Url::parse(arg_str(&args, "url")?)
                .map_err(|_| ToolError::new("Invalid browser URL."))?;
            if connection.local.as_ref().is_some_and(|local| {
                url::Url::parse(local.as_str()).is_ok_and(|local| local.origin() != url.origin())
            }) {
                connection.local = None;
            }
        }
        if action == "close" {
            connection.stop().await?;
            return Ok(ToolOutput::plain(json!({"closed":true})));
        }
        let operation = async {
            if action == "open" {
                connection
                    .call(
                        json!({"action":"launch"}),
                        &self.web,
                        cx.cancellation.clone(),
                    )
                    .await?;
                args["action"] = json!("navigate");
            }
            connection
                .call(args.clone(), &self.web, cx.cancellation.clone())
                .await
        };
        let result = tokio::select! {biased;()=self.closed.cancelled()=>Err(ToolError::new("Browser service stopped.")),()=cx.cancellation.cancelled()=>Err(ToolError::new("Browser action cancelled; session closed.")),result=tokio::time::timeout(Duration::from_secs(45),operation)=>result.unwrap_or_else(|_|Err(ToolError::new("Browser action deadline exceeded; session closed.")))};
        let mut value = match result {
            Ok(value) => value,
            Err(error) => {
                connection.stop().await?;
                return Err(error);
            }
        };
        if action != "screenshot" {
            bound_snapshot(&mut value);
        }
        value["session"] = json!(connection.id);
        *state = Some(connection);
        if action == "screenshot" {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(arg_str(&value, "png")?)
                .map_err(|_| ToolError::new("Invalid browser screenshot."))?;
            if bytes.len() > 8 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(ToolError::new("Invalid or oversized browser PNG."));
            }
            value.as_object_mut().map(|object| object.remove("png"));
            let path = resolve_path(&self.filesystem, cx, arg_str(&args, "path")?)?;
            self.filesystem
                .write(
                    heycode_exec::WriteFileSpec::new(path.clone(), &bytes).map_err(file_error)?,
                    cx.cancellation.clone(),
                )
                .await
                .map_err(file_error)?;
            value["path"] = json!(path.as_path());
            value["bytes"] = json!(bytes.len());
            value["image_input_requires_vision"] = json!(true);
            if args["include_image"] == true {
                return image_output(value, bytes);
            }
        }
        Ok(ToolOutput::plain(value))
    }
}

fn bound_snapshot(value: &mut Value) {
    for key in ["text", "accessibility"] {
        if let Some(text) = value[key].as_str()
            && text.len() > 16 * 1024
        {
            let text = super::prefix(text, 16 * 1024).to_owned();
            value[key] = json!(text);
            value["truncated"] = json!(true);
        }
    }
    while value.to_string().len() > 64 * 1024 {
        if let Some(elements) = value["elements"].as_array_mut()
            && elements.pop().is_some()
        {
            value["truncated"] = json!(true);
            continue;
        }
        for key in ["text", "accessibility"] {
            if let Some(text) = value[key].as_str() {
                value[key] = json!(super::prefix(text, text.len() / 2).to_owned());
            }
        }
        value["truncated"] = json!(true);
    }
}
#[async_trait::async_trait]
impl Tool for Browser {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus {
            configured: Some(self.config.as_ref().is_some_and(BrowserConfig::valid)),
            detail: "Status remains available. Opening a browser requires configured installation paths; launch is verified at invocation. Use browser status for setup.".into(),
        }
    }
    fn rebind_workspace(
        &self,
        filesystem: &FileSystemService,
        shell: &heycode_exec::ShellService,
    ) -> Option<Arc<dyn Tool>> {
        Some(match shell.subprocess() {
            Some(subprocess) => Arc::new(Self {
                config: self.config.clone(),
                filesystem: Arc::new(filesystem.clone()),
                subprocess: Arc::new(subprocess),
                web: self.web.clone(),
                state: Mutex::new(None),
                // Only the ID allocator is shared, so a parent/sibling handle cannot
                // accidentally name a different live session in this fresh scope.
                next_id: self.next_id.clone(),
                closed: self.closed.child_token(),
            }),
            None => super::workspace_unavailable(self),
        })
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec{name:"browser".into(),description:"Provider-independent isolated browser. status reports setup. open(url) returns session; allow_local=true explicitly grants only that numeric loopback origin. navigate/inspect return bounded text, accessibility and element refs. click/type require a fresh element ref. screenshot writes a PNG path; include_image=true explicitly requires vision. preview renders local HTML without external requests. close kills the owned process tree. No personal profiles, arbitrary JS, file uploads or WebSockets.".into(),parameters:json!({"type":"object","properties":{"action":{"enum":["status","open","navigate","inspect","click","type","screenshot","preview","close"]},"session":{"type":"integer","minimum":1},"url":{"type":"string"},"allow_local":{"type":"boolean"},"element":{"type":"string"},"text":{"type":"string","maxLength":16384},"path":{"type":"string"},"include_image":{"type":"boolean"}},"required":["action"],"additionalProperties":false})}
    }
    fn untrusted_content(&self) -> Option<heycode_core::UntrustedContentBoundary> {
        Some(heycode_core::UntrustedContentBoundary::web())
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        let (value, _, _) = self.execute(args, cx).await?.into_parts();
        Ok(value)
    }
    async fn run_output(&self, args: Value, cx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        self.execute(args, cx).await
    }
}
