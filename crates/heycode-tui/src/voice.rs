//! Human-owned local dictation. No model call, microphone access at startup, or auto-submit.
use crate::app::{AppState, Item};
use heycode_exec::{ProcessOutputChunk, ProcessSpec, SubprocessService};
use heycode_tools::interactive::{LocalSpeech, SpeechCommandConfig};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const MAX_AUDIO: usize = 12 * 1024 * 1024;
const RECORD_TIME: Duration = Duration::from_secs(60);
const READY_TIME: Duration = Duration::from_secs(45);
const FINISH_TIME: Duration = Duration::from_secs(8);
const CAPTURE_HELP: &str = "Set HEYCODE_VOICE_CAPTURE_COMMAND to a JSON array with an absolute executable and literal arguments. The helper emits HEYCODE_VOICE_READY followed by a newline, records until stdin EOF, then emits only a PCM WAV. Other platforms need an explicitly configured capture helper.";

#[derive(Clone)]
struct CaptureConfig {
    program: PathBuf,
    args: Vec<String>,
    mac_native: bool,
    scratch: Arc<std::sync::Mutex<Option<tempfile::TempDir>>>,
}
impl CaptureConfig {
    fn from_environment() -> anyhow::Result<Self> {
        if let Some(value) = std::env::var_os("HEYCODE_VOICE_CAPTURE_COMMAND") {
            let mut args: Vec<String> = serde_json::from_str(
                value
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!(CAPTURE_HELP))?,
            )?;
            anyhow::ensure!(!args.is_empty(), CAPTURE_HELP);
            let program = PathBuf::from(args.remove(0));
            anyhow::ensure!(
                program.is_absolute()
                    && args.len() <= 64
                    && args.iter().all(|arg| !arg.contains('\0'))
                    && args.iter().map(String::len).sum::<usize>() <= 16384,
                CAPTURE_HELP
            );
            return Ok(Self {
                program,
                args,
                mac_native: false,
                scratch: Arc::default(),
            });
        }
        #[cfg(target_os = "macos")]
        return Ok(Self {
            program: "/usr/bin/swift".into(),
            args: vec!["-e".into(), include_str!("voice_capture.swift").into()],
            mac_native: true,
            scratch: Arc::default(),
        });
        #[cfg(not(target_os = "macos"))]
        anyhow::bail!(CAPTURE_HELP)
    }
    fn spec(&self, cwd: &std::path::Path, action: &str) -> anyhow::Result<ProcessSpec> {
        let mut args = self.args.clone();
        let mut environment = Vec::new();
        if self.mac_native {
            // Swift's default /var/folders cache is outside WorkspaceWrite.
            // Private compiler scratch uses the profile's existing /private/tmp
            // grant, with no additional sandbox rights. It contains no audio.
            let mut scratch = self
                .scratch
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if scratch.is_none() {
                *scratch = Some(
                    tempfile::Builder::new()
                        .prefix("heycode-voice-")
                        .tempdir_in("/private/tmp")?,
                );
            }
            let path = scratch
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Voice compiler scratch is unavailable"))?
                .path();
            args.splice(
                0..0,
                [
                    "-module-cache-path".into(),
                    path.join("modules").to_string_lossy().into_owned(),
                ],
            );
            environment.push(("HEYCODE_VOICE_ACTION", action.to_owned()));
            environment.push(("TMPDIR", path.to_string_lossy().into_owned()));
        }
        let spec = ProcessSpec::new(&self.program, cwd)?
            .with_args(args)?
            .with_environment(environment)?;
        Ok(spec
            .with_timeout(Some(READY_TIME + RECORD_TIME + FINISH_TIME))?
            .with_output_limit_bytes(8192)?)
    }
}

