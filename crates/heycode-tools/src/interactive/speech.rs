//! Optional local STT adapter. Explicit file input; never opens the microphone or inserts text.
use super::file_error;
use crate::builtins::{arg_str, resolve_path};
use crate::{Tool, ToolCtx, ToolError};
use heycode_exec::{
    FileSystemService, ProcessOutputChunk, ProcessSpec, ReadFileSpec, SubprocessService,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Exact trusted STT executable and arguments. Reads PCM WAV bytes from stdin to EOF and writes
/// only the UTF-8 transcript to stdout. Diagnostics belong on stderr. No shell is involved.
#[derive(Clone, Debug)]
pub struct SpeechCommandConfig {
    program: PathBuf,
    args: Vec<String>,
}
impl SpeechCommandConfig {
    /// Validate the trusted command. This does not prove an installed model or working recognition.
    ///
    /// # Errors
    /// Relative/empty program, more than 64 args, NULs or more than 16 KiB of arguments.
    pub fn new(program: PathBuf, args: Vec<String>) -> Result<Self, ToolError> {
        if !program.is_absolute()
            || args.len() > 64
            || args.iter().any(|s| s.contains('\0'))
            || args.iter().map(String::len).sum::<usize>() > 16384
        {
            return Err(ToolError::new(
                "STT command requires an absolute executable and bounded literal argv.",
            ));
        }
        Ok(Self { program, args })
    }
    /// Read a process-owned JSON argv array from HEYCODE_STT_COMMAND, without shell expansion.
    ///
    /// # Errors
    /// Invalid/non-UTF8 environment value or invalid command shape.
    pub fn from_environment() -> Result<Option<Self>, ToolError> {
        let Some(value) = std::env::var_os("HEYCODE_STT_COMMAND") else {
            return Ok(None);
        };
        let mut argv: Vec<String> = value
            .to_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| {
                ToolError::new("HEYCODE_STT_COMMAND must be a JSON array of literal strings.")
            })?;
        if argv.is_empty() {
            return Err(ToolError::new(
                "HEYCODE_STT_COMMAND requires an absolute executable.",
            ));
        }
        let program = PathBuf::from(argv.remove(0));
        Self::new(program, argv).map(Some)
    }
}

pub(super) struct Speech {
    pub(super) config: Option<SpeechCommandConfig>,
    pub(super) filesystem: Arc<FileSystemService>,
    pub(super) subprocess: Arc<SubprocessService>,
    pub(super) closed: CancellationToken,
}
#[async_trait::async_trait]
impl Tool for Speech {
    fn prerequisite_status(&self) -> crate::ToolPrerequisiteStatus {
        crate::ToolPrerequisiteStatus {
            configured: Some(self.config.as_ref().is_some_and(|config| config.program.is_file())),
            detail: "Status remains available. Transcription requires HEYCODE_STT_COMMAND; installed model and recognition are verified when transcribing.".into(),
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
                closed: self.closed.child_token(),
            }),
            None => super::workspace_unavailable(self),
        })
    }
    fn spec(&self) -> heycode_core::ToolSpec {
        heycode_core::ToolSpec{name:"transcribe_audio".into(),description:"Transcribe an explicitly selected local PCM WAV with an optional installed STT command. status reports setup. transcribe(path) validates audio and returns bounded plain text; it does not record a microphone, insert text, use provider audio, or require model audio support.".into(),parameters:json!({"type":"object","properties":{"action":{"enum":["status","transcribe"]},"path":{"type":"string"}},"required":["action"],"additionalProperties":false})}
    }
    async fn run(&self, args: Value, cx: &ToolCtx) -> Result<Value, ToolError> {
        const SETUP: &str = "Set HEYCODE_STT_COMMAND to a JSON argv array for an installed local speech recognizer or wrapper. It must read PCM WAV stdin and emit only UTF-8 transcript stdout. Install its model separately, then restart heycode. This file tool does not record or insert text; use /voice in the human TUI for dictation.";
        match arg_str(&args, "action")? {
            "status" => {
                return Ok(
                    json!({"installation_configured":self.config.as_ref().is_some_and(|c|c.program.is_file()),"recognition_tested":false,"microphone":false,"text_insertion":false,"setup":SETUP}),
                );
            }
            "transcribe" => {}
            _ => return Err(ToolError::new("action must be status or transcribe.")),
        }
        let config = self.config.as_ref().ok_or_else(|| ToolError::new(SETUP))?;
        let path = resolve_path(&self.filesystem, cx, arg_str(&args, "path")?)?;
        let read = self
            .filesystem
            .read(
                ReadFileSpec::new_binary(path, 32 * 1024 * 1024).map_err(file_error)?,
                cx.cancellation.clone(),
            )
            .await
            .map_err(file_error)?;
        if read.truncated() {
            return Err(ToolError::new("Audio exceeds 32 MiB."));
        }
        let cwd = resolve_path(&self.filesystem, cx, ".")?;
        let transcript = LocalSpeech::new(
            Some(config.clone()),
            (*self.subprocess).clone(),
            self.closed.clone(),
        )
        .transcribe_pcm_wav(read.bytes(), cwd.as_path(), None, cx.cancellation.clone())
        .await?;
        Ok(
            json!({"text":transcript.text,"sample_rate_hz":transcript.audio.sample_rate_hz(),"source":"local_stt_command","microphone_recorded":false,"inserted":false}),
        )
    }
}

