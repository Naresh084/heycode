//! What `heycode run` prints, and how a script reads it.
//!
//! Three formats, one contract: the reply goes to stdout, everything a human
//! wants but a pipeline does not (tool activity, the session id, token counts)
//! goes to stderr, and the exit code says how the turn ended. `json` folds the
//! whole turn into one envelope; `stream-json` writes the durable session
//! events — the JSONL v2 lines, unchanged — one object per line as they
//! commit, so `heycode run --output-format stream-json | jq` sees exactly what a
//! later `session.jsonl` reader sees.

use std::io::Write;
use std::sync::{Arc, Mutex};

use heycode_agent::{TurnReport, UiEvent};
use heycode_core::SessionId;
use serde::Serialize;

/// How a headless run reports the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Reply on stdout as it streams; tool activity and the session trailer
    /// on stderr.
    #[default]
    Text,
    /// One JSON envelope on stdout after the turn settles.
    Json,
    /// One durable session event per line on stdout as each commits.
    StreamJson,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "stream-json" => Ok(Self::StreamJson),
            other => Err(format!(
                "--output-format expects text, json or stream-json, got `{other}`"
            )),
        }
    }
}

/// Process exit status for one headless turn.
#[must_use]
pub fn exit_code(report: &TurnReport) -> i32 {
    match report.reason {
        "error" => 1,
        // The conventional 128 + SIGINT, so a shell script's `$?` reads a
        // cancelled turn the same way it reads a Ctrl+C.
        "aborted" | "cancelled" => 130,
        _ => 0,
    }
}

/// One tool call as the `json` envelope reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCallRecord {
    /// Tool name.
    pub name: String,
    /// Full argument object.
    pub args: serde_json::Value,
    /// `None` while the call has not settled.
    pub ok: Option<bool>,
    /// Full result value once settled.
    pub result: Option<serde_json::Value>,
}

/// The `json` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnEnvelope {
    /// Durable session the turn was appended to.
    pub session_id: String,
    /// Final assistant text.
    pub reply: String,
    /// Every tool call, in start order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Provider usage when reported.
    pub usage: Option<heycode_core::TokenUsage>,
    /// `stop | max_tokens | error | aborted`.
    pub reason: String,
    /// Errors surfaced during the turn, in order.
    pub errors: Vec<String>,
}

#[derive(Default)]
struct Collected {
    reply: String,
    tool_calls: Vec<ToolCallRecord>,
    errors: Vec<String>,
    turns_finished: usize,
    usage: heycode_core::TokenUsage,
    usage_missing: bool,
    failure_reason: Option<&'static str>,
}

/// Sink pair a headless run writes to; `std::io` in production, buffers in
/// tests.
pub struct Sinks {
    /// Reply / envelope / event stream.
    pub out: Box<dyn Write + Send>,
    /// Tool activity, errors and the session trailer.
    pub err: Box<dyn Write + Send>,
}

impl Sinks {
    /// The process's stdout and stderr.
    #[must_use]
    pub fn process() -> Self {
        Self {
            out: Box::new(std::io::stdout()),
            err: Box::new(std::io::stderr()),
        }
    }
}

/// Headless printer: subscribe it to the agent's UI bus and the session bus
/// before the turn, then [`HeadlessOutput::finish`] after it.
pub struct HeadlessOutput {
    format: OutputFormat,
    sinks: Arc<Mutex<Sinks>>,
    collected: Arc<Mutex<Collected>>,
}

impl HeadlessOutput {
    /// Printer for `format` over `sinks`.
    #[must_use]
    pub fn new(format: OutputFormat, sinks: Sinks) -> Self {
        Self {
            format,
            sinks: Arc::new(Mutex::new(sinks)),
            collected: Arc::new(Mutex::new(Collected::default())),
        }
    }

    /// Selected format.
    #[must_use]
    pub const fn format(&self) -> OutputFormat {
        self.format
    }