struct VoiceBackend {
    capture: CaptureConfig,
    speech: LocalSpeech,
    subprocess: SubprocessService,
    cwd: PathBuf,
}
impl VoiceBackend {
    fn from_environment(
        subprocess: SubprocessService,
        cwd: PathBuf,
        closed: CancellationToken,
    ) -> anyhow::Result<Self> {
        let speech = LocalSpeech::new(
            SpeechCommandConfig::from_environment()?,
            subprocess.clone(),
            closed,
        );
        Ok(Self {
            capture: CaptureConfig::from_environment()?,
            speech,
            subprocess,
            cwd,
        })
    }
    async fn status(&self, cancellation: CancellationToken) -> anyhow::Result<String> {
        let permission = if self.capture.mac_native {
            let output = self
                .subprocess
                .output(
                    self.capture
                        .spec(&self.cwd, "status")?
                        .with_timeout(Some(READY_TIME))?,
                    cancellation,
                )
                .await?;
            anyhow::ensure!(
                output.exit().is_success() && !output.truncated(),
                "Microphone status helper failed; check Swift installation and sandbox policy. {}",
                output
                    .stderr()
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .take(1024)
                    .collect::<String>()
            );
            String::from_utf8(output.stdout().to_vec())?
                .trim()
                .to_owned()
        } else {
            "Microphone permission and capture readiness are checked by the configured helper on start.".into()
        };
        Ok(format!(
            "Voice: {}. Capture executable: {}. {permission}\n60-second recording limit. /voice start, stop, cancel; transcripts enter the draft and are never submitted.\n{}",
            self.speech.status(),
            if self.capture.program.is_file() {
                "installed"
            } else {
                "missing"
            },
            if self.capture.mac_native {
                "If microphone permission is denied/restricted, enable it for your terminal in System Settings > Privacy & Security > Microphone. Not-determined permission is requested only by /voice start."
            } else {
                CAPTURE_HELP
            }
        ))
    }
    async fn dictate(
        &self,
        stop: CancellationToken,
        cancellation: CancellationToken,
        events: &mpsc::UnboundedSender<VoiceEvent>,
        id: u64,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(self.speech.configured(), "{}", self.speech.status());
        let audio = self
            .capture_audio(stop, cancellation.clone(), events, id)
            .await?;
        let _ = events.send(VoiceEvent {
            id,
            kind: EventKind::Phase(Phase::Transcribing),
        });
        let transcript = self
            .speech
            .transcribe_pcm_wav(&audio, &self.cwd, Some(RECORD_TIME), cancellation)
            .await?;
        Ok(transcript.text)
    }
    async fn capture_audio(
        &self,
        stop: CancellationToken,
        cancellation: CancellationToken,
        events: &mpsc::UnboundedSender<VoiceEvent>,
        id: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let operation = cancellation.child_token();
        let _guard = operation.clone().drop_guard();
        let raw = self
            .subprocess
            .spawn_interactive_raw(
                self.capture
                    .spec(&self.cwd, "record")?
                    .with_interactive_stdio(),
                operation.clone(),
            )
            .await?;
        let (process, input, mut output) = raw.into_raw_parts();
        let mut input = Some(input);
        let work = async {
            let ready = async {
                let mut bytes = Vec::new();
                loop {
                    match output.read_chunk(cancellation.clone()).await? {
                        ProcessOutputChunk::Eof => anyhow::bail!(
                            "Capture stopped before readiness; check microphone permission, input device and sandbox policy."
                        ),
                        ProcessOutputChunk::Data(chunk) => bytes.extend(chunk),
                    }
                    if let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
                        anyhow::ensure!(
                            end <= 1024,
                            "Capture readiness message exceeds its limit."
                        );
                        anyhow::ensure!(
                            &bytes[..end] == b"HEYCODE_VOICE_READY",
                            "{}",
                            String::from_utf8_lossy(&bytes[..end])
                        );
                        let remaining = bytes.split_off(end + 1);
                        anyhow::ensure!(remaining.len() <= MAX_AUDIO, "Capture exceeds 12 MiB.");
                        return Ok::<_, anyhow::Error>(remaining);
                    }
                    anyhow::ensure!(
                        bytes.len() <= 1024,
                        "Capture did not emit its bounded readiness line."
                    );
                }
            };
            let mut audio = tokio::select! {
                biased;
                () = stop.cancelled() => anyhow::bail!("Recording stopped before capture was ready; no transcript inserted."),
                result = tokio::time::timeout(READY_TIME, ready) => result.map_err(|_| anyhow::anyhow!("Microphone readiness timed out; check permission and input device."))??,
            };
            let _ = events.send(VoiceEvent {
                id,
                kind: EventKind::Phase(Phase::Recording),
            });
            let deadline = tokio::time::sleep(RECORD_TIME);
            tokio::pin!(deadline);
            let mut stopping = false;
            let mut finish_at = None;
            loop {
                tokio::select! {
                    biased;
                    () = stop.cancelled(), if !stopping => {
                        stopping = true;
                        if let Some(input) = input.take() { input.finish().await?; }
                        finish_at = Some(tokio::time::Instant::now() + FINISH_TIME);
                        let _ = events.send(VoiceEvent { id, kind: EventKind::Phase(Phase::Stopping) });
                    }
                    () = &mut deadline, if !stopping => {
                        stopping = true;
                        if let Some(input) = input.take() { input.finish().await?; }
                        finish_at = Some(tokio::time::Instant::now() + FINISH_TIME);
                        let _ = events.send(VoiceEvent { id, kind: EventKind::Phase(Phase::Stopping) });
                    }
                    () = async { if let Some(at) = finish_at { tokio::time::sleep_until(at).await } else { std::future::pending().await } } => anyhow::bail!("Capture did not finish after stop."),
                    chunk = output.read_chunk(cancellation.clone()) => match chunk? {
                        ProcessOutputChunk::Eof => break,
                        ProcessOutputChunk::Data(bytes) => {
                            anyhow::ensure!(audio.len() + bytes.len() <= MAX_AUDIO, "Capture exceeds 12 MiB.");
                            audio.extend(bytes);
                        }
                    }
                }
            }
            anyhow::ensure!(!audio.is_empty(), "Capture returned no audio.");
            Ok(audio)
        };
        let result = tokio::select! { biased; () = cancellation.cancelled() => Err(anyhow::anyhow!("Dictation cancelled.")), result = work => result };
        drop(input);
        drop(output);
        if result.is_err() {
            operation.cancel();
        }
        let exit = tokio::time::timeout(FINISH_TIME, process.wait())
            .await
            .map_err(|_| {
                anyhow::anyhow!("Capture cleanup timed out; process ownership retired.")
            })?;
        let audio = result?;
        anyhow::ensure!(
            exit?.is_success(),
            "Capture failed; check microphone permission and device availability."
        );
        Ok(audio)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Starting,
    Recording,
    Stopping,
    Transcribing,
    Checking,
}
impl Phase {
    fn caption(self) -> &'static str {
        match self {
            Self::Starting => "Voice · starting microphone · /voice cancel",
            Self::Recording => "Voice · RECORDING (60s max) · /voice stop or cancel",
            Self::Stopping => "Voice · stopping capture · /voice cancel",
            Self::Transcribing => "Voice · transcribing locally · /voice cancel",
            Self::Checking => "Voice · checking setup (no recording)",
        }
    }
}
#[derive(Default)]
pub(crate) struct VoiceView {
    phase: Option<Phase>,
    pending: Option<String>,
}
impl VoiceView {
    pub(crate) fn caption(&self) -> Option<&'static str> {
        self.phase.map(Phase::caption).or_else(|| {
            self.pending
                .as_ref()
                .map(|_| "Voice · transcript ready · /voice insert or cancel")
        })
    }
}
pub(crate) struct VoiceEvent {
    id: u64,
    kind: EventKind,
}
enum EventKind {
    Phase(Phase),
    Transcript(anyhow::Result<String>),
    Status(anyhow::Result<String>),
}
struct Operation {
    _workspace: Option<heycode_agent::workspace_transition::WorkspaceActivityLease>,
    id: u64,
    draft: tui_textarea::TextArea<'static>,
    stop: CancellationToken,
    cancellation: CancellationToken,
    cancelled: bool,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
    }
}