/// The shared bounded local speech transport, usable by human capture and file tools.
/// It never opens a microphone and never inserts or submits text.
#[derive(Clone)]
pub struct LocalSpeech {
    config: Option<SpeechCommandConfig>,
    subprocess: SubprocessService,
    closed: CancellationToken,
}
/// A successfully settled recognizer response and validated input metadata.
pub struct SpeechTranscript {
    /// Plain UTF-8 recognizer output, limited to 32 KiB.
    pub text: String,
    /// Validated PCM WAV metadata.
    pub audio: heycode_core::AttachmentAudioMetadata,
}
impl LocalSpeech {
    /// Bind exact trusted argv to the supplied subprocess authority and lifecycle.
    #[must_use]
    pub fn new(
        config: Option<SpeechCommandConfig>,
        subprocess: SubprocessService,
        closed: CancellationToken,
    ) -> Self {
        Self {
            config,
            subprocess,
            closed,
        }
    }
    /// Installation presence, not proof of a usable acoustic model.
    #[must_use]
    pub fn configured(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| config.program.is_file())
    }
    /// Actionable setup description without launching a recognizer.
    #[must_use]
    pub fn status(&self) -> &'static str {
        if self.configured() {
            "Local recognizer command configured; installed model and recognition are checked when transcribing"
        } else {
            "Local recognizer missing. Set HEYCODE_STT_COMMAND to a JSON literal argv array with an absolute installed executable. It must read PCM WAV stdin and emit only UTF-8 transcript stdout; install its model separately, then restart heycode"
        }
    }
    /// Validate PCM input and run the configured local recognizer through the
    /// exact supplied subprocess service. Capture callers may impose a shorter
    /// audio-duration ceiling. No filesystem reads or model API are involved.
    ///
    /// # Errors
    /// Missing configuration, invalid/oversized/overlong WAV, cancellation,
    /// timeout, invalid/bounded output, failed exit or unconfirmed cleanup.
    pub async fn transcribe_pcm_wav(
        &self,
        wav: &[u8],
        cwd: &std::path::Path,
        maximum_duration: Option<Duration>,
        cancellation: CancellationToken,
    ) -> Result<SpeechTranscript, ToolError> {
        if self.closed.is_cancelled() || cancellation.is_cancelled() {
            return Err(ToolError::new("Transcription cancelled before admission."));
        }
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| ToolError::new(self.status()))?;
        if wav.len() > 32 * 1024 * 1024 {
            return Err(ToolError::new("Audio exceeds 32 MiB."));
        }
        let metadata = heycode_attachments::validate_pcm_wav(wav)
            .map_err(|_| ToolError::new("Select a valid PCM WAV recording."))?;
        if maximum_duration
            .is_some_and(|limit| u128::from(metadata.duration_ms()) > limit.as_millis())
        {
            return Err(ToolError::new(
                "Audio exceeds the recording duration limit.",
            ));
        }
        let spec = ProcessSpec::new(&config.program, cwd)
            .and_then(|s| s.with_args(&config.args))
            .and_then(|s| s.with_timeout(Some(Duration::from_secs(120))))
            .map_err(|_| ToolError::new("Invalid STT command configuration."))?
            .with_interactive_stdio();
        let operation = cancellation.child_token();
        let _guard = operation.clone().drop_guard();
        let raw = self
            .subprocess
            .spawn_interactive_raw(spec, operation.clone())
            .await
            .map_err(|_| {
                ToolError::new(
                    "STT command failed to start; check installation and sandbox policy.",
                )
            })?;
        let (process, mut input, mut output) = raw.into_raw_parts();
        let work = async {
            input
                .write(wav)
                .await
                .map_err(|_| ToolError::new("STT input failed."))?;
            input
                .finish()
                .await
                .map_err(|_| ToolError::new("STT input closed unexpectedly."))?;
            let mut transcript = Vec::new();
            loop {
                match output
                    .read_chunk(cancellation.clone())
                    .await
                    .map_err(|_| ToolError::new("STT output failed."))?
                {
                    ProcessOutputChunk::Eof => break,
                    ProcessOutputChunk::Data(bytes) => {
                        if transcript.len() + bytes.len() > 32768 {
                            return Err(ToolError::new("STT transcript exceeds 32 KiB."));
                        }
                        transcript.extend(bytes);
                    }
                }
            }
            let text = String::from_utf8(transcript)
                .map_err(|_| ToolError::new("STT command must return UTF-8."))?;
            if text.trim().is_empty() {
                return Err(ToolError::new("STT command returned no transcript."));
            }
            Ok(text)
        };
        let result = tokio::select! {biased;()=self.closed.cancelled()=>Err(ToolError::new("Speech adapter stopped.")),()=cancellation.cancelled()=>Err(ToolError::new("Transcription cancelled.")),result=tokio::time::timeout(Duration::from_secs(120),work)=>result.unwrap_or_else(|_|Err(ToolError::new("Transcription timed out.")))};
        drop(output);
        match result {
            Err(error) => {
                process
                    .kill()
                    .await
                    .map_err(|_| ToolError::new("STT process cleanup failed."))?;
                Err(error)
            }
            Ok(text) => {
                let exit = super::settle_helper(
                    process,
                    operation,
                    self.closed.clone(),
                    cancellation.clone(),
                )
                .await?;
                if !exit.is_success() {
                    return Err(ToolError::new("STT command exited unsuccessfully."));
                }
                Ok(SpeechTranscript {
                    text,
                    audio: metadata,
                })
            }
        }
    }
}