    /// Listener for the agent's [`UiEvent`] bus.
    pub fn ui_listener(&self) -> impl Fn(&UiEvent) + Send + Sync + 'static {
        let format = self.format;
        let sinks = self.sinks.clone();
        let collected = self.collected.clone();
        move |event| {
            let Ok(mut sinks) = sinks.lock() else {
                return;
            };
            let Ok(mut collected) = collected.lock() else {
                return;
            };
            match event {
                UiEvent::TurnStarted { .. }
                    if collected.turns_finished > 0
                        && !collected.reply.is_empty()
                        && !collected.reply.ends_with('\n') =>
                {
                    collected.reply.push('\n');
                    if format == OutputFormat::Text {
                        let _ = writeln!(sinks.out);
                    }
                }
                UiEvent::TurnFinished { reason, usage, .. } => {
                    collected.turns_finished += 1;
                    if let Some(usage) = usage {
                        collected.usage.prompt_tokens = collected
                            .usage
                            .prompt_tokens
                            .saturating_add(usage.prompt_tokens);
                        collected.usage.completion_tokens = collected
                            .usage
                            .completion_tokens
                            .saturating_add(usage.completion_tokens);
                    } else {
                        collected.usage_missing = true;
                    }
                    match reason.as_str() {
                        "aborted" | "cancelled" => collected.failure_reason = Some("aborted"),
                        "error" if collected.failure_reason != Some("aborted") => {
                            collected.failure_reason = Some("error")
                        }
                        _ => {}
                    }
                }
                UiEvent::SettingsShellRequested { tab, snapshot } => {
                    let text = snapshot.plain_text_for(*tab);
                    collected.reply.push_str(&text);
                    if format == OutputFormat::Text {
                        let _ = writeln!(sinks.out, "{text}");
                        let _ = sinks.out.flush();
                    }
                }
                UiEvent::AssistantDelta { text } => {
                    collected.reply.push_str(text);
                    if format == OutputFormat::Text {
                        let _ = write!(sinks.out, "{text}");
                        let _ = sinks.out.flush();
                    }
                }
                UiEvent::ToolStarted { name, args } => {
                    collected.tool_calls.push(ToolCallRecord {
                        name: name.clone(),
                        args: args.clone(),
                        ok: None,
                        result: None,
                    });
                    if format == OutputFormat::Text {
                        let _ = writeln!(sinks.err, "⚙ {name} {}", args_summary(args));
                    }
                }
                UiEvent::ToolFinished {
                    name, ok, value, ..
                } => {
                    if let Some(call) = collected
                        .tool_calls
                        .iter_mut()
                        .rev()
                        .find(|call| call.name == *name && call.ok.is_none())
                    {
                        call.ok = Some(*ok);
                        call.result = Some(value.clone());
                    }
                    if format == OutputFormat::Text {
                        let mark = if *ok { "✓" } else { "✗" };
                        let _ = writeln!(sinks.err, "{mark} {name} {}", result_summary(value));
                    }
                }
                UiEvent::Error { message } => {
                    collected.errors.push(message.clone());
                    if format != OutputFormat::Json {
                        let _ = writeln!(sinks.err, "[error] {message}");
                    }
                }
                _ => {}
            }
        }
    }

    /// Listener for the durable session bus; only `stream-json` prints here.
    pub fn session_listener(
        &self,
    ) -> impl Fn(&heycode_session::SessionEvent) + Send + Sync + 'static {
        let format = self.format;
        let sinks = self.sinks.clone();
        move |event| {
            if format != OutputFormat::StreamJson {
                return;
            }
            let Ok(mut sinks) = sinks.lock() else {
                return;
            };
            if let Ok(line) = serde_json::to_string(event) {
                let _ = writeln!(sinks.out, "{line}");
                let _ = sinks.out.flush();
            }
        }
    }

    /// Close out the turn: the envelope for `json`, the trailer for the
    /// others. Returns the process exit code.
    ///
    /// # Errors
    /// Sink write failures.
    pub fn finish(&self, session_id: &SessionId, report: &TurnReport) -> anyhow::Result<i32> {
        let mut sinks = self
            .sinks
            .lock()
            .map_err(|_| anyhow::anyhow!("headless output sinks are poisoned"))?;
        let collected = self
            .collected
            .lock()
            .map_err(|_| anyhow::anyhow!("headless output state is poisoned"))?;
        let mut aggregate;
        let report = if collected.turns_finished > 1 {
            aggregate = report.clone();
            aggregate.text = collected.reply.clone();
            aggregate.usage = (!collected.usage_missing).then_some(collected.usage);
            aggregate.reason = collected.failure_reason.unwrap_or(report.reason);
            &aggregate
        } else {
            report
        };
        let usage = report
            .usage
            .map(|usage| {
                format!(
                    " · tokens {}↑ {}↓",
                    usage.prompt_tokens, usage.completion_tokens
                )
            })
            .unwrap_or_default();
        match self.format {
            OutputFormat::Text => {
                if !report.text.is_empty() && !report.text.ends_with('\n') {
                    writeln!(sinks.out)?;
                }
                sinks.out.flush()?;
                writeln!(
                    sinks.err,
                    "session {} · {}{usage}",
                    session_id.as_str(),
                    report.reason
                )?;
            }
            OutputFormat::Json => {
                let envelope = TurnEnvelope {
                    session_id: session_id.as_str().to_owned(),
                    reply: if report.text.is_empty() {
                        collected.reply.clone()
                    } else {
                        report.text.clone()
                    },
                    tool_calls: collected.tool_calls.clone(),
                    usage: report.usage,
                    reason: report.reason.to_owned(),
                    errors: collected.errors.clone(),
                };
                writeln!(sinks.out, "{}", serde_json::to_string(&envelope)?)?;
                sinks.out.flush()?;
            }
            OutputFormat::StreamJson => {
                sinks.out.flush()?;
                writeln!(
                    sinks.err,
                    "session {} · {}{usage}",
                    session_id.as_str(),
                    report.reason
                )?;
            }
        }
        sinks.err.flush()?;
        Ok(exit_code(report))
    }
}