pub(crate) struct VoiceController {
    workspace: Option<Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>>,
    owner: String,
    backend: Result<Arc<VoiceBackend>, String>,
    operation: Option<Operation>,
    next: u64,
    closed: CancellationToken,
    tx: mpsc::UnboundedSender<VoiceEvent>,
    pub(crate) rx: mpsc::UnboundedReceiver<VoiceEvent>,
}
impl VoiceController {
    pub(crate) fn new(owner: String, subprocess: SubprocessService, cwd: PathBuf) -> Self {
        let closed = CancellationToken::new();
        let backend = VoiceBackend::from_environment(subprocess, cwd, closed.clone())
            .map(Arc::new)
            .map_err(|e| e.to_string());
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            workspace: None,
            owner,
            backend,
            operation: None,
            next: 0,
            closed,
            tx,
            rx,
        }
    }
    fn owner_matches(&self, state: &AppState) -> bool {
        state
            .current_session_id()
            .is_none_or(|id| id.as_str() == self.owner)
            && !state.task_console.active
    }

    pub(crate) fn with_workspace(
        mut self,
        workspace: Option<Arc<heycode_agent::workspace_transition::WorkspaceTransitionService>>,
    ) -> Self {
        self.workspace = workspace;
        self
    }
    pub(crate) fn sync_owner(&mut self, state: &mut AppState) {
        if !self.owner_matches(state)
            && (self.operation.as_ref().is_some_and(|op| !op.cancelled)
                || state.voice.pending.is_some())
        {
            self.cancel(state);
            state.items.push(Item::Info(
                "Dictation cancelled because the composer owner changed.".into(),
            ));
        }
    }
    fn cancel(&mut self, state: &mut AppState) {
        if let Some(operation) = &mut self.operation {
            operation.cancelled = true;
            operation.cancellation.cancel();
        }
        state.voice = VoiceView::default();
        if self.operation.is_some() {
            state.voice.phase = Some(Phase::Stopping);
        }
    }
    pub(crate) fn command(&mut self, args: &str, state: &mut AppState) {
        self.sync_owner(state);
        let action = args.trim();
        if action == "cancel" {
            self.cancel(state);
            state
                .items
                .push(Item::Info("Cancelling dictation; draft preserved.".into()));
            return;
        }
        if !self.owner_matches(state) {
            state.items.push(Item::Error(
                "Return to the original session composer before using /voice.".into(),
            ));
            return;
        }
        if action == "stop" {
            if let Some(operation) = &self.operation {
                operation.stop.cancel();
            } else {
                state
                    .items
                    .push(Item::Info("No voice recording is running.".into()));
            }
            return;
        }
        if action == "insert" {
            if let Some(text) = state.voice.pending.take() {
                insert_transcript(state, &text);
            } else {
                state
                    .items
                    .push(Item::Info("No dictation transcript is waiting.".into()));
            }
            return;
        }
        if !matches!(action, "" | "status" | "start") {
            state.items.push(Item::Error(
                "Usage: /voice start|stop|cancel|status|insert".into(),
            ));
            return;
        }
        if self.operation.is_some() || state.voice.pending.is_some() {
            state.items.push(Item::Info(
                state
                    .voice
                    .caption()
                    .unwrap_or("Voice capture is stopping; wait for cleanup before starting again.")
                    .into(),
            ));
            return;
        }
        let backend = match &self.backend {
            Ok(backend) => backend.clone(),
            Err(error) => {
                state.items.push(Item::Error(error.clone()));
                return;
            }
        };
        let workspace = match self
            .workspace
            .as_ref()
            .map(|owner| owner.pin_activity())
            .transpose()
        {
            Ok(lease) => lease,
            Err(error) => {
                state.items.push(Item::Error(error.to_string()));
                return;
            }
        };
        self.next = self.next.wrapping_add(1);
        let id = self.next;
        let stop = CancellationToken::new();
        let cancellation = self.closed.child_token();
        let op_stop = stop.clone();
        let op_cancel = cancellation.clone();
        let tx = self.tx.clone();
        let recording = action == "start";
        state.voice.phase = Some(if recording {
            Phase::Starting
        } else {
            Phase::Checking
        });
        let task = tokio::spawn(async move {
            let kind = if recording {
                EventKind::Transcript(backend.dictate(op_stop, op_cancel, &tx, id).await)
            } else {
                EventKind::Status(backend.status(op_cancel).await)
            };
            let _ = tx.send(VoiceEvent { id, kind });
        });
        self.operation = Some(Operation {
            _workspace: workspace,
            id,
            draft: state.input.clone(),
            stop,
            cancellation,
            cancelled: false,
            task,
        });
    }
    pub(crate) fn event(&mut self, event: VoiceEvent, state: &mut AppState) {
        self.sync_owner(state);
        let Some(operation) = self.operation.as_ref().filter(|op| op.id == event.id) else {
            return;
        };
        if operation.cancelled {
            if !matches!(event.kind, EventKind::Phase(_)) {
                self.operation.take();
                state.voice.phase = None;
                state.items.push(Item::Info(
                    "Dictation stopped; no transcript inserted.".into(),
                ));
            }
            return;
        }
        match event.kind {
            EventKind::Phase(phase) => state.voice.phase = Some(phase),
            EventKind::Status(result) => {
                self.operation.take();
                state.voice.phase = None;
                state.items.push(match result {
                    Ok(text) => Item::Info(text),
                    Err(error) => Item::Error(error.to_string()),
                });
            }
            EventKind::Transcript(result) => {
                let unchanged = state.input.lines() == operation.draft.lines()
                    && state.input.cursor() == operation.draft.cursor();
                self.operation.take();
                state.voice.phase = None;
                match result {
                    Err(error) => state.items.push(Item::Error(error.to_string())),
                    Ok(text)
                        if text.len() > 32768
                            || text
                                .chars()
                                .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r')) =>
                    {
                        state.items.push(Item::Error(
                            "Dictation returned an invalid or oversized transcript.".into(),
                        ))
                    }
                    Ok(text) if unchanged => insert_transcript(state, &text),
                    Ok(text) => {
                        state.voice.pending = Some(text);
                        state.items.push(Item::Info("Dictation is ready. Your draft changed, so it was preserved. Use Ctrl+P → /voice insert to add the transcript, or /voice cancel.".into()));
                    }
                }
            }
        }
    }
    pub(crate) async fn shutdown(&mut self) {
        self.cancel_for_exit();
        if let Some(mut operation) = self.operation.take() {
            operation.cancellation.cancel();
            let _ = tokio::time::timeout(FINISH_TIME, &mut operation.task).await;
        }
    }
    pub(crate) fn cancel_for_exit(&self) {
        self.closed.cancel();
    }
}
impl Drop for VoiceController {
    fn drop(&mut self) {
        self.closed.cancel();
        self.operation.take();
    }
}

fn insert_transcript(state: &mut AppState, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        state
            .items
            .push(Item::Error("Dictation returned no text.".into()));
        return;
    }
    let (row, column) = state.input.cursor();
    let line = &state.input.lines()[row];
    let before = column.checked_sub(1).and_then(|col| line.chars().nth(col));
    let after = line.chars().nth(column);
    let leading = if before.is_some_and(|c| !c.is_whitespace()) {
        " "
    } else {
        ""
    };
    let trailing = if after.is_some_and(|c| !c.is_whitespace()) {
        " "
    } else {
        ""
    };
    state.input.insert_str(format!("{leading}{text}{trailing}"));
    state.items.push(Item::Info(
        "Dictation inserted into the draft. Review it, then press Enter when ready.".into(),
    ));
}

pub(crate) fn command()
-> Result<Arc<dyn heycode_agent::Command>, heycode_agent::CommandMetadataError> {
    struct VoiceCommand(heycode_agent::CommandDescriptor);
    #[async_trait::async_trait]
    impl heycode_agent::Command for VoiceCommand {
        fn descriptor(&self) -> &heycode_agent::CommandDescriptor {
            &self.0
        }
        async fn execute(&self, _: &heycode_agent::Agent, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("/voice requires the active human TUI composer.")
        }
    }
    Ok(Arc::new(VoiceCommand(
        heycode_agent::CommandDescriptor::new(
            "voice",
            "Dictate locally into your draft without submitting",
            vec![heycode_agent::CommandArgument::optional(
                "action",
                "start, stop, cancel, status, or insert",
            )?],
            heycode_agent::CommandTiming::Immediate,
            heycode_agent::CommandSource::from_plugin("tui")?,
        )?,
    )))
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout
    )]
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Mutex;

    fn wav() -> Vec<u8> {
        wav_frames(1600)
    }
    fn wav_frames(frames: u32) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend((36 + frames * 2).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(16000_u32.to_le_bytes());
        bytes.extend(32000_u32.to_le_bytes());
        bytes.extend(2_u16.to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend((frames * 2).to_le_bytes());
        bytes.resize(44 + frames as usize * 2, 0);
        bytes
    }
    struct Fixture {
        root: tempfile::TempDir,
        voice: VoiceController,
        state: AppState,
        sessions: Arc<heycode_session::SessionQueryService>,
    }
    fn fixture(capture_script: &str, stt_script: Option<&str>) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let sessions = Arc::new(heycode_session::SessionQueryService::local(
            root.path().join("sessions"),
        ));
        let metadata = heycode_session::SessionCreationMetadata::new(
            Some(root.path().to_path_buf()),
            Some("native".into()),
            heycode_session::SessionSource::Interactive,
        )
        .unwrap();
        let session = sessions
            .create(&heycode_session::SessionCreateRequest::new(
                metadata.clone(),
            ))
            .unwrap();
        let owner = session.id().to_string();
        let mut state = AppState::new("text-only-test", root.path().to_path_buf());
        state.set_session_service(sessions.clone(), Arc::new(Mutex::new(session)), metadata);
        let mut commands = heycode_agent::CommandRegistry::new();
        commands.register(command().unwrap()).unwrap();
        state.set_commands(Arc::new(commands));
        std::fs::write(root.path().join("input.wav"), wav()).unwrap();
        let subprocess = SubprocessService::local();
        let closed = CancellationToken::new();
        let speech = LocalSpeech::new(
            stt_script.map(|script| {
                SpeechCommandConfig::new("/bin/sh".into(), vec!["-c".into(), script.into()])
                    .unwrap()
            }),
            subprocess.clone(),
            closed.clone(),
        );
        let capture = CaptureConfig {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), capture_script.into()],
            mac_native: false,
            scratch: Arc::default(),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let voice = VoiceController {
            workspace: None,
            owner,
            backend: Ok(Arc::new(VoiceBackend {
                capture,
                speech,
                subprocess,
                cwd: root.path().to_path_buf(),
            })),
            operation: None,
            next: 0,
            closed,
            tx,
            rx,
        };
        Fixture {
            root,
            voice,
            state,
            sessions,
        }
    }
    const CAPTURE: &str =
        "echo $$ > capture.pid; printf 'HEYCODE_VOICE_READY\n'; cat >/dev/null; cat input.wav";
    const STT: &str = "cat > received.wav; printf 'dictated words'";

    fn submit(f: &mut Fixture, action: &str) {
        f.state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        f.state.input = tui_textarea::TextArea::from(vec![format!("/voice {action}")]);
        f.state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        let pending = f.state.pending_send.take().expect("real slash submission");
        let (name, args) = heycode_agent::parse_slash(&pending).unwrap();
        assert_eq!(name, "voice");
        f.voice.command(&args, &mut f.state);
    }
    async fn until(f: &mut Fixture, ready: impl Fn(&Fixture) -> bool) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !ready(f) {
                let event = f.voice.rx.recv().await.unwrap();
                f.voice.event(event, &mut f.state);
            }
        })
        .await
        .unwrap();
    }
    async fn settled(f: &mut Fixture) {
        until(f, |f| f.voice.operation.is_none()).await;
    }
    async fn assert_capture_exited(f: &Fixture) {
        let pid = std::fs::read_to_string(f.root.path().join("capture.pid")).unwrap();
        let result = SubprocessService::local()
            .output(
                ProcessSpec::new("/bin/kill", f.root.path())
                    .unwrap()
                    .with_args(["-0", pid.trim()])
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            !result.exit().is_success(),
            "capture process must be settled"
        );
    }

    #[tokio::test]
    async fn voice_commands_stop_transcribe_and_insert_at_preserved_cursor_without_submit() {
        let mut f = fixture(CAPTURE, Some(STT));
        f.state.input.insert_str("first last");
        f.state
            .input
            .move_cursor(tui_textarea::CursorMove::Jump(0, 5));
        f.state
            .apply(&heycode_agent::UiEvent::TurnStarted { turn: 1 });
        submit(&mut f, "start");
        assert_eq!(f.state.input.lines(), &["first last"]);
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        assert!(f.state.voice.caption().unwrap().contains("RECORDING"));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, &mut f.state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("RECORDING"));
        assert!(
            crate::app::accessibility::ScreenReaderSnapshot::from_state(&f.state)
                .as_text()
                .contains("RECORDING")
        );
        f.state.workflow_console.view = crate::workflow_console::WorkflowView::Workspace;
        terminal
            .draw(|frame| crate::render::draw(frame, &mut f.state))
            .unwrap();
        let expanded = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(expanded.contains("RECORDING"));
        assert!(
            crate::app::accessibility::ScreenReaderSnapshot::from_state(&f.state)
                .as_text()
                .contains("RECORDING")
        );
        f.state.workflow_console.view = crate::workflow_console::WorkflowView::Collapsed;
        submit(&mut f, "stop");
        settled(&mut f).await;
        assert_eq!(f.state.input.lines(), &["first dictated words last"]);
        assert!(
            f.state.has_active_turn(),
            "dictation must not cancel the model turn"
        );
        assert!(f.state.pending_send.is_none());
        assert_eq!(
            std::fs::read(f.root.path().join("received.wav")).unwrap(),
            wav()
        );
        assert_capture_exited(&f).await;
        f.voice.shutdown().await;
    }

    #[tokio::test]
    async fn voice_preserves_edited_draft_until_explicit_insert_and_never_submits() {
        let mut f = fixture(CAPTURE, Some(STT));
        f.state.input.insert_str("original");
        submit(&mut f, "start");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        f.state.input.insert_str(" edited");
        submit(&mut f, "stop");
        settled(&mut f).await;
        assert_eq!(f.state.input.lines(), &["original edited"]);
        assert!(f.state.voice.pending.is_some());
        submit(&mut f, "insert");
        assert_eq!(f.state.input.lines(), &["original edited dictated words"]);
        assert!(f.state.pending_send.is_none());
        assert!(f.state.voice.pending.is_none());
    }

    #[tokio::test]
    async fn voice_cancel_and_teardown_settle_owned_capture_without_transcription() {
        for teardown in [false, true] {
            let mut f = fixture(CAPTURE, Some(STT));
            f.state.input.insert_str("keep me");
            submit(&mut f, "start");
            until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
            if teardown {
                f.voice.shutdown().await;
            } else {
                submit(&mut f, "cancel");
                settled(&mut f).await;
            }
            assert_eq!(f.state.input.lines(), &["keep me"]);
            assert!(!f.root.path().join("received.wav").exists());
            assert_capture_exited(&f).await;
            assert!(f.state.pending_send.is_none());
        }
    }

    #[tokio::test]
    async fn voice_session_switch_cancels_and_late_result_cannot_edit_new_composer() {
        let mut f = fixture(CAPTURE, Some(STT));
        submit(&mut f, "start");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        let id = f.voice.operation.as_ref().unwrap().id;
        let metadata = heycode_session::SessionCreationMetadata::new(
            Some(f.root.path().to_path_buf()),
            Some("native".into()),
            heycode_session::SessionSource::Interactive,
        )
        .unwrap();
        let other = f
            .sessions
            .create(&heycode_session::SessionCreateRequest::new(
                metadata.clone(),
            ))
            .unwrap();
        f.state
            .set_session_service(f.sessions.clone(), Arc::new(Mutex::new(other)), metadata);
        f.state.input.insert_str("other session");
        f.voice.sync_owner(&mut f.state);
        settled(&mut f).await;
        f.voice.event(
            VoiceEvent {
                id,
                kind: EventKind::Transcript(Ok("stale text".into())),
            },
            &mut f.state,
        );
        assert_eq!(f.state.input.lines(), &["other session"]);
        assert!(f.state.voice.pending.is_none());
        assert_capture_exited(&f).await;
    }

    #[tokio::test]
    async fn voice_missing_recognizer_and_capture_permission_failure_preserve_draft() {
        for (capture, recognizer) in [
            (CAPTURE, None),
            (
                "printf 'ERROR Microphone permission denied\n'; exit 1",
                Some(STT),
            ),
        ] {
            let mut f = fixture(capture, recognizer);
            f.state.input.insert_str("untouched");
            submit(&mut f, "start");
            settled(&mut f).await;
            assert_eq!(f.state.input.lines(), &["untouched"]);
            assert!(
                f.state
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Error(_)))
            );
            assert!(!f.root.path().join("received.wav").exists());
            assert!(!f.root.path().join("capture.pid").exists());
        }
    }

    #[tokio::test]
    async fn voice_invalid_audio_and_transcript_bounds_fail_without_insertion() {
        for (audio, stt) in [
            (b"not WAV".to_vec(), STT),
            (wav_frames(16000 * 61), STT),
            (vec![0; MAX_AUDIO + 1], STT),
            (
                wav(),
                "cat >/dev/null; head -c 40000 /dev/zero | tr '\\000' x",
            ),
        ] {
            let mut f = fixture(CAPTURE, Some(stt));
            std::fs::write(f.root.path().join("input.wav"), audio).unwrap();
            submit(&mut f, "start");
            until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
            submit(&mut f, "stop");
            settled(&mut f).await;
            assert_eq!(f.state.input.lines(), &[""]);
            assert!(
                f.state
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Error(_)))
            );
            assert!(f.state.pending_send.is_none());
        }
    }
    #[tokio::test]
    async fn voice_cancel_during_transcription_kills_recognizer_and_preserves_draft() {
        let mut f = fixture(
            CAPTURE,
            Some("echo $$ > stt.pid; cat >/dev/null; touch recognizing; exec sleep 30"),
        );
        f.state.input.insert_str("keep this");
        submit(&mut f, "start");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        submit(&mut f, "stop");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Transcribing)).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.root.path().join("recognizing").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        submit(&mut f, "cancel");
        settled(&mut f).await;
        let pid = std::fs::read_to_string(f.root.path().join("stt.pid")).unwrap();
        let result = SubprocessService::local()
            .output(
                ProcessSpec::new("/bin/kill", f.root.path())
                    .unwrap()
                    .with_args(["-0", pid.trim()])
                    .unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.exit().is_success());
        assert_eq!(f.state.input.lines(), &["keep this"]);
        assert!(f.state.pending_send.is_none());
    }

    #[tokio::test]
    async fn voice_status_does_not_record_and_child_focus_cancels_capture() {
        let mut f = fixture(CAPTURE, Some(STT));
        submit(&mut f, "status");
        settled(&mut f).await;
        assert!(!f.root.path().join("capture.pid").exists());
        assert!(!f.root.path().join("received.wav").exists());
        submit(&mut f, "start");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        f.state.task_console.view = crate::task_console::ConsoleView::Detail;
        f.state.task_console.active = true;
        f.state.input.insert_str("child draft");
        f.voice.sync_owner(&mut f.state);
        settled(&mut f).await;
        assert_eq!(f.state.input.lines(), &["child draft"]);
        assert_capture_exited(&f).await;
    }

    #[tokio::test]
    #[ignore = "requires explicit local recognizer argv and generated test WAV; never records a microphone"]
    async fn voice_generated_audio_real_stt_enters_composer_without_submission() {
        let mut f = fixture(CAPTURE, Some(STT));
        let mut argv: Vec<String> =
            serde_json::from_str(&std::env::var("HEYCODE_VOICE_TEST_STT_COMMAND").unwrap())
                .unwrap();
        let config = SpeechCommandConfig::new(argv.remove(0).into(), argv).unwrap();
        let path = PathBuf::from(std::env::var("HEYCODE_VOICE_TEST_WAV").unwrap());
        assert!(path.is_absolute());
        std::fs::copy(path, f.root.path().join("input.wav")).unwrap();
        let backend = Arc::get_mut(f.voice.backend.as_mut().unwrap()).unwrap();
        backend.speech = LocalSpeech::new(
            Some(config),
            backend.subprocess.clone(),
            f.voice.closed.clone(),
        );
        f.state.input.insert_str("Draft:");
        submit(&mut f, "start");
        until(&mut f, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        submit(&mut f, "stop");
        settled(&mut f).await;
        let text = f.state.input.lines().join("\n");
        let transcript = text
            .strip_prefix("Draft: ")
            .expect("speech inserted into original composer");
        let words = |text: &str| {
            text.split_whitespace()
                .map(|word| {
                    word.trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase()
                })
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
        };
        let reference = words(
            "The quick brown fox jumps over the lazy dog. Please save the meeting notes in the local project folder.",
        );
        let recognized = words(transcript);
        let mut costs = (0..=recognized.len()).collect::<Vec<_>>();
        for (i, expected) in reference.iter().enumerate() {
            let mut previous = costs[0];
            costs[0] = i + 1;
            for (j, actual) in recognized.iter().enumerate() {
                let old = costs[j + 1];
                costs[j + 1] = (costs[j] + 1)
                    .min(old + 1)
                    .min(previous + usize::from(expected != actual));
                previous = old;
            }
        }
        println!(
            "voice composer transcript: {transcript}; word errors {}/{}",
            costs[recognized.len()],
            reference.len()
        );
        assert!(
            costs[recognized.len()] * 10 <= reference.len(),
            "recognizer smoke test exceeds 10% word error"
        );
        assert!(f.state.pending_send.is_none());
        assert!(!f.state.has_active_turn());
        assert_capture_exited(&f).await;
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "queries OS permission status under WorkspaceWrite; never requests access or records"]
    async fn voice_macos_status_uses_composed_workspace_write_executor() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let sandbox = heycode_exec::SandboxService::new(
            heycode_exec::SandboxMode::WorkspaceWrite,
            cwd.clone(),
            Some(heycode_sandbox::platform_default().unwrap()),
        )
        .unwrap();
        let mut context = heycode_core::compose(&[
            heycode_exec::sandbox_service_plugin(sandbox),
            heycode_exec::local_subprocess_plugin(),
        ])
        .unwrap();
        let subprocess = (*context
            .get::<SubprocessService>(heycode_exec::SERVICE_SUBPROCESS)
            .unwrap())
        .clone();
        let backend = VoiceBackend {
            capture: CaptureConfig {
                program: "/usr/bin/swift".into(),
                args: vec!["-e".into(), include_str!("voice_capture.swift").into()],
                mac_native: true,
                scratch: Arc::default(),
            },
            speech: LocalSpeech::new(None, subprocess.clone(), CancellationToken::new()),
            subprocess,
            cwd,
        };
        let status = backend.status(CancellationToken::new()).await.unwrap();
        println!("{status}");
        assert!(status.contains("microphone_permission="));
        let scratch = backend
            .capture
            .scratch
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .path()
            .to_path_buf();
        assert!(scratch.starts_with("/private/tmp"));
        drop(backend);
        assert!(
            !scratch.exists(),
            "private compiler scratch must leave with its owner"
        );
        context.shutdown();
    }
    #[tokio::test]
    async fn voice_new_frontend_after_resume_owns_its_new_session() {
        let mut old = fixture(CAPTURE, Some(STT));
        submit(&mut old, "start");
        until(&mut old, |f| f.state.voice.phase == Some(Phase::Recording)).await;
        let metadata = heycode_session::SessionCreationMetadata::new(
            Some(old.root.path().to_path_buf()),
            Some("native".into()),
            heycode_session::SessionSource::Interactive,
        )
        .unwrap();
        let other = old
            .sessions
            .create(&heycode_session::SessionCreateRequest::new(
                metadata.clone(),
            ))
            .unwrap();
        let other_id = other.id().clone();
        drop(other);
        old.state
            .handle_session_command(crate::session_browser::SessionCommandRequest::Resume(
                other_id,
            ));
        assert!(matches!(
            old.state.take_run_outcome(),
            Some(crate::app::TuiRunOutcome::RecomposeSession(_))
        ));
        // The production loop tears down before returning this outcome. A new
        // loop constructs a new owner from the newly composed Agent/session.
        old.voice.shutdown().await;
        assert_capture_exited(&old).await;
        let mut fresh = fixture(CAPTURE, Some(STT));
        assert_ne!(old.voice.owner, fresh.voice.owner);
        fresh.state.input.insert_str("resumed draft");
        submit(&mut fresh, "start");
        until(&mut fresh, |f| {
            f.state.voice.phase == Some(Phase::Recording)
        })
        .await;
        submit(&mut fresh, "stop");
        settled(&mut fresh).await;
        assert_eq!(fresh.state.input.lines(), &["resumed draft dictated words"]);
        assert!(fresh.state.pending_send.is_none());
    }
}