/// What a non-terminal stdin contributed to a headless prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinAppendix {
    /// Stdin is a terminal or was empty: nothing to add.
    None,
    /// Piped text to append after the prompt.
    Text(String),
    /// Stdin is open but produced nothing within the wait; the prompt is
    /// used as typed and the caller should say so on stderr.
    TimedOut(std::time::Duration),
}

impl StdinAppendix {
    /// The appended text, if any.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::None | Self::TimedOut(_) => None,
        }
    }
}

/// Text appended to a headless prompt from a non-terminal stdin.
///
/// `echo SPEC | heycode run "summarise"` means "summarise this", so the piped
/// text is appended after a blank line. A parent that leaves stdin open
/// without writing (a supervisor, a forgotten redirection) must not hang the
/// run: the read is abandoned after `wait` and the prompt is used as typed.
#[must_use]
pub fn stdin_appendix(wait: std::time::Duration) -> StdinAppendix {
    use std::io::{IsTerminal as _, Read as _};
    if std::io::stdin().is_terminal() {
        return StdinAppendix::None;
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::stdin().lock().read_to_string(&mut text);
        let _ = sender.send(text);
    });
    match receiver.recv_timeout(wait) {
        Ok(text) if text.trim().is_empty() => StdinAppendix::None,
        Ok(text) => StdinAppendix::Text(text),
        Err(_) => StdinAppendix::TimedOut(wait),
    }
}

/// Join a prompt and an optional stdin appendix with a blank line between.
#[must_use]
pub fn prompt_with_appendix(prompt: &str, appendix: Option<&str>) -> String {
    match appendix {
        Some(text) => format!("{}\n\n{}", prompt.trim_end(), text.trim_end()),
        None => prompt.to_owned(),
    }
}

/// One line for a tool's arguments: the command or path when there is one,
/// otherwise the compact JSON, clipped.
fn args_summary(args: &serde_json::Value) -> String {
    let text = ["command", "path", "file_path", "pattern", "query", "url"]
        .into_iter()
        .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
        .map_or_else(|| args.to_string(), str::to_owned);
    clip(&text.replace('\n', "⏎"), 120)
}

fn result_summary(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let lines = text.lines().count();
    let first = text.lines().next().unwrap_or_default();
    if lines > 1 {
        format!("({lines} lines) {}", clip(first, 80))
    } else {
        clip(first, 100)
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn output(format: OutputFormat) -> (HeadlessOutput, Buffer, Buffer) {
        let out = Buffer::default();
        let err = Buffer::default();
        let printer = HeadlessOutput::new(
            format,
            Sinks {
                out: Box::new(out.clone()),
                err: Box::new(err.clone()),
            },
        );
        (printer, out, err)
    }

    #[test]
    fn background_turns_contribute_reply_usage_and_failure_to_headless_result() {
        let (printer, out, _) = output(OutputFormat::Json);
        let listen = printer.ui_listener();
        for (turn, text, reason) in [
            (1, "Delegated the work.", "stop"),
            (2, "Used the findings.", "error"),
        ] {
            listen(&UiEvent::TurnStarted { turn });
            listen(&UiEvent::AssistantDelta {
                text: text.to_owned(),
            });
            listen(&UiEvent::TurnFinished {
                reason: reason.into(),
                usage: Some(heycode_core::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                }),
                context_tokens: None,
            });
        }
        assert_eq!(
            printer
                .finish(
                    &SessionId::from_raw("headless"),
                    &TurnReport {
                        text: "Delegated the work.".into(),
                        usage: Some(heycode_core::TokenUsage {
                            prompt_tokens: 10,
                            completion_tokens: 2
                        }),
                        reason: "stop"
                    }
                )
                .unwrap(),
            1
        );
        let envelope: serde_json::Value = serde_json::from_str(&out.text()).unwrap();
        assert_eq!(envelope["reply"], "Delegated the work.\nUsed the findings.");
        assert_eq!(envelope["usage"]["prompt_tokens"], 20);
        assert_eq!(envelope["reason"], "error");
    }

    #[test]
    fn settings_shell_fallback_preserves_unavailable_state_in_text_and_json() {
        use heycode_agent::ui::{SettingsShellSection, SettingsShellSnapshot, SettingsShellTab};
        let snapshot = SettingsShellSnapshot {
            status: SettingsShellSection::Ready {
                text: "ready".to_owned(),
            },
            usage: SettingsShellSection::Empty {
                message: "No session usage yet".to_owned(),
            },
            stats: SettingsShellSection::Unavailable {
                reason: "History aggregation unavailable".to_owned(),
            },
            stats_snapshot: None,
        };
        let expected = snapshot.plain_text_for(SettingsShellTab::Stats);
        for format in [OutputFormat::Text, OutputFormat::Json] {
            let (printer, out, _) = output(format);
            printer.ui_listener()(&UiEvent::SettingsShellRequested {
                tab: SettingsShellTab::Stats,
                snapshot: snapshot.clone(),
            });
            assert_eq!(printer.collected.lock().unwrap().reply, expected);
            if format == OutputFormat::Text {
                assert_eq!(out.text(), format!("{expected}\n"));
            } else {
                assert_eq!(out.text(), "");
                let report = TurnReport {
                    text: String::new(),
                    usage: None,
                    reason: "stop",
                };
                printer
                    .finish(&SessionId::from_raw("settings-test"), &report)
                    .unwrap();
                let envelope: serde_json::Value = serde_json::from_str(&out.text()).unwrap();
                assert_eq!(envelope["reply"], expected);
            }
        }
    }

    fn drive(printer: &HeadlessOutput) {
        let listen = printer.ui_listener();
        listen(&UiEvent::ToolStarted {
            name: "bash".to_owned(),
            args: serde_json::json!({"command": "ls -la\nfoo"}),
        });
        listen(&UiEvent::ToolFinished {
            name: "bash".to_owned(),
            ok: true,
            value: serde_json::json!("a\nb\nc"),
            untrusted_content: None,
        });
        listen(&UiEvent::AssistantDelta {
            text: "Hello ".to_owned(),
        });
        listen(&UiEvent::AssistantDelta {
            text: "world".to_owned(),
        });
        listen(&UiEvent::Error {
            message: "minor".to_owned(),
        });
    }

    fn report() -> TurnReport {
        TurnReport {
            text: "Hello world".to_owned(),
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 3,
            }),
            reason: "stop",
        }
    }

    #[test]
    fn text_streams_the_reply_to_stdout_and_everything_else_to_stderr() {
        let (printer, out, err) = output(OutputFormat::Text);
        drive(&printer);
        let code = printer
            .finish(&SessionId::from_raw("abc123"), &report())
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            out.text(),
            "Hello world\n",
            "stdout is the reply and nothing else"
        );
        let err = err.text();
        assert!(err.contains("⚙ bash ls -la⏎foo\n"), "{err}");
        assert!(err.contains("✓ bash (3 lines) a\n"), "{err}");
        assert!(err.contains("[error] minor\n"), "{err}");
        assert!(
            err.ends_with("session abc123 · stop · tokens 12↑ 3↓\n"),
            "{err}"
        );
    }

    #[test]
    fn json_is_one_envelope_on_stdout_and_a_silent_stderr() {
        let (printer, out, err) = output(OutputFormat::Json);
        drive(&printer);
        let code = printer
            .finish(&SessionId::from_raw("abc123"), &report())
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(err.text(), "", "a json consumer parses stdout only");
        let text = out.text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        let envelope: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(envelope["session_id"], "abc123");
        assert_eq!(envelope["reply"], "Hello world");
        assert_eq!(envelope["reason"], "stop");
        assert_eq!(envelope["usage"]["prompt_tokens"], 12);
        assert_eq!(envelope["tool_calls"][0]["name"], "bash");
        assert_eq!(envelope["tool_calls"][0]["ok"], true);
        assert_eq!(envelope["tool_calls"][0]["result"], "a\nb\nc");
        assert_eq!(envelope["errors"], serde_json::json!(["minor"]));
    }

    #[test]
    fn stream_json_writes_each_session_event_unchanged_and_nothing_from_the_ui_bus() {
        let (printer, out, err) = output(OutputFormat::StreamJson);
        drive(&printer);
        assert_eq!(out.text(), "", "UI deltas are not session events");
        let session = printer.session_listener();
        let event = heycode_session::SessionEvent {
            v: 2,
            seq: 0,
            time_ms: 1_730_000_000_000,
            kind: heycode_session::SessionEventKind::UserMessage {
                text: "hi".to_owned(),
            },
        };
        session(&event);
        let expected = serde_json::to_string(&event).unwrap();
        assert_eq!(out.text(), format!("{expected}\n"));
        let code = printer
            .finish(&SessionId::from_raw("abc123"), &report())
            .unwrap();
        assert_eq!(code, 0);
        assert!(
            err.text().contains("session abc123 · stop"),
            "{}",
            err.text()
        );
    }

    #[test]
    fn exit_codes_follow_the_turn_reason() {
        let mut report = report();
        assert_eq!(exit_code(&report), 0);
        report.reason = "max_tokens";
        assert_eq!(exit_code(&report), 0, "a truncated reply is still a reply");
        report.reason = "error";
        assert_eq!(exit_code(&report), 1);
        report.reason = "aborted";
        assert_eq!(exit_code(&report), 130);
    }

    #[test]
    fn output_format_parses_the_three_names_and_names_the_bad_one() {
        assert_eq!("text".parse::<OutputFormat>().unwrap(), OutputFormat::Text);
        assert_eq!("json".parse::<OutputFormat>().unwrap(), OutputFormat::Json);
        assert_eq!(
            "stream-json".parse::<OutputFormat>().unwrap(),
            OutputFormat::StreamJson
        );
        let error = "yaml".parse::<OutputFormat>().unwrap_err();
        assert!(error.contains("`yaml`"), "{error}");
    }

    #[test]
    fn a_piped_document_is_appended_after_a_blank_line() {
        assert_eq!(
            prompt_with_appendix("summarise\n", Some("SPEC line 1\nSPEC line 2\n")),
            "summarise\n\nSPEC line 1\nSPEC line 2"
        );
        assert_eq!(prompt_with_appendix("summarise", None), "summarise");
    }

    #[test]
    fn a_long_argument_is_clipped_with_an_ellipsis() {
        let summary = args_summary(&serde_json::json!({"command": "x".repeat(300)}));
        assert_eq!(summary.chars().count(), 120);
        assert!(summary.ends_with('…'));
        assert_eq!(
            args_summary(&serde_json::json!({"n": 1})),
            "{\"n\":1}",
            "no known key: compact JSON"
        );
    }
}
